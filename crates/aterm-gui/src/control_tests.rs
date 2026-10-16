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

/// When two live instances answer to one id (one from a build without the adoption
/// claim), `@<sid>` is an error here, never a keystroke into whichever window this
/// process holds; the refusal names the pid to address instead.
#[test]
fn an_id_a_second_live_instance_also_serves_is_refused_not_delivered() {
    use super::{Selector, ambiguous_sid_refusal};
    use aterm_session::{LaunchNonce, SessionId};

    let _guard = crate::identity_claim::rendezvous_test_guard();
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    crate::identity_claim::set_rendezvous_override(Some(dir.path().to_path_buf()));

    let sid = SessionId::generate();
    let sel = Selector::parse(sid.as_str());

    // An id this process minted cannot be held elsewhere, so it is never ambiguous,
    // even over a live foreign entry (only an 80-bit collision could produce one).
    std::fs::create_dir_all(dir.path().join("graph")).expect("graph dir");
    std::fs::write(
        dir.path().join("graph").join(sid.as_str()),
        live_foreign_entry(),
    )
    .expect("plant a foreign entry");
    assert!(
        ambiguous_sid_refusal(&sel).is_none(),
        "a minted id is not contestable, and must not pay for a probe"
    );

    // From here the id is one this process ADOPTED — the premint case, the
    // only population that can be doubly held.
    crate::identity_claim::note_adopted(&sid);
    std::fs::remove_file(dir.path().join("graph").join(sid.as_str())).expect("unplant");

    // No discovery entry: nobody else claims the id, so nothing is refused.
    assert!(ambiguous_sid_refusal(&sel).is_none());

    // Our own entry, the ordinary case for every hosted session; reading it as a
    // rival would refuse every cross-session address.
    crate::proxy::write_graph_entry(
        dir.path(),
        &sid,
        "/nonexistent/aterm.sock",
        &LaunchNonce::generate(),
    );
    assert!(
        ambiguous_sid_refusal(&sel).is_none(),
        "an instance must serve the ids IT published"
    );

    // A live foreign instance also serving it (`LIVE_FOREIGN_PID`), the duplicate
    // case.
    std::fs::write(
        dir.path().join("graph").join(sid.as_str()),
        live_foreign_entry(),
    )
    .expect("plant a foreign entry");
    let refusal = ambiguous_sid_refusal(&sel).expect("an ambiguous id must be refused");
    assert!(
        refusal.starts_with("ERR ambiguous session id "),
        "{refusal:?}"
    );
    assert!(refusal.contains(sid.as_str()), "{refusal:?}");
    assert!(
        refusal.contains(&format!("pid {LIVE_FOREIGN_PID}")),
        "{refusal:?}"
    );
    assert!(
        refusal.ends_with('\n'),
        "every reply is one line: {refusal:?}"
    );

    // Process-local forms never leave this process, so they cannot be ambiguous
    // and pay no probe.
    assert!(ambiguous_sid_refusal(&Selector::parse("7")).is_none());
    assert!(ambiguous_sid_refusal(&Selector::parse(".")).is_none());

    crate::identity_claim::set_rendezvous_override(None);
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
    let model = aterm_spec::derive::handoff_address_owner_model();
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
    use super::control_input::control_attempt_closes_cursor_license;
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

    // Parsing happens after this verb-level boundary, so malformed front and cross
    // forms cancel like valid ones. Read-only and unauthorized requests never reach
    // this post-authorization predicate.
    for verb in [
        "send", "key", "ctrl", "feed", "signal", "mouse", "paste", "focus", "turn", "resize",
        "scroll",
    ] {
        assert!(
            control_attempt_closes_cursor_license(verb),
            "authorized {verb} attempt must fence before argument parsing"
        );
    }
    for verb in ["text", "screen", "cursor", "image", "metrics"] {
        assert!(!control_attempt_closes_cursor_license(verb));
    }
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
    let model = aterm_spec::derive::artifact_handoff_capacity_model();
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

    let mut grown = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        CHARGE,
    )
    .expect("reverse-order first permit fits");
    assert!(grown.try_reconcile_descriptor_units(CHARGE + 1));
    let ordinary = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        CHARGE,
    )
    .expect("reverse-order second permit fits");
    validate("SelectThree", (2, 5, [0, 1, 1], 2), (2, 5, [0, 1, 1], 3));
    drop(grown);
    validate("Release", (2, 5, [0, 1, 1], 3), (1, 2, [0, 1, 0], 3));
    validate("SelectTwo", (1, 2, [0, 1, 0], 3), (1, 2, [0, 1, 0], 2));
    drop(ordinary);
    validate("Release", (1, 2, [0, 1, 0], 2), (0, 0, [0, 0, 0], 2));
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

    let mut saturated = (0..descriptor_slots)
        .map(|_| {
            ArtifactHandoffPermit::try_acquire_from(
                &USAGE,
                ARTIFACT_HANDOFF_LIMIT,
                DESCRIPTOR_LIMIT,
                CHARGE,
            )
            .expect("representative charge fits")
        })
        .collect::<Vec<_>>();
    assert!(
        !saturated[0].try_reconcile_descriptor_units(CHARGE + 1),
        "exact pinned depth cannot grow past aggregate capacity"
    );
    validate(
        "RefuseReconcile",
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
    while let Some(permit) = saturated.pop() {
        drop(permit);
    }
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
    assert_eq!(*USAGE.lock().unwrap(), ArtifactHandoffUsage::default());
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

/// The reaper owes cleanup a later capture waits on, so it never takes the
/// starvable class. Only a starved host would otherwise notice a revert.
#[test]
fn quarantine_reaper_is_never_housekeeping() {
    assert_ne!(
        super::ARTIFACT_QUARANTINE_REAPER_ROLE,
        crate::qos::Role::Housekeeping
    );
}

/// The sweep's lower bound at exact instants, independent of any thread's
/// scheduling: a guard one tick inside its quarantine stays queued, one
/// whose deadline has come is taken, and a taken guard lives until the
/// caller drops it (outside the queue mutex).
#[test]
fn the_quarantine_sweep_never_releases_a_guard_before_its_deadline() {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let alive = Arc::new(AtomicBool::new(true));
    let mut entries = vec![QuarantinedArtifactReply {
        until,
        _retention: ReplyRetention::new(WireProbe {
            alive: Arc::clone(&alive),
            prepared: Arc::new(AtomicBool::new(false)),
            fail_prepare: false,
        }),
        ticket: 0,
    }];
    let early = ArtifactReplyQuarantine::take_expired(
        &mut entries,
        until - std::time::Duration::from_nanos(1),
    );
    assert!(
        early.is_empty() && entries.len() == 1,
        "a guard inside its quarantine stays queued"
    );
    drop(early);
    assert!(alive.load(Ordering::Acquire));
    let due = ArtifactReplyQuarantine::take_expired(&mut entries, until);
    assert!(
        entries.is_empty() && due.len() == 1,
        "a guard whose deadline has come is taken"
    );
    assert!(
        alive.load(Ordering::Acquire),
        "taken, not yet released: the caller drops it"
    );
    drop(due);
    assert!(!alive.load(Ordering::Acquire));
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

struct GuardCheckingWriter {
    alive: Arc<AtomicBool>,
    prepared: Arc<AtomicBool>,
    expect_alive: bool,
    bytes: Vec<u8>,
    flushed: bool,
}

impl Write for GuardCheckingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        assert_eq!(
            self.alive.load(Ordering::Acquire),
            self.expect_alive,
            "artifact guard lifetime at socket write"
        );
        assert!(
            self.prepared.load(Ordering::Acquire),
            "wire identity validation must precede write"
        );
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        assert_eq!(
            self.alive.load(Ordering::Acquire),
            self.expect_alive,
            "artifact guard lifetime at socket flush"
        );
        self.flushed = true;
        Ok(())
    }
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
fn artifact_guard_spans_actual_reply_write_and_flush() {
    let alive = Arc::new(AtomicBool::new(true));
    let prepared = Arc::new(AtomicBool::new(false));
    let reply = ControlReply::guarded(
        "OK 1 1 /private/artifact.png\n".to_string(),
        Some(ReplyRetention::new(WireProbe {
            alive: Arc::clone(&alive),
            prepared: Arc::clone(&prepared),
            fail_prepare: false,
        })),
    );
    let mut writer = GuardCheckingWriter {
        alive: Arc::clone(&alive),
        prepared,
        expect_alive: true,
        bytes: Vec::new(),
        flushed: false,
    };

    let mut pending = write_control_reply(&mut writer, reply)
        .expect("wire write succeeds")
        .expect("guarded reply returns its retention");

    assert!(writer.flushed);
    let wire = String::from_utf8(writer.bytes).unwrap();
    assert!(wire.starts_with("OK 1 1 /private/artifact.png\nACK-CHALLENGE "));
    let nonce = wire
        .lines()
        .nth(1)
        .and_then(|line| {
            line.strip_prefix(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX)
        })
        .unwrap();
    assert!(aterm_types::control_verbs::valid_artifact_ack_nonce(nonce));
    assert!(
        alive.load(Ordering::Acquire),
        "artifact guards remain live after write and flush until client acknowledgement"
    );
    pending.retention.acknowledge_peer_anchor();
    drop(pending);
    assert!(
        !alive.load(Ordering::Acquire),
        "client acknowledgement releases the artifact guards"
    );
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
    let model = aterm_spec::derive::artifact_reply_publication_model();
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
    let model = aterm_spec::derive::artifact_reply_publication_model();

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
    let model = aterm_spec::derive::artifact_reply_publication_model();
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
    let model = aterm_spec::derive::artifact_reply_publication_model();
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

#[test]
fn guarded_reply_releases_only_after_explicit_peer_ack() {
    let alive = Arc::new(AtomicBool::new(true));
    let prepared = Arc::new(AtomicBool::new(false));
    let (mut client, server) = CtlStream::pair().unwrap();
    // The response and the challenge MUST arrive: a hang detector.
    client
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    let server_alive = Arc::clone(&alive);
    let server_prepared = Arc::clone(&prepared);
    let worker = std::thread::spawn(move || {
        let reply = ControlReply::guarded(
            "OK 1 1 /private/artifact.png\n".to_string(),
            Some(ReplyRetention::new(WireProbe {
                alive: server_alive,
                prepared: server_prepared,
                fail_prepare: false,
            })),
        );
        let mut writer = &server;
        let pending = write_control_reply(&mut writer, reply)
            .unwrap()
            .expect("guarded response");
        let mut reader = BufReader::new(&server);
        await_guarded_reply_close_with_quarantine(
            &server,
            &mut reader,
            pending,
            std::time::Duration::from_millis(40),
        )
    });

    let mut reader = BufReader::new(client.try_clone().unwrap());
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert_eq!(response, "OK 1 1 /private/artifact.png\n");
    let mut challenge = String::new();
    reader.read_line(&mut challenge).unwrap();
    let nonce = challenge
        .trim_end()
        .strip_prefix(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX)
        .unwrap();
    assert!(aterm_types::control_verbs::valid_artifact_ack_nonce(nonce));
    assert!(
        alive.load(Ordering::Acquire),
        "reading the response does not silently release its exact guard"
    );
    client
        .write_all(
            format!(
                "{}{nonce}\n",
                aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX
            )
            .as_bytes(),
        )
        .unwrap();
    client.flush().unwrap();
    assert_eq!(worker.join().unwrap(), ArtifactAckOutcome::PeerAcknowledged);
    assert!(
        !alive.load(Ordering::Acquire),
        "the exact ACK transition releases the guard"
    );
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

#[test]
fn pre_pipelined_ack_guess_is_failed_and_quarantined() {
    let alive = Arc::new(AtomicBool::new(true));
    let prepared = Arc::new(AtomicBool::new(false));
    let (mut client, server) = CtlStream::pair().unwrap();
    // This guess exists before the server creates its nonce and therefore
    // cannot causally acknowledge the response it has not read.
    client.write_all(b"ACK artifact\n").unwrap();
    client.flush().unwrap();
    let server_alive = Arc::clone(&alive);
    let worker = std::thread::spawn(move || {
        let reply = ControlReply::guarded(
            "OK 1 1 /private/artifact.png\n".to_string(),
            Some(ReplyRetention::new(WireProbe {
                alive: server_alive,
                prepared,
                fail_prepare: false,
            })),
        );
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
    assert_eq!(response, "OK 1 1 /private/artifact.png\n");
    let mut challenge = String::new();
    reader.read_line(&mut challenge).unwrap();
    assert!(challenge.starts_with(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX));
    let (outcome, ticket) = worker.join().unwrap();
    assert_eq!(outcome, ArtifactAckOutcome::AcknowledgementQuarantined);
    assert!(
        alive.load(Ordering::Acquire),
        "a pre-challenge guess must retain the exact guard in quarantine"
    );
    end_quarantine(ticket);
}

#[test]
fn eager_request_half_close_is_ack_failure_not_peer_ack() {
    let alive = Arc::new(AtomicBool::new(true));
    let prepared = Arc::new(AtomicBool::new(false));
    let (client, server) = CtlStream::pair().unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    let server_alive = Arc::clone(&alive);
    let worker = std::thread::spawn(move || {
        let reply = ControlReply::guarded(
            "OK 1 1 /private/artifact.png\n".to_string(),
            Some(ReplyRetention::new(WireProbe {
                alive: server_alive,
                prepared,
                fail_prepare: false,
            })),
        );
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
    assert_eq!(response, "OK 1 1 /private/artifact.png\n");
    let mut challenge = String::new();
    reader.read_line(&mut challenge).unwrap();
    assert!(challenge.starts_with(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX));
    let (outcome, ticket) = worker.join().unwrap();
    assert_eq!(
        outcome,
        ArtifactAckOutcome::AcknowledgementQuarantined,
        "request EOF is never accepted as proof the response was consumed"
    );
    assert!(
        alive.load(Ordering::Acquire),
        "an abandoned/invalid ACK path transfers the exact guard to quarantine"
    );
    end_quarantine(ticket);
    let deadline = std::time::Instant::now() + REAPER_PATIENCE;
    while alive.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        sweep_artifact_quarantine_for_test();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        !alive.load(Ordering::Acquire),
        "the quarantine reaper releases the guard only after its grace interval"
    );
}

#[test]
fn wire_edge_identity_failure_replaces_ok_and_aborts_guard() {
    let alive = Arc::new(AtomicBool::new(true));
    let prepared = Arc::new(AtomicBool::new(false));
    let reply = ControlReply::guarded(
        "OK stale-path\n".to_string(),
        Some(ReplyRetention::new(WireProbe {
            alive: Arc::clone(&alive),
            prepared: Arc::clone(&prepared),
            fail_prepare: true,
        })),
    );
    let mut writer = GuardCheckingWriter {
        alive: Arc::clone(&alive),
        prepared,
        expect_alive: false,
        bytes: Vec::new(),
        flushed: false,
    };

    let retention = write_control_reply(&mut writer, reply).expect("path-free ERR is writable");

    let body = String::from_utf8(writer.bytes).unwrap();
    assert!(body.starts_with("ERR artifact reply validation failed:"));
    assert!(!body.contains("stale-path"));
    assert!(!alive.load(Ordering::Acquire));
    assert!(
        retention.is_none(),
        "a path-free validation error carries no stale guard"
    );
}

#[test]
fn removed_bottom_hud_verbs_are_unadvertised_stable_tombstones() {
    assert_eq!(
        retired_bottom_hud_verb_error("widgets"),
        Some("ERR verb widgets was removed with the bottom HUD\n")
    );
    assert_eq!(
        retired_bottom_hud_verb_error("metric"),
        Some("ERR verb metric was removed with the bottom HUD\n")
    );
    assert_eq!(retired_bottom_hud_verb_error("metrics"), None);
    assert!(
        aterm_types::control_verbs::spec("widgets").is_none()
            && aterm_types::control_verbs::spec("metric").is_none(),
        "retired verbs must not reappear in help, completion, or authority catalogs"
    );
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
    }
    let row = |label: &'static str, seed: &'static [u8], call: Call| Row {
        label,
        seed,
        call,
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
use super::control_input::feed_bytes;
use super::control_input::send_bytes;
use super::control_input::{parse_resize, parse_tab, paste_text, take_mods};
use super::control_media::{
    MAX_IMAGE_PAYLOAD_BYTES, cmd_image_read, image_payload, image_read_line,
};
use super::control_query::{
    AbsRow, abs_row_text, cmd_cell, cmd_colors, cmd_cursor_json, cmd_cwd, cmd_line, cmd_mainscreen,
    cmd_modes, cmd_screen_styled_json, cmd_search, cmd_text, cmd_text_json, serialize_dims,
    serialize_dims_json, styled_image_json,
};
// `cmd_select`/`cmd_selection` are drawn only by the unix-gated pipe-backed tests.
use super::control_selection::{cmd_blocks, cmd_blocks_json, cmd_blocktext, cmd_wait};
#[cfg(unix)]
use super::control_selection::{cmd_select, cmd_selection};
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

#[test]
fn operator_proposal_schema_is_closed_and_human_approvals_are_rejected() {
    let valid = br#"{
            "schema":1,
            "event_id":7,
            "claim_token":"0000000000000000000000000000000000000000000000000000000000000000",
            "sid":"s-test",
            "generation":{"lifecycle_epoch":3,"alternate_screen":false,"content_seq":9,"fingerprint":"0000000000000000000000000000000000000000000000000000000000000000"},
            "action":{"kind":"turn","text":"Continue and summarize the result."},
            "expectation":{"kind":"busy_then_attention","deadline_ms":300000}
        }"#;
    assert!(decode_operator_proposal(valid).is_ok());

    let approval = String::from_utf8(valid.to_vec())
        .unwrap()
        .replace("Continue and summarize the result.", "yes");
    assert!(
        decode_operator_proposal(approval.as_bytes())
            .err()
            .unwrap()
            .contains("human-only")
    );
    let unknown = String::from_utf8(valid.to_vec())
        .unwrap()
        .replace("\"schema\":1,", "\"schema\":1,\"extra\":true,");
    assert!(decode_operator_proposal(unknown.as_bytes()).is_err());

    for disallowed in [r"line\nbreak", r"line\tbreak", " leading", "trailing "] {
        let proposal = String::from_utf8(valid.to_vec())
            .unwrap()
            .replace("Continue and summarize the result.", disallowed);
        assert!(
            decode_operator_proposal(proposal.as_bytes()).is_err(),
            "operator text {disallowed:?} must fail before any egress"
        );
    }

    // JSON `\\n` decodes to a literal backslash+n pair. Unlike the interactive
    // `paste` verb, the structured operator path preserves those exact bytes.
    let literal = r"Continue literally: \\n";
    let proposal = String::from_utf8(valid.to_vec())
        .unwrap()
        .replace("Continue and summarize the result.", literal);
    let decoded = decode_operator_proposal(proposal.as_bytes()).unwrap();
    assert_eq!(decoded.text, r"Continue literally: \n");
    assert_eq!(
        operator_paste_event(&decoded.text),
        InputEvent::Paste(r"Continue literally: \n".to_string(), PasteFraming::AtDrain)
    );
    assert_ne!(
        control_input::paste_text(&decoded.text),
        decoded.text,
        "regression control: interactive paste would expand the literal suffix"
    );
}

#[test]
fn operator_proposal_rejections_close_without_reading_body() {
    struct NoBodyRead;

    impl std::io::Read for NoBodyRead {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            panic!("operator proposal rejection attempted to read its body")
        }
    }

    impl std::io::BufRead for NoBodyRead {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            panic!("operator proposal rejection attempted to buffer its body")
        }

        fn consume(&mut self, _amount: usize) {}
    }

    let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(1);
    let operator =
        crate::operator_host::ControlHandle::new("test-proposal-pre-body".to_string(), notify_tx);
    let store = crate::session_store::new_store();
    let subscribers = crate::subscribe::new_registry();

    let assert_rejected = |line: &str,
                           scope: Scope,
                           operator: Option<&crate::operator_host::ControlHandle>,
                           expected: &str| {
        let mut reader = NoBodyRead;
        let mut output = Vec::new();
        assert!(
            !run_operator_proposal_bin(
                line,
                &mut reader,
                scope,
                operator,
                &store,
                &subscribers,
                &mut output,
            ),
            "an unread announced body makes the connection unrecoverable"
        );
        assert_eq!(String::from_utf8(output).unwrap(), expected);
    };

    assert_rejected(
        "operator-propose-bin 4",
        Scope::Edge(EdgeToken::generate()),
        Some(&operator),
        "ERR denied: operator-propose-bin is owner-only\n",
    );
    assert_rejected(
        "@3 operator-propose-bin 4",
        Scope::Owner,
        Some(&operator),
        "ERR denied: operator-propose-bin takes no @<sid>\n",
    );
    assert_rejected(
        "operator-propose-bin 4",
        Scope::Owner,
        None,
        &operator_unavailable(),
    );
    // Off and failed-to-start are told apart: the one action for off, the
    // launch error for a start that failed.
    assert_eq!(
        operator_unavailable_for(None),
        "ERR operator unavailable: off; set [operator] enabled = true in aterm.toml and \
         relaunch\n"
    );
    assert_eq!(
        operator_unavailable_for(Some("could not start operator observer: disk full")),
        "ERR operator unavailable: could not start operator observer: disk full\n"
    );
    assert_rejected(
        &format!("operator-propose-bin {}", MAX_OPERATOR_PROPOSAL + 1),
        Scope::Owner,
        Some(&operator),
        "ERR operator proposal too large\n",
    );

    operator.inject_fleet_fault_for_test(aterm_agent::operator::FleetFaultReason::ObserverPanicked);
    let response = run_operator_proposal(b"not even JSON", &operator, &store, &subscribers);
    assert!(response.contains("fleet faulted"), "{response}");
    assert!(response.contains("observer-panicked"), "{response}");
    assert!(
        !response.contains("invalid proposal JSON"),
        "fleet fault must reject before proposal parsing or any action"
    );
}

/// `operator-propose-bin` types into the session its body names, so that session's
/// halt is the one it obeys; it never consults the front tab, so which tab is in
/// front (held, unheld, or native with `active == None`) cannot change the answer.
#[test]
#[cfg(unix)]
fn operator_proposal_is_halted_on_its_target_sid_not_the_front_tab() {
    let hold = || {
        Some(crate::fabric::Hold {
            reason: "incident".to_string(),
            origin: "fleet".to_string(),
        })
    };
    let (session_a, rx_a) = pipe_session(81);
    let (session_b, rx_b) = pipe_session(82);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(session_a.clone());
    store.write().unwrap().register(session_b.clone());
    let subscribers = crate::subscribe::new_registry();
    let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(1);
    // An invalid fleet id: past the halt gate the proposal's first durable step
    // (`queue()`) is refused by fleet-id validation before any state root is
    // resolved, so neither arm touches the owner's operator state.
    let operator =
        crate::operator_host::ControlHandle::new("not a valid fleet id".to_string(), notify_tx);
    let propose = |sid: &str| -> String {
        let body = format!(
            r#"{{"schema":1,"event_id":7,"claim_token":"{zeros}","sid":"{sid}","generation":{{"lifecycle_epoch":3,"alternate_screen":false,"content_seq":9,"fingerprint":"{zeros}"}},"action":{{"kind":"turn","text":"echo pwned"}},"expectation":{{"kind":"busy_then_attention","deadline_ms":1000}}}}"#,
            zeros = "0".repeat(64),
        );
        let line = format!("operator-propose-bin {}", body.len());
        let mut reader = std::io::Cursor::new(body.into_bytes());
        let mut output = Vec::new();
        let _ = run_operator_proposal_bin(
            &line,
            &mut reader,
            Scope::Owner,
            Some(&operator),
            &store,
            &subscribers,
            &mut output,
        );
        String::from_utf8(output).unwrap()
    };
    let sid_b = session_b.sid.as_str().to_string();

    // B held, A (any front tab) unheld: the proposal for B is halted, zero bytes.
    assert!(crate::fabric::apply_hold_for_test(&session_b.ctx, hold()));
    assert!(crate::fabric::halt_refusal(&session_a.ctx, "operator-propose-bin").is_none());
    let epoch_b = session_b.ctx.sink.input_epoch();
    assert_eq!(
        propose(&sid_b),
        "ERR halted reason=incident origin=fleet\n",
        "a held target must refuse whatever tab is in front"
    );
    assert_eq!(session_b.ctx.sink.input_epoch(), epoch_b);
    assert!(
        drain_pipe(&rx_b).is_empty(),
        "no operator byte reached held B"
    );

    // Mirror: A held, B unheld — the proposal for B is not halted.
    assert!(crate::fabric::apply_hold_for_test(&session_b.ctx, None));
    assert!(crate::fabric::apply_hold_for_test(&session_a.ctx, hold()));
    let reply = propose(&sid_b);
    assert!(
        !reply.starts_with("ERR halted"),
        "another session's hold must not refuse a proposal for an unheld target: {reply}"
    );
    assert!(drain_pipe(&rx_a).is_empty());
}

/// A hold landing mid-turn (after the proposal's gate, between validation and the
/// paste or Enter) still stops the operator's egress: the final fence refuses with
/// zero bytes, as for a changed screen.
#[test]
#[cfg(unix)]
fn operator_egress_refuses_a_session_held_mid_turn() {
    use aterm_agent::operator::EventGeneration;

    let (target, rx) = pipe_session(83);
    aterm_pty::set_nonblocking(target.master, true).expect("nonblocking test master");
    target.ctx.sink.note_master_nonblocking(true);
    let generation = {
        let terminal = term_lock(&target.term);
        let evidence = crate::operator_host::terminal_evidence(&terminal);
        EventGeneration::new(
            0,
            terminal.is_alternate_screen(),
            terminal.content_seq(),
            Sha256::digest(evidence.as_bytes()),
        )
    };
    let paste = || {
        operator_input_if_epoch(
            &target.term,
            &target.ctx,
            Some(operator_paste_event("echo pwned")),
            target.ctx.sink.input_epoch(),
            OperatorTerminalFence::Exact(generation),
        )
    };
    assert!(crate::fabric::apply_hold_for_test(
        &target.ctx,
        Some(crate::fabric::Hold {
            reason: "incident".to_string(),
            origin: "fleet".to_string(),
        })
    ));
    let held = paste();
    assert!(matches!(held, Delivery::ConflictZero), "{held:?}");
    assert!(
        drain_pipe(&rx).is_empty(),
        "a held session got operator bytes"
    );

    // Control: the same egress, unheld, delivers — the refusal was the hold.
    assert!(crate::fabric::apply_hold_for_test(&target.ctx, None));
    let delivered = paste();
    assert!(
        matches!(delivered, Delivery::FullAt { .. }),
        "{delivered:?}"
    );
    assert!(
        String::from_utf8_lossy(&drain_pipe(&rx)).contains("echo pwned"),
        "the unheld control delivered"
    );
}

#[test]
#[cfg(unix)]
fn guarded_turn_redacts_operator_text_while_generic_turn_keeps_text() {
    let store = crate::session_store::new_store();
    let (operator_target, _operator_rx) = pipe_session(68);
    store.write().unwrap().register(operator_target.clone());
    let subscribers = subscribe::new_registry();
    let secret = "credential-like proposal body";
    let paste = |text: &str| {
        assert_eq!(text, secret);
        true
    };
    let press = |_: &str| panic!("submit=none must not press");
    let mut preflight = || Ok(());
    let mut pre_submit = || Ok(());
    let response = control_session::cmd_turn_guarded(
        &operator_target.term,
        &store,
        operator_target.local_id,
        &format!("idle=1 timeout=1000 submit=none -- {secret}"),
        &subscribers,
        &operator_target.ctx,
        &control_session::TurnIo {
            paste: &paste,
            press: &press,
            ..control_session::TurnIo::paste_only()
        },
        &mut preflight,
        &mut pre_submit,
        Some("[operator action]"),
    );
    assert!(response.starts_with("OK "), "{response}");
    let operator_history = control_session::cmd_history(&operator_target.ctx, "");
    assert!(!operator_history.contains(secret));
    assert!(
        operator_target
            .ctx
            .turns
            .lock()
            .unwrap()
            .since(None)
            .all(|record| record.text == "[operator action]")
    );

    let (generic_target, _generic_rx) = pipe_session(69);
    store.write().unwrap().register(generic_target.clone());
    let generic = control_session::cmd_turn(
        &generic_target.term,
        &store,
        generic_target.local_id,
        &format!("idle=1 timeout=1000 submit=none -- {secret}"),
        &subscribers,
        &generic_target.ctx,
        &control_session::TurnIo {
            paste: &paste,
            press: &press,
            ..control_session::TurnIo::paste_only()
        },
    );
    assert!(generic.starts_with("OK "), "{generic}");
    assert_eq!(
        generic_target
            .ctx
            .turns
            .lock()
            .unwrap()
            .since(None)
            .last()
            .map(|record| record.text.as_str()),
        Some(secret),
        "ordinary turn ledger behavior changed"
    );
}

#[test]
#[cfg(unix)]
fn foreign_input_between_operator_paste_and_submit_emits_no_enter() {
    use std::collections::BTreeMap;
    use std::io::Read as _;

    use aterm_agent::operator::{
        AttentionCondition, DurableQueue, EventGeneration, EventStatus, NewEvent, QueueConfig,
    };
    use aterm_spec::derive::operator_wal_actuator_model;
    use aterm_spec::verify;

    let store = crate::session_store::new_store();
    let (target, mut reader) = pipe_session(70);
    store.write().unwrap().register(target.clone());
    aterm_pty::set_nonblocking(target.master, true).expect("nonblocking test master");
    target.ctx.sink.note_master_nonblocking(true);

    let unique = format!(
        "aterm-operator-interjection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    );
    let directory = std::env::temp_dir().join(unique);
    let _ = std::fs::remove_dir_all(&directory);
    let queue = DurableQueue::open(&directory, 1, QueueConfig::default()).unwrap();
    queue.manage_sid("s-interjection").unwrap();
    let fingerprint: [u8; 32] = Sha256::digest(b"ready");
    queue
        .enqueue(NewEvent::new(
            "s-interjection",
            EventGeneration::new(1, false, 1, fingerprint),
            AttentionCondition::Ready,
            "ready",
        ))
        .unwrap();
    let claim = queue.claim().unwrap().unwrap();

    let epoch = std::cell::Cell::new(target.ctx.sink.input_epoch());
    let initial_terminal_generation = {
        let terminal = term_lock(&target.term);
        let evidence = crate::operator_host::terminal_evidence(&terminal);
        EventGeneration::new(
            0,
            terminal.is_alternate_screen(),
            terminal.content_seq(),
            Sha256::digest(evidence.as_bytes()),
        )
    };
    let paste = |text: &str| {
        let delivered = operator_input_if_epoch(
            &target.term,
            &target.ctx,
            Some(InputEvent::Paste(text.to_string(), PasteFraming::AtDrain)),
            epoch.get(),
            OperatorTerminalFence::Exact(initial_terminal_generation),
        );
        let Delivery::FullAt { epoch: advanced } = delivered else {
            panic!("operator paste failed unexpectedly: {delivered:?}");
        };
        epoch.set(advanced);
        // Deterministic human/raw-controller interjection after paste and
        // before cmd_turn_guarded reaches its submit hook.
        assert_eq!(target.ctx.sink.write_frame(b"human").unwrap(), 5);
        true
    };
    let presses = std::sync::atomic::AtomicUsize::new(0);
    let press = |_: &str| {
        presses.fetch_add(1, Ordering::SeqCst);
        false
    };
    let validate_epoch = || {
        if target.ctx.sink.input_epoch() == epoch.get() {
            Ok(())
        } else {
            Err("target input interjected".to_string())
        }
    };
    let response = operator_action_transaction(
        &queue,
        claim.event.id,
        &claim.token,
        &"22".repeat(32),
        validate_epoch,
        |preflight| {
            let mut pre_submit = || {
                if target.ctx.sink.input_epoch() == epoch.get() {
                    Ok(())
                } else {
                    Err("target input interjected".to_string())
                }
            };
            control_session::cmd_turn_guarded(
                &target.term,
                &store,
                target.local_id,
                "idle=1 timeout=1000 presses=1 -- operator",
                &subscribe::new_registry(),
                &target.ctx,
                &control_session::TurnIo {
                    paste: &paste,
                    press: &press,
                    ..control_session::TurnIo::paste_only()
                },
                preflight,
                &mut pre_submit,
                Some("[operator action]"),
            )
        },
    );
    assert!(
        response.starts_with("ERR operator action in doubt"),
        "{response}"
    );
    assert_eq!(presses.load(Ordering::SeqCst), 0, "Enter was attempted");
    assert!(matches!(
        queue.snapshot(claim.event.id).unwrap().status,
        EventStatus::InDoubt { .. }
    ));

    // Tier-1 bind of the real conditional sink write and durable transaction to the
    // extended actuator model; a forged submit across the same foreign epoch is the
    // negative control.
    let project = |before: &BTreeMap<&'static str, i64>, changes: &[(&'static str, i64)]| {
        let mut after = before.clone();
        for (name, value) in changes {
            after.insert(*name, *value);
        }
        after
    };
    let assert_step = |before: &BTreeMap<&'static str, i64>,
                       after: &BTreeMap<&'static str, i64>,
                       action: &str| {
        let model = operator_wal_actuator_model();
        let (accepted, diagnostics) = verify::validate_transition_tiered(
            &model,
            &[("Buggy", 0)],
            before,
            after,
            Some(action),
            "operator real interjection fence",
        );
        assert!(accepted, "{action} rejected\n{diagnostics}");
    };
    let model = operator_wal_actuator_model();
    let initial = model.init_state();
    let intent = project(&initial, &[("phase", 1), ("intent_durable", 1)]);
    let pasted = project(
        &intent,
        &[
            ("phase", 2),
            ("mutations", 1),
            ("input_epoch", 1),
            ("expected_epoch", 1),
        ],
    );
    let interjected = project(&pasted, &[("input_epoch", 2), ("interjected", 1)]);
    let rejected = project(&interjected, &[("phase", 3), ("in_doubt", 1)]);
    assert_step(&initial, &intent, "PersistIntent");
    assert_step(&intent, &pasted, "MutateOnce");
    assert_step(&pasted, &interjected, "ForeignInput");
    assert_step(&interjected, &rejected, "RejectInterjectedSubmit");
    let forged_submit = project(&interjected, &[("submit_writes", 1)]);
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        &model,
        &[("Buggy", 0)],
        &interjected,
        &forged_submit,
        Some("RejectInterjectedSubmit"),
        "operator interjection submit negative control",
    );
    assert!(
        !accepted,
        "foreign-input submit negative control was admitted\n{diagnostics}"
    );

    let mut landed = [0_u8; 13];
    reader.read_exact(&mut landed).expect("paste + human input");
    assert_eq!(&landed, b"operatorhuman");
    assert!(
        drain_pipe(&reader).is_empty(),
        "guarded Enter must never arrive late"
    );
    drop(queue);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn input_change_during_intent_fsync_stops_before_paste() {
    use aterm_agent::operator::{
        AttentionCondition, DurableQueue, EventGeneration, EventStatus, NewEvent, QueueConfig,
    };

    let unique = format!(
        "aterm-operator-post-wal-fence-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    );
    let directory = std::env::temp_dir().join(unique);
    let _ = std::fs::remove_dir_all(&directory);
    let queue = DurableQueue::open(&directory, 1, QueueConfig::default()).unwrap();
    queue.manage_sid("s-post-wal").unwrap();
    let fingerprint: [u8; 32] = Sha256::digest(b"ready");
    queue
        .enqueue(NewEvent::new(
            "s-post-wal",
            EventGeneration::new(1, false, 1, fingerprint),
            AttentionCondition::Ready,
            "ready",
        ))
        .unwrap();
    let claim = queue.claim().unwrap().unwrap();

    let validations = std::sync::atomic::AtomicUsize::new(0);
    let pastes = std::sync::atomic::AtomicUsize::new(0);
    let response = operator_action_transaction(
        &queue,
        claim.event.id,
        &claim.token,
        &"33".repeat(32),
        || {
            let call = validations.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                Ok(())
            } else {
                // Deterministic stand-in for an epoch/state change while
                // begin_action's durable append + fsync was in flight.
                Err("target input interjected during intent persistence".to_string())
            }
        },
        |preflight| match preflight() {
            Ok(()) => {
                pastes.fetch_add(1, Ordering::SeqCst);
                "OK 0 turn submitted=1 status=settled\n".to_string()
            }
            Err(error) => format!("ERR {error}\n"),
        },
    );
    assert!(
        response.starts_with("ERR operator action in doubt"),
        "{response}"
    );
    assert_eq!(validations.load(Ordering::SeqCst), 2);
    assert_eq!(pastes.load(Ordering::SeqCst), 0, "paste crossed WAL fsync");
    assert!(matches!(
        queue.snapshot(claim.event.id).unwrap().status,
        EventStatus::InDoubt { .. }
    ));
    drop(queue);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
#[cfg(unix)]
fn echoed_operator_paste_captures_post_paste_generation_and_submits_once() {
    use std::io::Read as _;

    use aterm_agent::operator::{
        AttentionCondition, DurableQueue, EventGeneration, EventStatus, NewEvent, QueueConfig,
        Resolution,
    };

    let store = crate::session_store::new_store();
    let (target, mut reader) = pipe_session(73);
    store.write().unwrap().register(target.clone());
    aterm_pty::set_nonblocking(target.master, true).expect("nonblocking test master");
    target.ctx.sink.note_master_nonblocking(true);
    term_lock(&target.term).process(b"ready> ");

    let initial_generation = {
        let terminal = term_lock(&target.term);
        let evidence = crate::operator_host::terminal_evidence(&terminal);
        EventGeneration::new(
            0,
            terminal.is_alternate_screen(),
            terminal.content_seq(),
            Sha256::digest(evidence.as_bytes()),
        )
    };
    let initial_evidence = {
        let terminal = term_lock(&target.term);
        crate::operator_host::terminal_evidence(&terminal)
    };
    let unique = format!(
        "aterm-operator-echo-fence-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    );
    let directory = std::env::temp_dir().join(unique);
    let _ = std::fs::remove_dir_all(&directory);
    let queue = DurableQueue::open(&directory, 1, QueueConfig::default()).unwrap();
    queue.manage_sid("s-echo").unwrap();
    queue
        .enqueue(NewEvent::new(
            "s-echo",
            initial_generation,
            AttentionCondition::Ready,
            initial_evidence,
        ))
        .unwrap();
    let claim = queue.claim().unwrap().unwrap();
    let proposal = OperatorProposal {
        event_id: claim.event.id,
        token: claim.token.clone(),
        sid: "s-echo".to_string(),
        generation: initial_generation,
        text: "continue work".to_string(),
        action_hash: "44".repeat(32),
    };

    let epoch = std::cell::Cell::new(target.ctx.sink.input_epoch());
    let submit_generation = std::cell::Cell::new(None);
    let paste = |text: &str| {
        let delivery = operator_input_if_epoch(
            &target.term,
            &target.ctx,
            Some(InputEvent::Paste(text.to_string(), PasteFraming::AtDrain)),
            epoch.get(),
            OperatorTerminalFence::Exact(proposal.generation),
        );
        let Delivery::FullAt { epoch: advanced } = delivery else {
            panic!("paste did not land: {delivery:?}");
        };
        epoch.set(advanced);
        // Realistic editor echo: this is OUTPUT, so it advances the terminal
        // generation without touching the attempted-input epoch.
        term_lock(&target.term).process(text.as_bytes());
        true
    };
    let presses = std::sync::atomic::AtomicUsize::new(0);
    let press = |name: &str| {
        assert_eq!(name, "enter");
        let generation = submit_generation.get().expect("post-paste capture");
        let delivery = operator_input_if_epoch(
            &target.term,
            &target.ctx,
            parse_key(name),
            epoch.get(),
            OperatorTerminalFence::Exact(generation),
        );
        let Delivery::FullAt { epoch: advanced } = delivery else {
            panic!("submit did not land: {delivery:?}");
        };
        epoch.set(advanced);
        presses.fetch_add(1, Ordering::SeqCst);
        term_lock(&target.term).process(b"\r\nresponse\r\n");
        true
    };
    let validate = || {
        validate_operator_action_state(&queue, &proposal, false)?;
        if target.ctx.sink.input_epoch() != epoch.get() {
            return Err("target input interjected".to_string());
        }
        let terminal = term_lock(&target.term);
        let evidence = crate::operator_host::terminal_evidence(&terminal);
        let fingerprint: [u8; 32] = Sha256::digest(evidence.as_bytes());
        if terminal.content_seq() != proposal.generation.content_seq
            || fingerprint != proposal.generation.fingerprint
        {
            return Err("target changed before paste".to_string());
        }
        Ok(())
    };
    let response = operator_action_transaction(
        &queue,
        claim.event.id,
        &claim.token,
        &proposal.action_hash,
        validate,
        |preflight| {
            let mut pre_submit = || {
                validate_operator_action_state(&queue, &proposal, true)?;
                if target.ctx.sink.input_epoch() != epoch.get() {
                    return Err("target input interjected".to_string());
                }
                let terminal = term_lock(&target.term);
                let evidence = crate::operator_host::terminal_evidence(&terminal);
                if crate::operator_host::looks_like_approval(&evidence) {
                    return Err("approval-shaped screens are human-only".to_string());
                }
                let generation = EventGeneration::new(
                    proposal.generation.lifecycle_epoch,
                    terminal.is_alternate_screen(),
                    terminal.content_seq(),
                    Sha256::digest(evidence.as_bytes()),
                );
                assert_ne!(
                    generation, proposal.generation,
                    "echo must create a distinct post-paste generation"
                );
                submit_generation.set(Some(generation));
                Ok(())
            };
            control_session::cmd_turn_guarded(
                &target.term,
                &store,
                target.local_id,
                "idle=1 timeout=1000 presses=1 submit_verify=seq -- continue work",
                &subscribe::new_registry(),
                &target.ctx,
                &control_session::TurnIo {
                    paste: &paste,
                    press: &press,
                    ..control_session::TurnIo::paste_only()
                },
                preflight,
                &mut pre_submit,
                Some("[operator action]"),
            )
        },
    );
    assert!(response.starts_with("OK "), "{response}");
    assert!(response.contains("submitted=1"), "{response}");
    assert_eq!(presses.load(Ordering::SeqCst), 1);
    assert!(matches!(
        queue.snapshot(claim.event.id).unwrap().status,
        EventStatus::Resolved {
            resolution: Resolution::Acted,
            ..
        }
    ));
    let mut landed = vec![0_u8; proposal.text.len() + 1];
    reader.read_exact(&mut landed).expect("paste plus Enter");
    let mut expected = proposal.text.as_bytes().to_vec();
    expected.push(b'\r');
    assert_eq!(landed, expected);
    assert!(drain_pipe(&reader).is_empty(), "more than one Enter landed");
    drop(queue);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
#[cfg(unix)]
fn output_change_after_submit_capture_is_rejected_under_terminal_lock() {
    use aterm_agent::operator::EventGeneration;

    let (target, reader) = pipe_session(74);
    aterm_pty::set_nonblocking(target.master, true).expect("nonblocking test master");
    target.ctx.sink.note_master_nonblocking(true);
    term_lock(&target.term).process(b"safe post-paste draft");
    let epoch = target.ctx.sink.input_epoch();
    let captured = {
        let terminal = term_lock(&target.term);
        let evidence = crate::operator_host::terminal_evidence(&terminal);
        EventGeneration::new(
            0,
            terminal.is_alternate_screen(),
            terminal.content_seq(),
            Sha256::digest(evidence.as_bytes()),
        )
    };

    // Output arrives after pre-submit captured the safe generation. The final
    // compare and key encoding share the terminal lock, so Enter contributes zero
    // bytes instead of answering the new approval prompt.
    term_lock(&target.term).process(b"\r\nAllow this command? [y/N]");
    assert_eq!(
        operator_input_if_epoch(
            &target.term,
            &target.ctx,
            parse_key("enter"),
            epoch,
            OperatorTerminalFence::Exact(captured),
        ),
        Delivery::ConflictZero
    );
    assert!(drain_pipe(&reader).is_empty(), "Enter reached the PTY");
}

#[test]
#[cfg(unix)]
fn contended_terminal_at_final_egress_refuses_immediately_without_sink_write() {
    use aterm_agent::operator::EventGeneration;

    let (target, reader) = pipe_session(75);
    aterm_pty::set_nonblocking(target.master, true).expect("nonblocking test master");
    target.ctx.sink.note_master_nonblocking(true);
    let epoch = target.ctx.sink.input_epoch();
    let terminal_guard = term_lock(&target.term);
    let evidence = crate::operator_host::terminal_evidence(&terminal_guard);
    let generation = EventGeneration::new(
        0,
        terminal_guard.is_alternate_screen(),
        terminal_guard.content_seq(),
        Sha256::digest(evidence.as_bytes()),
    );

    let started = std::time::Instant::now();
    assert_eq!(
        operator_input_if_epoch(
            &target.term,
            &target.ctx,
            parse_key("enter"),
            epoch,
            OperatorTerminalFence::Exact(generation),
        ),
        Delivery::BusyZero
    );
    // A blocking regression deadlocks on the guard this thread holds, so
    // any finite bound catches it and the minute is a hang detector; 1 s
    // also measured one long preemption (the load-sensitive test audit of
    // 2026-09-27).
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "final egress waited on the terminal lock"
    );
    assert_eq!(target.ctx.sink.input_epoch(), epoch);
    drop(terminal_guard);
    assert!(drain_pipe(&reader).is_empty(), "Enter reached the PTY");
}

#[test]
fn operator_action_transaction_is_wal_first_exactly_once_and_never_retries() {
    use std::collections::BTreeMap;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use aterm_agent::operator::{
        AttentionCondition, DurableQueue, EventGeneration, EventStatus, NewEvent, QueueConfig,
        Resolution,
    };
    use aterm_spec::derive::{Model, operator_wal_actuator_model};
    use aterm_spec::verify;

    type State = BTreeMap<&'static str, i64>;

    fn state_after(before: &State, changes: &[(&'static str, i64)]) -> State {
        let mut after = before.clone();
        for (name, value) in changes {
            after.insert(name, *value);
        }
        after
    }

    fn assert_transition(model: &Model, before: &State, after: &State, action: &str, label: &str) {
        let (accepted, diagnostics) = verify::validate_transition_tiered(
            model,
            &[("Buggy", 0)],
            before,
            after,
            Some(action),
            label,
        );
        assert!(
            accepted,
            "shipping {label} transition must be admitted as {action}\n{diagnostics}"
        );
    }

    fn assert_rejected(model: &Model, before: &State, forged: &State, action: &str, label: &str) {
        let (accepted, diagnostics) = verify::validate_transition_tiered(
            model,
            &[("Buggy", 0)],
            before,
            forged,
            Some(action),
            label,
        );
        assert!(
            !accepted,
            "{label} negative control was admitted as {action}\n{diagnostics}"
        );
    }

    let unique = format!(
        "aterm-operator-transaction-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    );
    let directory = std::env::temp_dir().join(unique);
    let _ = std::fs::remove_dir_all(&directory);
    let queue = DurableQueue::open(&directory, 1, QueueConfig::default()).unwrap();
    queue.manage_sid("s-test").unwrap();

    let claim = |evidence: &str, seq: u64| {
        let fingerprint: [u8; 32] = Sha256::digest(evidence.as_bytes());
        queue
            .enqueue(NewEvent::new(
                "s-test",
                EventGeneration::new(1, false, seq, fingerprint),
                AttentionCondition::Ready,
                evidence,
            ))
            .unwrap();
        queue.claim().unwrap().unwrap()
    };

    let first = claim("ready one", 1);
    let action_hash = "11".repeat(32);
    let mutations = std::sync::atomic::AtomicUsize::new(0);
    let model = operator_wal_actuator_model();
    let initial = model.init_state();
    let intent = state_after(&initial, &[("phase", 1), ("intent_durable", 1)]);
    let mutated = state_after(
        &intent,
        &[
            ("phase", 2),
            ("mutations", 1),
            ("input_epoch", 1),
            ("expected_epoch", 1),
        ],
    );
    let submitted = state_after(&mutated, &[("submit_writes", 1)]);
    let response = operator_action_transaction(
        &queue,
        first.event.id,
        &first.token,
        &action_hash,
        || Ok(()),
        |preflight| {
            preflight().unwrap();
            assert!(matches!(
                queue.snapshot(first.event.id).unwrap().status,
                EventStatus::ActionInFlight { .. }
            ));
            assert_transition(
                &model,
                &initial,
                &intent,
                "PersistIntent",
                "operator actuator durable preflight",
            );
            mutations.fetch_add(1, Ordering::SeqCst);
            assert_transition(
                &model,
                &intent,
                &mutated,
                "MutateOnce",
                "operator actuator terminal input",
            );
            assert_transition(
                &model,
                &mutated,
                &submitted,
                "GuardedSubmit",
                "operator actuator epoch-guarded submit",
            );
            "OK 1 turn submitted=1 status=settled seq=1 id=1 dur_ms=1 hash=abc\nsecret settled screen row\n".to_string()
        },
    );
    assert!(response.starts_with("OK "));
    assert!(response.contains("outcome=acted"), "{response}");
    assert!(
        !response.contains("secret settled screen row"),
        "operator reply leaked cmd_turn screen: {response}"
    );
    assert_eq!(mutations.load(Ordering::SeqCst), 1);
    assert!(matches!(
        queue.snapshot(first.event.id).unwrap().status,
        EventStatus::Resolved {
            resolution: Resolution::Acted,
            ..
        }
    ));
    let result = state_after(
        &submitted,
        &[("phase", 4), ("result_durable", 1), ("resolved", 1)],
    );
    assert_transition(
        &model,
        &submitted,
        &result,
        "PersistResult",
        "operator actuator durable result and resolution",
    );
    // A finished action has exactly one durable outcome: the unknown-outcome
    // path can no longer mark it in doubt.
    assert!(
        queue
            .mark_action_in_doubt(first.event.id, &first.token, "late unknown outcome")
            .is_err()
    );
    assert!(matches!(
        queue.snapshot(first.event.id).unwrap().status,
        EventStatus::Resolved { .. }
    ));
    let doubted_result = state_after(&result, &[("phase", 3), ("in_doubt", 1)]);
    assert_rejected(
        &model,
        &result,
        &doubted_result,
        "CrashAfterMutation",
        "operator actuator finished action marked in doubt",
    );
    // Negative control: the paste the preflight fenced (terminal input before
    // `begin_action`'s intent frame is durable).
    let paste_first = state_after(
        &initial,
        &[
            ("phase", 2),
            ("mutations", 1),
            ("input_epoch", 1),
            ("expected_epoch", 1),
        ],
    );
    assert_rejected(
        &model,
        &initial,
        &paste_first,
        "MutateOnce",
        "operator actuator paste before its durable intent",
    );

    let second = claim("ready two", 2);
    let ambiguous_intent = state_after(&initial, &[("phase", 1), ("intent_durable", 1)]);
    let ambiguous_mutation = state_after(
        &ambiguous_intent,
        &[
            ("phase", 2),
            ("mutations", 1),
            ("input_epoch", 1),
            ("expected_epoch", 1),
        ],
    );
    let response = operator_action_transaction(
        &queue,
        second.event.id,
        &second.token,
        &action_hash,
        || Ok(()),
        |preflight| {
            preflight().unwrap();
            assert!(matches!(
                queue.snapshot(second.event.id).unwrap().status,
                EventStatus::ActionInFlight { .. }
            ));
            assert_transition(
                &model,
                &initial,
                &ambiguous_intent,
                "PersistIntent",
                "ambiguous actuator durable preflight",
            );
            mutations.fetch_add(1, Ordering::SeqCst);
            assert_transition(
                &model,
                &ambiguous_intent,
                &ambiguous_mutation,
                "MutateOnce",
                "ambiguous actuator terminal input",
            );
            "ERR submit verification timeout\nsecret-screen-row\n".to_string()
        },
    );
    assert!(response.starts_with("ERR operator action in doubt"));
    assert_eq!(
        mutations.load(Ordering::SeqCst),
        2,
        "ambiguous action is not retried"
    );
    assert!(matches!(
        queue.snapshot(second.event.id).unwrap().status,
        EventStatus::InDoubt { ref reason, .. }
            if !reason.contains("secret-screen-row")
    ));
    let ambiguous = state_after(&ambiguous_mutation, &[("phase", 3), ("in_doubt", 1)]);
    assert_transition(
        &model,
        &ambiguous_mutation,
        &ambiguous,
        "CrashAfterMutation",
        "ambiguous actuator outcome",
    );
    // Negative control: the same run persisted as `Acted`, as if the transaction's
    // `submitted=1` check were dropped.
    let forged_acted = state_after(
        &ambiguous_mutation,
        &[("phase", 4), ("result_durable", 1), ("resolved", 1)],
    );
    assert_rejected(
        &model,
        &ambiguous_mutation,
        &forged_acted,
        "PersistResult",
        "operator actuator unsubmitted action persisted as acted",
    );

    drop(queue);

    // Crash after the shipping helper durably wrote intent and called its executor,
    // before appending a result. Takeover must expose InDoubt, and re-entering the
    // transaction must fail preflight before the mocked input seam is reached.
    let crash_directory = directory.join("crash");
    let crash_queue = DurableQueue::open(&crash_directory, 1, QueueConfig::default()).unwrap();
    crash_queue.manage_sid("s-test").unwrap();
    let crash_evidence = "ready crash";
    let crash_fingerprint: [u8; 32] = Sha256::digest(crash_evidence.as_bytes());
    crash_queue
        .enqueue(NewEvent::new(
            "s-test",
            EventGeneration::new(1, false, 3, crash_fingerprint),
            AttentionCondition::Ready,
            crash_evidence,
        ))
        .unwrap();
    let crash_claim = crash_queue.claim().unwrap().unwrap();
    let crash_mutations = std::sync::atomic::AtomicUsize::new(0);
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        operator_action_transaction(
            &crash_queue,
            crash_claim.event.id,
            &crash_claim.token,
            &action_hash,
            || Ok(()),
            |preflight| {
                preflight().unwrap();
                assert!(matches!(
                    crash_queue.snapshot(crash_claim.event.id).unwrap().status,
                    EventStatus::ActionInFlight { .. }
                ));
                crash_mutations.fetch_add(1, Ordering::SeqCst);
                panic!("simulated process loss after terminal input")
            },
        )
    }));
    assert!(panicked.is_err());
    assert_eq!(crash_mutations.load(Ordering::SeqCst), 1);
    drop(crash_queue);

    let recovered = DurableQueue::open(&crash_directory, 2, QueueConfig::default()).unwrap();
    assert!(matches!(
        recovered.snapshot(crash_claim.event.id).unwrap().status,
        EventStatus::InDoubt { .. }
    ));
    let crash_intent = state_after(&initial, &[("phase", 1), ("intent_durable", 1)]);
    let crash_mutated = state_after(
        &crash_intent,
        &[
            ("phase", 2),
            ("mutations", 1),
            ("input_epoch", 1),
            ("expected_epoch", 1),
        ],
    );
    let crash_in_doubt = state_after(&crash_mutated, &[("phase", 3), ("in_doubt", 1)]);
    assert_transition(
        &model,
        &crash_mutated,
        &crash_in_doubt,
        "CrashAfterMutation",
        "crashed actuator takeover",
    );

    let retry = operator_action_transaction(
        &recovered,
        crash_claim.event.id,
        &crash_claim.token,
        &action_hash,
        || Ok(()),
        |preflight| match preflight() {
            Ok(()) => {
                crash_mutations.fetch_add(1, Ordering::SeqCst);
                "OK 0 turn submitted=1 status=settled\n".to_string()
            }
            Err(error) => format!("ERR {error}\n"),
        },
    );
    assert!(retry.starts_with("ERR "));
    assert_eq!(
        crash_mutations.load(Ordering::SeqCst),
        1,
        "takeover must not call terminal input again"
    );
    assert_transition(
        &model,
        &crash_in_doubt,
        &crash_in_doubt,
        "ReplayInDoubt",
        "recovered actuator replay refusal",
    );

    // Negative control: the Buggy=1 duplicate-input successor, which healthy model
    // semantics must reject.
    let forged_replay = state_after(&crash_in_doubt, &[("mutations", 2), ("replayed", 1)]);
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        &model,
        &[("Buggy", 0)],
        &crash_in_doubt,
        &forged_replay,
        Some("ReplayInDoubt"),
        "operator actuator duplicate-input negative control",
    );
    assert!(
        !accepted,
        "duplicate-input negative control was accepted; binding is vacuous\n{diagnostics}"
    );

    drop(recovered);
    let _ = std::fs::remove_dir_all(directory);
}

/// An owner-only refusal names the rule that refused: the @<sid> or the scope.
#[test]
fn owner_only_refusals_say_which_rule_refused() {
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    let sid = Selector::Local(7);
    assert_eq!(
        owner_only_refusal("sessions", Some(&sid), Scope::Owner).as_deref(),
        Some("ERR denied: sessions takes no @<sid>\n"),
        "an owner who named a session is told the selector is the problem"
    );
    assert_eq!(
        owner_only_refusal("sessions", None, edge).as_deref(),
        Some("ERR denied: sessions is owner-only\n")
    );
    assert_eq!(
        owner_only_refusal("sessions", Some(&Selector::SelfTok), Scope::Owner),
        None
    );
}

/// The `dial <name>` relay verb is OWNER-only and takes exactly one connection
/// name; both are rejected with a friendly `ERR` BEFORE any dial is attempted,
/// and a non-`dial <name>` line passes straight through (handled elsewhere).
#[test]
fn dial_verb_gate_rejects_non_owner_and_malformed_names() {
    let reply = |line: &str, scope: Scope| -> String {
        let (client, server) = CtlStream::pair().unwrap();
        let mut reader = BufReader::new(server.try_clone().unwrap());
        assert!(try_net_dial(line, scope, &server, &mut reader));
        let mut resp = String::new();
        BufReader::new(client).read_line(&mut resp).unwrap();
        resp
    };
    // An edge-scoped connection cannot dial out (owner authority required).
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    assert_eq!(
        reply("dial work", edge),
        "ERR denied: dial is owner-only\n",
        "an edge cannot dial out, refused in the owner-only verbs' words"
    );
    // An EMPTY name is rejected (the name is required).
    assert_eq!(
        reply("dial   ", Scope::Owner),
        DIAL_USAGE,
        "an empty name is rejected"
    );
    // `dial <name> <verb...>` is the one-shot remote-verb form: the name+verb
    // is ACCEPTED (no grammar rejection) and attempts a relay, which fails here
    // because no saved connection is named — an `ERR dial ...`, not a usage error.
    // Against an EMPTY config: the caller's own aterm.toml may name a `work`.
    let config = aterm_tempfile::Builder::new()
        .prefix("atc-dial-cfg-")
        .tempdir()
        .expect("a scratch XDG_CONFIG_HOME");
    let with_verb = crate::test_env::scoped("XDG_CONFIG_HOME", config.path(), || {
        reply("dial work text", Scope::Owner)
    });
    assert!(
        with_verb.contains("dial") && !with_verb.contains("connection name"),
        "name+verb is a dial attempt, not a usage rejection: {with_verb:?}"
    );
    // A non-`dial <name>` line is NOT a relay verb (false => fall through).
    let (_c, s) = CtlStream::pair().unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    assert!(!try_net_dial("screen", Scope::Owner, &s, &mut r));
    assert!(
        !try_net_dial("dial-list", Scope::Owner, &s, &mut r),
        "dial-list is not a relay verb"
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
        "dial work",
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
        "dial",
        "dial-list",
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

/// The open-connection cap follows the descriptor limit: a quarter of the soft limit,
/// within `[CONTROL_WORKERS, CONTROL_CONNECTIONS_MAX]`. The launchd default (256),
/// which a Dock-launched window runs with, is the case that matters
/// (`control_lanes_live_headless.rs` drives it).
#[test]
fn the_connection_cap_leaves_most_descriptors_to_the_rest_of_the_window() {
    assert_eq!(connections_cap_for(Some(256)), 64, "launchd's default");
    assert_eq!(connections_cap_for(Some(2_048)), 512);
    assert_eq!(connections_cap_for(Some(10_240)), CONTROL_CONNECTIONS_MAX);
    assert_eq!(
        connections_cap_for(Some(u64::MAX)),
        CONTROL_CONNECTIONS_MAX,
        "RLIM_INFINITY"
    );
    assert_eq!(
        connections_cap_for(Some(20)),
        CONTROL_WORKERS,
        "never below what the fixed lanes held"
    );
    assert_eq!(connections_cap_for(None), CONTROL_WORKERS, "unreadable");
    #[cfg(unix)]
    {
        let cap = control_connections_cap();
        assert!((CONTROL_WORKERS..=CONTROL_CONNECTIONS_MAX).contains(&cap));
        assert_eq!(cap, connections_cap_for(soft_descriptor_limit()));
        assert!(soft_descriptor_limit().is_some_and(|soft| soft > 0));
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

/// Push capacity is isolated and lane-exact: four long-lived subscribers own
/// four workers, the FIFTH gets a prompt explicit error rather than waiting
/// behind them without an ACK, and completion restores one admission.
#[test]
fn fifth_subscription_is_promptly_rejected_and_capacity_recovers() {
    use std::io::BufRead;

    let subscriptions = SubscriptionDispatch::new();
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

/// `dial-token` validates arity + that the token is real hex BEFORE writing to
/// the Keychain / a file, so a malformed call has no side effect.
#[test]
fn dial_token_verb_validates_usage_and_hex_without_side_effects() {
    assert!(cmd_dial_token("").starts_with("ERR usage"));
    assert!(cmd_dial_token("only-name").starts_with("ERR usage"));
    assert!(cmd_dial_token("a b c").starts_with("ERR usage"));
    // Bad token hex is rejected (no store attempted).
    let r = cmd_dial_token("work not-64-hex");
    assert!(
        r.starts_with("ERR") && !r.contains("OK"),
        "bad hex rejected: {r}"
    );
}

/// The `key` / `ctrl` grammar, one labelled row per input line.
///
/// * `key ctrl+<c>` and `ctrl <c>` build the identical event, so both write the
///   same control byte through the same seam (`ctrl+u` -> 0x15).
/// * The full `NamedKey` vocabulary is reachable (numpad, F13–F35, media/audio,
///   modifier sides, system keys), so a controller can press any physical key.
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
        // The full named vocabulary.
        ("space".into(), named(Nk::Space)),
        ("capslock".into(), named(Nk::CapsLock)),
        ("menu".into(), named(Nk::ContextMenu)),
        ("contextmenu".into(), named(Nk::ContextMenu)),
        ("printscreen".into(), named(Nk::PrintScreen)),
        ("f13".into(), named(Nk::F13)),
        ("f35".into(), named(Nk::F35)),
        ("kp0".into(), named(Nk::Numpad0)),
        ("kp9".into(), named(Nk::Numpad9)),
        ("kpdot".into(), named(Nk::NumpadDecimal)),
        ("kpenter".into(), named(Nk::NumpadEnter)),
        ("kpadd".into(), named(Nk::NumpadAdd)),
        ("kpbegin".into(), named(Nk::NumpadBegin)),
        ("shiftleft".into(), named(Nk::ShiftLeft)),
        ("metaright".into(), named(Nk::MetaRight)),
        ("hyperleft".into(), named(Nk::HyperLeft)),
        ("mediaplaypause".into(), named(Nk::MediaPlayPause)),
        ("volumeup".into(), named(Nk::AudioVolumeUp)),
        ("mute".into(), named(Nk::AudioVolumeMute)),
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
///   are the control: every known name, and the empty list, still parse.
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
    // ...and every name the table knows, and the empty list, still parse.
    for good in [
        "a mods=shift",
        "a mods=ctrl",
        "a mods=control",
        "a mods=alt",
        "a mods=option",
        "a mods=meta",
        "a mods=hyper",
        "a mods=super",
        "a mods=cmd",
        "a mods=command",
        "a mods=ctrl+shift+alt",
        "a mods=cmd,alt",
        "a mods=",
        "a",
    ] {
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

/// `send` is byte-faithful: interior whitespace is NOT collapsed (the line
/// decoder / dispatcher preserve the tail verbatim).
#[test]
fn send_bytes_preserves_whitespace() {
    assert_eq!(send_bytes("a   b\tc"), b"a   b\tc");
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
    // Every relay holds its lane for as long as it lives, so each line that
    // forwards is a wait (`is_wait_request`), never request-lane work.
    for relayed in [&line, &kline, &sline, &spaced] {
        assert!(
            is_wait_request(relayed, Scope::Owner, &store),
            "{relayed:?} is relayed, so it is a wait"
        );
    }

    // Edge scope cannot escalate to a child.
    assert!(
        proxy_forward_plan(&line, Scope::Edge(EdgeToken::generate()), &store, &dir).is_none(),
        "edge scope must not forward",
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
/// locally hosted sid, self-dial (a stale entry naming our own socket), dead
/// listener, unreadable token, non-op verbs. The recorded self socket is
/// process-global, so all self-sock assertions live in this one test.
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

    // A write verb forwards identically (no per-op token split on this hop —
    // the sibling's own gate classifies the op).
    let kline = format!("@{} send hi there", sid.as_str());
    let kplan = proxy_forward_plan(&kline, Scope::Owner, &store, &dir).expect("write forwards");
    assert_eq!(
        presented(&kplan),
        format!("TOKEN cafe0123 @{} send hi there\n", sid.as_str())
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
    // A relay holds its lane for as long as it lives (a subscription's, for
    // `subscribe`), so each line that forwards is a wait, never request-lane
    // work that eight watchers of another window's sessions would fill.
    for relayed in [&line, &kline, &sline, &pline] {
        assert!(
            is_wait_request(relayed, Scope::Owner, &store),
            "{relayed:?} is relayed, so it is a wait"
        );
    }

    // `@*` is this instance's live target set (the bridge's and fleet's
    // subscription), never one session: an entry named `*` relays it nowhere.
    crate::proxy::write_graph_entry(&dir, &SessionId::new("*"), &sib_sock_str, &nonce);
    let all = "subscribe @* events,sessions";
    assert!(
        proxy_forward_plan(all, Scope::Owner, &store, &dir).is_none(),
        "`subscribe @*` stays local"
    );
    assert!(!is_wait_request(all, Scope::Owner, &store));

    // Edge scope must NOT reach a sibling, so its line is no wait either.
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    for relayed in [&line, &sline] {
        assert!(
            proxy_forward_plan(relayed, edge, &store, &dir).is_none(),
            "edge scope must not forward to a sibling",
        );
        assert!(!is_wait_request(relayed, edge, &store));
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

    // Self-dial guard: an entry naming our own socket (a stale record of a session
    // we no longer host) is refused. The raw path is recorded as production does;
    // on macOS the temp dir sits behind `/var` → `/private/var`, so this also covers
    // a raw recorded path vs a canonicalized dial path.
    crate::proxy::set_self_sock(&dir, &sib_sock_str);
    let stale = SessionId::generate();
    crate::proxy::write_graph_entry(&dir, &stale, &sib_sock_str, &nonce);
    let stale_line = format!("@{} text", stale.as_str());
    assert!(
        proxy_forward_plan(&stale_line, Scope::Owner, &store, &dir).is_none(),
        "an entry naming our own socket must never self-dial",
    );
    crate::proxy::clear_self_sock();

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
/// verb class; dropping the subscribe forward arm, or letting a forwardable verb
/// fall to the local path, fails it.
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
    // `subscribe` is selector-SECOND; it MUST forward (read-side) — the exact M1
    // case the generic @-first parse missed. (Not in the loop: different grammar.)
    let sub = format!("subscribe @{sid} cells,bytes");
    assert!(
        proxy_forward_plan(&sub, Scope::Owner, &store, &dir).is_some(),
        "subscribe @<child> must forward (the M1 regression guard)"
    );
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
/// `aterm_spec::derive::no_transitive_authority_model` (`NoTransitiveAuthority`): a
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
    let model = aterm_spec::derive::no_transitive_authority_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let init = model.init_state();
    let mut buggy_rejections = 0;
    for line in &lines {
        let first_line = presented(
            &proxy_forward_plan(line, Scope::Owner, &store, &dir)
                .unwrap_or_else(|| panic!("`{line}`: the Owner must reach a registered child")),
        );
        let (arrived_scope, _) = first_line_scope(
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

/// Tier-1 conformance of the child hop to `aterm_spec::derive::proxy_forward_model`
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
    let model = aterm_spec::derive::proxy_forward_model();
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

/// Tier-1 conformance of the sibling hop to `aterm_spec::derive::proxy_forward_model`
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

    let model = aterm_spec::derive::proxy_forward_model();
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
/// `aterm_spec::derive::forward_handshake_model` (`ForwardHandshake`), its liveness
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

    let model = aterm_spec::derive::forward_handshake_model();
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
    // ...CONTROL: the three the report CAN carry, their aliases, and none.
    for good in [
        "press left 4 4 mods=shift+ctrl+alt",
        "press left 4 4 mods=meta",
        "press left 4 4 mods=option",
        "press left 4 4 mods=control",
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
fn zero_terminal_owner_app_and_meta_classify_before_session_resolution() {
    use aterm_types::control_verbs::{Access, Target};

    let active: ActiveHandle = Arc::new(Mutex::new(None));
    let store = crate::session_store::new_store();
    let subscribers = crate::subscribe::new_registry();
    assert!(resolve_active(&active).is_none());
    assert_eq!(
        control_session::cmd_sessions_store(&store, None),
        "OK 0\n",
        "Owner fleet meta remains truthful with an empty SessionStore"
    );
    assert_eq!(control_session::cmd_who(&store, &subscribers), "OK 0\n");

    let (selector, verb, rest) = request_head("open app settings /about");
    assert!(selector.is_none());
    assert_eq!(verb, "open");
    assert_eq!(rest, "app settings /about");
    let open = aterm_types::control_verbs::spec(verb).expect("open verb");
    assert_eq!(open.target, Target::App);
    assert_eq!(open.access, Access::Scoped);

    let text = aterm_types::control_verbs::spec("text").expect("legacy text verb");
    assert_eq!(text.target, Target::Session);
    assert_eq!(NO_ACTIVE_TERMINAL, "ERR no active terminal\n");
    assert!(
        resolve_explicit(&store, &Selector::Local(7))
            .expect("a local id is never ambiguous")
            .is_none(),
        "an explicit unknown PTY never falls back to hidden front content"
    );
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
    assert_eq!(
        sessionless_front_window_resize(
            "resize",
            "px 420",
            None,
            false,
            NativeControlPrincipal::Owner,
        ),
        Some(Err("ERR usage: resize px <w> <h>\n".to_string())),
        "a malformed window request is told how to spell itself, not handed to a resolver",
    );
    // …and an UNSPELLABLE one keeps the parser's other answer. Which tab is in
    // front is not a reason for one request to be told two different things.
    assert_eq!(
        sessionless_front_window_resize(
            "resize",
            "px wide tall",
            None,
            false,
            NativeControlPrincipal::Owner,
        ),
        Some(Err("ERR bad args\n".to_string())),
        "a native front must answer the same as a terminal front does",
    );
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

#[test]
fn shipping_front_paste_event_drives_editor_minibuffer_then_buffer() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-control-front-paste-{}-{:?}",
        std::process::id(),
        std::thread::current().id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("front-paste.md");
    std::fs::write(&path, "base\n").unwrap();
    app.open_document_tab(
        crate::native_app::AppKind::Editor,
        // The shipping encoder: a hand-rolled `format!` is malformed on Windows
        // (drive letter and backslashes after the authority slot).
        &crate::native_document_host::path_to_file_uri(&path).unwrap(),
    )
    .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();

    app.dispatch_native_event(
        wid,
        crate::native_app::AppEvent::EditorCommand(
            crate::native_editor::EditorCommand::IncrementalSearch,
        ),
    )
    .unwrap();
    let search_paste = sessionless_front_paste_event(
        "paste",
        || "base".to_string(),
        None,
        false,
        NativeControlPrincipal::Owner,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        app.input(wid, search_paste, Source::Controller,),
        InputOutcome::Ok,
    );
    let Some(crate::native_app::AppViewState::Editor(state)) = app.native_runtime.view_state(view)
    else {
        panic!("editor view remains live");
    };
    assert!(matches!(
        state.buffer.as_ref().unwrap().minibuffer,
        crate::native_editor::Minibuffer::Search { ref query, .. } if query == "base"
    ));
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "base\n",
        "a focused minibuffer owns the control paste",
    );

    assert_eq!(
        app.input(
            wid,
            InputEvent::Key {
                key: aterm_types::keyboard::Key::Named(aterm_types::keyboard::NamedKey::Escape,),
                mods: aterm_types::keyboard::Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            },
            Source::Controller,
        ),
        InputOutcome::Ok,
    );
    let buffer_paste = sessionless_front_paste_event(
        "paste",
        || "Paragraph ".to_string(),
        Some(&Selector::SelfTok),
        false,
        NativeControlPrincipal::Owner,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        app.input(wid, buffer_paste, Source::Controller,),
        InputOutcome::Ok,
    );
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "Paragraph base\n",
        "without a minibuffer the active editor buffer owns the paste",
    );
    let _ = std::fs::remove_dir_all(dir);
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
    (session_on(local_id, wr), rx)
}

/// [`pipe_session`]'s session, writing to `master`.
#[cfg(unix)]
fn session_on(local_id: u64, master: i32) -> crate::session_store::SessionHandle {
    use crate::session_store::{SessionHandle, SessionState};
    use aterm_session::sink::SinkWriter;
    use aterm_session::{EdgeTable, LaunchNonce, SessionId};
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let ctx = Arc::new(crate::SessionCtx {
        sink: Arc::new(SinkWriter::new(master)),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: crate::mode_mirror_of(&term),
        ui_waiting: Arc::default(),
        edges: std::sync::Mutex::new(EdgeTable::new()),
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
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        generation_look: Default::default(),
        reset_lane: Default::default(),
        update_parked: Default::default(),
        fenced_writes: Default::default(),
    });
    SessionHandle {
        sid,
        nonce,
        local_id,
        parent: None,
        state: SessionState::Alive,
        title: String::new(),
        term,
        master,
        ctx,
        identity: None,
    }
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
/// `EPIPE` cannot be undone. `a_stranger_holding_the_read_end_cannot_revive_a_kernel_killed_peer`
/// pins both halves.
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
    assert!(reply.starts_with("OK "), "{reply}");
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

/// A wedged target must not make the operator's foreground `turn` exceed its bound
/// before the watcher deadline starts. The ordinary cross-input path is
/// backpressured; the operator path (`operator_input_if_epoch`) uses the immediate
/// sink result, stops before Enter, and leaves no rejected paste in the spill
/// drainer to arrive after the call returns.
#[test]
#[cfg(unix)]
fn operator_turn_refuses_full_pty_promptly_without_late_input() {
    use aterm_agent::operator::EventGeneration;
    use std::io::Read as _;

    let store = crate::session_store::new_store();
    let (handle, mut reader) = pipe_session(71);
    store.write().unwrap().register(handle.clone());
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

    // The screen the operator validated: unchanged, so only the full PTY can
    // refuse the paste.
    let generation = {
        let terminal = term_lock(&handle.term);
        let evidence = crate::operator_host::terminal_evidence(&terminal);
        EventGeneration::new(
            0,
            terminal.is_alternate_screen(),
            terminal.content_seq(),
            Sha256::digest(evidence.as_bytes()),
        )
    };
    let paste = |text: &str| {
        let delivery = operator_input_if_epoch(
            &handle.term,
            &handle.ctx,
            Some(InputEvent::Paste(
                control_input::paste_text(text),
                PasteFraming::AtDrain,
            )),
            handle.ctx.sink.input_epoch(),
            OperatorTerminalFence::Exact(generation),
        );
        assert!(
            matches!(delivery, Delivery::BusyZero),
            "a full PTY refuses with zero bytes, not a fence conflict: {delivery:?}"
        );
        false
    };
    let press = |_: &str| panic!("BusyZero paste must stop before Enter");
    let started = std::time::Instant::now();
    let response = control_session::cmd_turn(
        &handle.term,
        &store,
        handle.local_id,
        // The regression waits out this turn timeout, the verb's ten-minute
        // ceiling, so the minute's hang detector below tells it apart from
        // a correct tree however loaded the machine (the load-sensitive
        // test audit of 2026-09-27: 1 s, then a third of a 30 s timeout).
        "timeout=600000 -- REJECTED",
        &subscribe::new_registry(),
        &handle.ctx,
        &control_session::TurnIo {
            paste: &paste,
            press: &press,
            ..control_session::TurnIo::paste_only()
        },
    );
    assert_eq!(response, "ERR paste delivery failed\n");
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "foreground turn waited behind a full PTY: {:?}",
        started.elapsed()
    );

    let mut old = vec![0_u8; filled];
    reader.read_exact(&mut old).expect("drain original fill");
    assert!(old.iter().all(|byte| *byte == b'.'));
    assert!(
        drain_pipe(&reader).is_empty(),
        "the refused operator paste must not arrive after the turn returned"
    );
}

#[test]
fn operator_input_diagnostics_retain_zero_vs_partial_and_stage() {
    let paste_zero = OperatorInputFailure {
        stage: OperatorInputStage::Paste,
        delivery: Delivery::BusyZero,
    }
    .response();
    assert_eq!(
        operator_result_summary(&paste_zero),
        "turn input=paste outcome=busy-zero"
    );

    let submit_partial = OperatorInputFailure {
        stage: OperatorInputStage::Submit,
        delivery: Delivery::PartialInDoubt { accepted: 1 },
    }
    .response();
    assert_eq!(
        operator_result_summary(&submit_partial),
        "turn input=submit outcome=partial accepted=1"
    );
    assert_eq!(
        operator_result_summary("ERR operator input submit partial accepted=1 leaked"),
        "turn result unavailable",
        "non-canonical fields must not enter the durable summary"
    );
}

/// A cross-session input verb resolves its `@<selector>` target like `send`/`feed`
/// and drives the source-blind seam against that session: bytes land in the
/// target's sink, never self's, and the self path is unchanged. Covers
/// `key`/`feed`/`select`/`send`.
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

    let self_tuple: Target = (
        h_self.term.clone(),
        h_self.master,
        h_self.local_id,
        h_self.ctx.clone(),
    );

    // Resolve `@2` (the target's local id) EXACTLY as `handle()` does.
    let sel = Selector::parse("2");
    assert!(matches!(sel, Selector::Local(2)), "@2 parses to Local(2)");
    let (term, master, session, ctx) =
        resolve_target(&self_tuple, &store, &sel).expect("@2 resolves");
    assert!(
        Arc::ptr_eq(&term, &h_target.term),
        "resolved the TARGET term"
    );
    assert_eq!(session, 2);
    let _ = master;
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

    // `select` has no `is_cross` guard: it mutates the resolved `term`'s selection
    // and repaints by the resolved `session` id. A proxy-less `GuiHost` drives the
    // real verb against the resolved target and reads it back with `cmd_selection`,
    // showing the target is the one selected and no pty bytes are emitted.
    let reg = subscribe::new_registry();
    let host = GuiHost::new(session, &term, None, &reg);
    term_lock(&term).process(b"hello world");
    assert_eq!(cmd_select(&host, session, "0 0 0 4"), "OK\n");
    let reply = cmd_selection(&host, session);
    assert!(
        reply.starts_with("OK ") && reply.contains("hello"),
        "TARGET selection: {reply}"
    );
    assert!(
        drain_pipe(&target_rx).is_empty(),
        "select must not write pty bytes"
    );
    assert_eq!(session, 2, "select repaints by the RESOLVED target id");
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
    use aterm_session::sink::SinkWriter;
    use aterm_session::{EdgeTable, LaunchNonce, SessionId};

    // A REAL pty pair so `aterm_pty::resize`'s TIOCSWINSZ actually takes effect
    // (a plain pipe is not a tty and the ioctl would no-op).
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let ctx = Arc::new(crate::SessionCtx {
        sink: Arc::new(SinkWriter::new(master)),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: crate::mode_mirror_of(&term),
        ui_waiting: Arc::default(),
        edges: std::sync::Mutex::new(EdgeTable::new()),
        turn_lease: std::sync::Mutex::new(None),
        self_id: sid,
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
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        generation_look: Default::default(),
        reset_lane: Default::default(),
        update_parked: Default::default(),
        fenced_writes: Default::default(),
    });
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

/// The bytes the `paste` verb puts on the wire: the seam's `format_paste` over the
/// verb's `paste_text(rest)`. The verb posts `InputEvent::Paste`, which a headless
/// test cannot drive, so this reproduces the seam's output.
fn paste_to_pipe(term: &Arc<Mutex<Terminal>>, rest: &str) -> Vec<u8> {
    term_lock(term).format_paste(&paste_text(rest))
}

/// The literal trailing `\n` still ends the paste with a line break,
/// which the engine sends as CR exactly like a real paste.
#[test]
fn paste_verb_trailing_newline_becomes_cr() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    assert_eq!(paste_to_pipe(&term, "echo hi\\n"), b"echo hi\r");
}

/// Mint a fresh `SessionCtx` for the auth/gate tests (no real PTY needed; the
/// sink wraps a harmless `-1` fd and is never written by these tests).
fn test_ctx() -> Arc<crate::SessionCtx> {
    use aterm_session::sink::SinkWriter;
    use aterm_session::{EdgeTable, LaunchNonce, SessionId};
    Arc::new(crate::SessionCtx {
        sink: Arc::new(SinkWriter::new(-1)),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: Arc::default(),
        ui_waiting: Arc::default(),
        edges: std::sync::Mutex::new(EdgeTable::new()),
        turn_lease: std::sync::Mutex::new(None),
        self_id: SessionId::generate(),
        nonce: LaunchNonce::generate(),
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
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        generation_look: Default::default(),
        reset_lane: Default::default(),
        update_parked: Default::default(),
        fenced_writes: Default::default(),
    })
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

/// Every verb the dispatch router handles is in [`VERBS`], so none can be added
/// without being classified (and, by part 2, documented). Scrapes the verb string
/// literals out of the dispatch `match` arms and checks each resolves in the table.
#[test]
fn every_dispatched_verb_is_in_the_table() {
    let src = include_str!("control.rs");
    // The dispatch match body: anchor on the router's `match verb {` inside
    // `pty_idem::guarded(…)`, closed by `});`. The first bare `    match verb {` in
    // this file is `escalated_op`'s, not the router's, so keep the anchor and the
    // `\n    });\n` terminator exact; the sentinel loop below checks the scrape is
    // not empty.
    let body = src
        .split_once("idem_key.as_deref(), || match verb {")
        .and_then(|(_, r)| r.split_once("\n    });\n"))
        .map(|(m, _)| m)
        .expect("dispatch match body");
    // Non-vacuity: the router body must actually contain known dispatch arms.
    // Without this, a future anchor drift silently empties the scrape again.
    for sentinel in [
        "\"text\" =>",
        "\"screen\" =>",
        "\"cursor\" =>",
        // The router's LAST arm: proves the scrape reached the end of the
        // match, not just its first few rows.
        "ERR unknown verb (try: help)",
    ] {
        assert!(
            body.contains(sentinel),
            "dispatch scrape lost its anchor — {sentinel} not found in the \
             scraped body (the router match moved or was renamed)"
        );
    }
    let known = |t: &str| aterm_types::control_verbs::spec(t).is_some();
    // Arms `"x" if …` / `"x" =>` / `"x" | "y"`. Sub-forms like `image read` /
    // `cast frames` match on their base verb, so checking each quoted lowercase arm
    // token for table membership is exact.
    for arm in body.split("=>") {
        // Only the LEFT of each `=>` holds the arm's verb literals.
        for tok in arm.split('"').skip(1).step_by(2) {
            // A verb literal is lowercase-with-dashes and not an argument word.
            let is_sub_keyword = matches!(tok, "read" | "frames" | "set" | "unset" | "new");
            if tok.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                && !tok.is_empty()
                && !is_sub_keyword
                && !known(tok)
                // Only assert on tokens that are an actual dispatch arm (`"x" =>`),
                // not arg keywords that merely appear inside a guard expression.
                && body.contains(&format!("\"{tok}\" =>"))
            {
                panic!("dispatch verb {tok:?} is missing from the VERBS table");
            }
        }
    }
}

/// Every `cmd_<verb>_json` handler has its verb in
/// [`aterm_types::control_verbs::JSON_CAPABLE_VERBS`], and vice versa.
///
/// JSON capability is a property of the server's handler, not of the table row, so
/// the list is a hand-maintained duplicate. If a handler writes the `json_ok`
/// `OK 1\n<body>` (Lines) shape while the table frames the verb as Status, the
/// client consumes the header and drops the body.
///
/// Scrapes handler names (`cmd_<verb>_json`), a stable contract, rather than
/// `json_ok` call sites; irregular names declare their verb in `IRREGULAR`.
#[test]
fn json_ok_sites_match_the_json_capable_verbs() {
    // Handlers whose name does not literally contain their verb.
    const IRREGULAR: &[(&str, &str)] = &[
        ("cmd_screen_styled_json", "screen"),
        // `edges`/`grants` are aliases served by ONE handler.
        ("cmd_edges_json", "edges"),
    ];
    // `*_json` helpers that are serializers, not verb handlers.
    const NOT_HANDLERS: &[&str] = &[
        "serialize_dims_json",
        "styled_image_json",
        // Sub-object of the styled frame (`screen`/`cells`), like the two
        // above — there is no `styled_selection` verb.
        "styled_selection_json",
        "write_styled_cell_json",
    ];

    let sources = [
        include_str!("control_query.rs"),
        include_str!("control_session.rs"),
        // `cmd_privacy_json` lives with the rest of the consent posture.
        include_str!("control_privacy.rs"),
        // Selection verbs live in `aterm-control`; scrape there too, or `blocks`
        // reads as unserved and the non-vacuity floor below weakens.
        include_str!("../../aterm-control/src/selection.rs"),
    ];
    let mut found: Vec<String> = Vec::new();
    for src in sources {
        for line in src.lines() {
            let Some(rest) = line.split_once("fn ").map(|(_, r)| r) else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '_' || c.is_ascii_digit())
                .collect();
            if name.ends_with("_json") && !NOT_HANDLERS.contains(&name.as_str()) {
                found.push(name);
            }
        }
    }
    found.sort();
    found.dedup();
    assert!(
        found.len() >= 6,
        "scrape found only {found:?} — the `fn <name>_json` shape moved and this              guard went vacuous"
    );

    for name in &found {
        let verb = IRREGULAR
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| (*v).to_string())
            .unwrap_or_else(|| {
                name.trim_start_matches("cmd_")
                    .trim_end_matches("_json")
                    .to_string()
            });
        assert!(
            aterm_types::control_verbs::JSON_CAPABLE_VERBS.contains(&verb.as_str()),
            "handler {name} serves verb {verb:?}, which is NOT in JSON_CAPABLE_VERBS \u{2014}                  `{verb} --json` will frame as Status and the client will drop the body"
        );
    }

    // …and nothing is listed that no handler serves (a stale entry would make
    // a Status-framed verb claim Lines under --json).
    for v in aterm_types::control_verbs::JSON_CAPABLE_VERBS {
        let served = *v == "grants" // alias of `edges`
            || found.iter().any(|n| {
                IRREGULAR.iter().any(|(hn, hv)| hn == n && hv == v)
                    || n.trim_start_matches("cmd_").trim_end_matches("_json") == *v
            });
        assert!(
            served,
            "JSON_CAPABLE_VERBS lists {v:?} but no `*_json` handler serves it"
        );
    }
}

/// `required_op` is the single source of truth for which `Op` each verb needs;
/// the design 7.2 read != write != signal split must hold exactly. TOTAL binding:
/// EVERY verb in [`VERBS`] is partitioned by the `Op` `required_op` returns and each
/// partition is pinned to an explicit expected set (below). So no verb can SILENTLY
/// change op-class: shifting a verb's class moves it between buckets and fails a set
/// assert; adding a table verb without listing it here fails the exhaustiveness
/// count. This closes the gap where video/history/metrics/hover/
/// ready/await/subscribe/family/edges/grants were previously unpinned.
#[test]
fn required_op_classifies_each_verb() {
    use aterm_types::control_verbs::VERBS;
    // The verbs of the table whose `required_op` is exactly `want`, sorted.
    let by_op = |want: Option<Op>| -> Vec<&'static str> {
        let mut v: Vec<&str> = VERBS
            .iter()
            .map(|s| s.name)
            .filter(|n| required_op(n) == want)
            .collect();
        v.sort_unstable();
        v
    };
    let assert_set = |want: Option<Op>, mut expected: Vec<&'static str>, label: &str| {
        expected.sort_unstable();
        assert_eq!(by_op(want), expected, "{label}");
    };

    // Read side (`ReadScreen`): every observer plus the controller's own view-state
    // controls (scroll/select move the viewport; nothing leaves the process). `who`
    // is Read-op but Owner-gated in dispatch (op-class ⟂ scope gate); `copy` is not
    // here, it is the clipboard boundary (ClipboardWrite).
    assert_set(
        Some(Op::ReadScreen),
        vec![
            "text",
            "screen",
            "line",
            "lines",
            // `offscreen` reads rows the screen already showed, so it is a screen
            // read like `text`/`cast`.
            "offscreen",
            "cell",
            "cursor",
            "dims",
            "modes",
            "custody",
            "title",
            "cwd",
            "colors",
            // `theme` / `theme list` read the live theme; the preview and
            // write sub-forms are their own rows below.
            "theme",
            "search",
            // `find` drives the bar `search` answers without; Read like
            // `scroll`/`select`, since find mode diverts keystrokes away from the
            // PTY and moves only viewport and highlight. Its reply is the match
            // position `search` already answers.
            "find",
            "selection",
            // `harness policy` reads the session's `[harness]` policy (an
            // edge reads only `effective=`).
            "harness",
            "blocks",
            "blocktext",
            "image",
            "window",
            "video",
            "chrome",
            // Instance state: reports what aterm has done on its own initiative
            // and starts nothing.
            "appstatus",
            // Instance state, read-op like every observer: the message log
            // as text; it presses, raises and ends nothing.
            "messages",
            "panes",
            "controls",
            "inspect",
            "cast",
            "temporal",
            "history",
            // `meta` is base-Read; its `set`/`unset` sub-forms escalate to
            // WriteInput via `escalated_op`.
            "meta",
            "timeline",
            // `log` and `brief` read the session log under the timeline's
            // lock; `brief all` escalates to Owner via `escalated_op`.
            "log",
            "brief",
            // `resizes` OBSERVES the resize ledger: the engine's journal and
            // the verdicts read from it. It resizes nothing.
            "resizes",
            // `status` observes the local status classifier and has no write form
            // (a status moves only with the session and `settings set tab_status`),
            // so it takes no `escalated_op` entry.
            "status",
            "metrics",
            // `streak` observes the prism wake engine and its gates; its knobs are
            // written through `settings`.
            "streak",
            // `tone` OBSERVES the mood classifier; the knob it reports is
            // rewritten through `settings` (ConfigWrite), never here.
            "tone",
            // `trail` observes the cursor-trail engine (the licence verdict ring and,
            // via `trail status`, its standing state); no write form, knobs go through
            // `settings`.
            "trail",
            "scroll",
            "select",
            "ready",
            "await",
            "wait",
            // `privacy` observes the macOS consent posture; each fact is one `status`
            // already exposes per session, so it adds no authority. Not Owner-gated,
            // so a supervising agent can read it.
            "privacy",
            "subscribe",
            "who",
            "family",
            "edges",
            "grants",
            // The fabric inbox is a read surface (`inbox` lists rows, `inbox get`
            // returns a body): no PTY moves and no bus trace. `inbox seen` is the
            // write half below.
            "inbox",
            "inbox get",
            // The fabric outbox is the same read class for outgoing mail; the
            // orthogonal `BridgeOnly` scope gate keeps edges and Owner out.
            "outbox",
        ],
        "ReadScreen set (every observer/view-state verb, incl. Owner-gated `who`)",
    );
    // Write side (`WriteInput`): the human input vocabulary and app-drive verbs.
    // `open` pops an aux surface and `invoke` fires a menu action; their
    // clipboard/config escalations are fenced by `escalated_op`.
    assert_set(
        Some(Op::WriteInput),
        vec![
            "send",
            "key",
            "hwkey",
            "ctrl",
            "feed",
            "feed-bin",
            "paste-bin",
            "mouse",
            // `pointer` moves the window's pointer, which a DEC 1000/1002/1003 app
            // receives as motion reports: `mouse`'s class, via the winit entry point.
            "pointer",
            "paste",
            "resize",
            "focus",
            // `mainscreen` moves the screen the program's next byte lands on (not
            // view state like `scroll`), so a read-only edge must not reach it.
            "mainscreen",
            "tab",
            // `pane` chooses which pane the keyboard drives; re-aiming the human's
            // next keystroke is beyond a read-only edge.
            "pane",
            "open",
            "invoke",
            // `confirm yes` runs the close an `invoke` parked in the window:
            // `invoke`'s own class, since it finishes `invoke`'s gesture.
            "confirm",
            "rain",
            // `theme preview|revert`: the transient theme lane, runtime-only
            // like `rain` (nothing durable is written).
            "theme preview",
            // `kitty wear` changes the user's own cursor and stamps the durable
            // collection: not `ReadScreen` (it changes something), not `ConfigWrite`
            // (reserved for `aterm.toml` and security knobs); the `rain`/`fx` class.
            "kitty",
            // `fx` arms a celebration the session's next keyed edge spends: a runtime
            // latch, no PTY bytes or durable config, like `rain`'s override.
            "fx",
            // `appearance dark|light` (a development build) injects an OS flip that
            // re-themes every session: a runtime override, the `rain`/`fx` class.
            "appearance",
            "hover",
            "turn",
            "spawn",
            "close",
            "lease",
            "act",
            // The fabric write half: none of the five reaches a PTY, but each mutates
            // state a read-only edge must not touch. The bridge verbs are also
            // `Access::BridgeOnly` and `hold` is `OwnerOnly`; op-class and scope gate
            // are orthogonal (as with `who`, a Read verb behind an Owner gate).
            "inbox seen",
            "post",
            "deliver",
            "link",
            "hello",
            "hold",
            "outbox sent",
            // `appnotice` mutates the pull-down, so it is write-class; `OwnerOnly`
            // keeps a child edge from posting one.
            "appnotice",
            // `notice` mutates the band (a row rises, moves, ends or is pressed):
            // write-class, with the orthogonal `OwnerOnly` gate.
            "notice",
            // `story` mutates the presence band's phase slot: write-class, and
            // `OwnerOnly` keeps a child edge from writing `✓ approved`.
            "story",
            // `devtools hand-over` can put System Settings in front of the owner
            // (or aterm's panel beside it): write-class, with the orthogonal
            // `OwnerOnly` gate.
            "devtools",
            // The manual reset mutates terminal modes (and with `flush` drops the
            // input queue): write-class like `resize`, though it writes no PTY input.
            "reset",
        ],
        "WriteInput set (input vocabulary + app-drive verbs)",
    );
    // The signal class, and two fine ops carved out of Read/Write: `settings`
    // rewrites durable config and `copy` exfiltrates to the OS clipboard, neither
    // reachable by an inherited read/write edge.
    // `restart` ends the tab's agent (its stamp first, then SIGTERM, then the
    // act's SIGKILL for a survivor): the signal class, as `signal` is.
    assert_set(Some(Op::Signal), vec!["signal", "restart"], "Signal set");
    // `theme set|commit` rewrite `aterm.toml` through the `settings set` lane.
    assert_set(
        Some(Op::ConfigWrite),
        vec!["settings", "theme set"],
        "ConfigWrite set",
    );
    assert_set(Some(Op::ClipboardWrite), vec!["copy"], "ClipboardWrite set");
    // None (OpClass::Owner): privilege/identity + build-meta verbs, gated before the
    // op check. Every one default-DENIES for an Edge (no op edge reaches them).
    assert_set(
        None,
        vec![
            "version",
            "update",
            "help",
            "verbs",
            // The sudo askpass relay: Owner-class privilege, gated before the op
            // check and intercepted in `serve_request_line`.
            "askpass",
            "operator",
            "operator-propose-bin",
            // The bridge supervisor of a running instance: Owner-only, no op
            // edge reaches it.
            "fabric",
            "sessions",
            // The exit ledger: `sessions`' past, Owner-gated with it.
            "exits",
            // The identity roster: the directories `sessions`' `identity=`
            // names, Owner-gated with it.
            "identities",
            "whoami",
            "grant",
            "revoke",
            // The receiver-side broadcast opt-in is Owner-class: a topic decides
            // what lands in an agent's inbox.
            "topic",
            // The typed re-key: it hands out a key that lets a session's
            // marks through, which no op edge may mint.
            "rekey",
            // Session-connection verbs: connection-grain twins of grant/revoke, the
            // aggregated graph and the raise act, all Owner-only.
            "connect",
            "disconnect",
            "flows",
            "raise",
            "dial",
            "dial-list",
            "dial-token",
        ],
        "None set (Owner-only privilege + build-meta verbs)",
    );

    // Exhaustive: the pinned partitions cover the whole table. Per-class sizes are
    // not written here; the set-equality assertions and `pinned == VERBS.len()` are
    // the check.
    let pinned = by_op(Some(Op::ReadScreen)).len()
        + by_op(Some(Op::WriteInput)).len()
        + by_op(Some(Op::Signal)).len()
        + by_op(Some(Op::ConfigWrite)).len()
        + by_op(Some(Op::ClipboardWrite)).len()
        + by_op(None).len();
    assert_eq!(
        pinned,
        VERBS.len(),
        "every table verb must fall in exactly one pinned op-class partition",
    );

    // `who` is Read-op yet Owner-GATED — the orthogonality this test relies on.
    assert!(
        aterm_types::control_verbs::is_owner_only("who"),
        "who is owner-gated in the table"
    );
    // Unknown verbs default-deny (absent from the table => None).
    assert_eq!(required_op("bogus"), None, "unknown verb default-deny");
    assert_eq!(required_op(""), None, "empty verb default-deny");
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

/// `find`'s mutating forms are gated by a conjunction, never an escalation.
///
/// `escalated_op` replaces the base op, and every `find` form answers with the match
/// position, so escalating to `WriteInput` would drop the read requirement and give
/// a keystroke-only edge a screen read. So `find` escalates for nothing, and the
/// dispatch arm requires write on top of the base read, as `turn` does.
#[test]
fn find_is_gated_by_conjunction_and_never_by_escalation() {
    for rest in [
        "",
        "status",
        "open",
        "next",
        "type secret",
        "accept",
        "cancel",
    ] {
        assert_eq!(
            escalated_op("find", rest),
            None,
            "find {rest:?} must not escalate — escalation would drop its read gate"
        );
    }
}

/// `theme` reads with its base class; its preview lane needs `WriteInput` and
/// the forms that rewrite `aterm.toml` need `ConfigWrite`, whatever follows the
/// sub-form word. Negative control: the read forms add nothing.
#[test]
fn control_theme_sub_forms_escalate_to_the_class_they_need() {
    use Escalation::Op as EOp;
    for rest in ["", "status", "list"] {
        assert_eq!(escalated_op("theme", rest), None, "{rest:?} is a read");
    }
    for rest in ["preview Nord", "preview GitHub Light ttl=5", "revert"] {
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

/// The `invoke` / `open` seams escalate to the fine op their target needs, and the
/// owner-only twins (`OpenPalette`, `SoftwareUpdate`, `ApplyUpdate`, the
/// connected-spawn presets) to `OwnerOnly`.
#[test]
fn escalated_op_fences_invoke_and_open_indirect_seams() {
    use Escalation::{Op as EOp, OwnerOnly};
    // The argument classification the dispatch reads.
    assert_eq!(
        escalated_op("invoke", "Copy"),
        Some(EOp(Op::ClipboardWrite))
    );
    assert_eq!(
        escalated_op("invoke", "Paste"),
        Some(EOp(Op::ClipboardWrite))
    );
    assert_eq!(
        escalated_op("invoke", "SelectAll"),
        Some(EOp(Op::ClipboardWrite))
    );
    assert_eq!(
        escalated_op("invoke", "ToggleSettings"),
        Some(EOp(Op::ConfigWrite))
    );
    assert_eq!(
        escalated_op("invoke", "Preferences"),
        Some(EOp(Op::ConfigWrite))
    );
    // The palette gateway and staged-update re-exec twins are Owner-only; no fine op
    // expresses that, so even a granted fine-op edge is denied.
    assert_eq!(escalated_op("invoke", "OpenPalette"), Some(OwnerOnly));
    assert_eq!(escalated_op("invoke", "SoftwareUpdate"), Some(OwnerOnly));
    assert_eq!(escalated_op("invoke", "ApplyUpdate"), Some(OwnerOnly));
    // The connected-spawn presets mint standing session-connection authority, so
    // their `invoke` twins carry the same OwnerOnly fence as `spawn connected=`.
    assert_eq!(
        escalated_op("invoke", "NewControlledWindow"),
        Some(OwnerOnly)
    );
    assert_eq!(escalated_op("invoke", "NewControlledTab"), Some(OwnerOnly));
    assert_eq!(
        escalated_op("invoke", "NewControllerWindow"),
        Some(OwnerOnly)
    );
    assert_eq!(escalated_op("invoke", "NewControllerTab"), Some(OwnerOnly));
    // The context-menu picker/map rows (§2.3): the `invoke` twins of the
    // `open connections` OwnerOnly arm below.
    assert_eq!(escalated_op("invoke", "ConnectToSession"), Some(OwnerOnly));
    assert_eq!(escalated_op("invoke", "ShowConnectionMap"), Some(OwnerOnly));
    // The configure/disconnect ids rewrite or dissolve standing session-connection
    // authority, fenced like the `connect`/`disconnect` verbs.
    assert_eq!(
        escalated_op("invoke", "ConfigureConnection"),
        Some(OwnerOnly)
    );
    assert_eq!(escalated_op("invoke", "DisconnectSession"), Some(OwnerOnly));
    // A benign action / a benign `open` target does NOT escalate (base gate suffices).
    // Font zoom is runtime-only view state (no config persist).
    assert_eq!(escalated_op("invoke", "NewTab"), None);
    assert_eq!(escalated_op("invoke", "FontIncrease"), None);
    // An UNKNOWN action name does not escalate — the base WriteInput gate applies and
    // `cmd_invoke` rejects the unknown name at execution (harmless: nothing runs).
    assert_eq!(escalated_op("invoke", "TotallyBogusAction"), None);
    assert_eq!(escalated_op("open", "about"), None);
    assert_eq!(escalated_op("open", "perf"), None);
    // `open prefs`/`open settings`/`open preferences` all raise the security-knob
    // overlay => ConfigWrite.
    assert_eq!(escalated_op("open", "prefs"), Some(EOp(Op::ConfigWrite)));
    assert_eq!(escalated_op("open", "settings"), Some(EOp(Op::ConfigWrite)));
    assert_eq!(
        escalated_op("open", "preferences"),
        Some(EOp(Op::ConfigWrite))
    );
    assert_eq!(
        escalated_op("open", "app settings /security"),
        Some(EOp(Op::ConfigWrite)),
        "the versioned native Settings route has the same fine-op fence"
    );
    assert_eq!(
        escalated_op(
            "act",
            "app/v1 view 42 settings/control/allow_osc52_query settings/set/allow_osc52_query true"
        ),
        Some(EOp(Op::ConfigWrite)),
        "semantic Settings actions cannot inherit WriteInput"
    );
    assert_eq!(
        escalated_op("open", "app editor file:///tmp/example.md"),
        None,
        "opening a document retains the established WriteInput class"
    );
    // `open menu`/`open palette` (gateway) and `open update` (re-exec surface) are
    // Owner-only, like their `invoke` twins.
    assert_eq!(escalated_op("open", "menu"), Some(OwnerOnly));
    assert_eq!(escalated_op("open", "palette"), Some(OwnerOnly));
    assert_eq!(escalated_op("open", "update"), Some(OwnerOnly));
    // The `close` variant inherits the open's fence: `cmd_open` acts on the first
    // token, so `open prefs close` / `open menu close` / `open update close` escalate
    // identically and a scoped edge cannot dismiss a privileged overlay it may not
    // open.
    assert_eq!(
        escalated_op("open", "prefs close"),
        Some(EOp(Op::ConfigWrite))
    );
    assert_eq!(
        escalated_op("open", "settings close"),
        Some(EOp(Op::ConfigWrite))
    );
    assert_eq!(escalated_op("open", "menu close"), Some(OwnerOnly));
    assert_eq!(escalated_op("open", "palette close"), Some(OwnerOnly));
    assert_eq!(escalated_op("open", "update close"), Some(OwnerOnly));

    // A connected spawn mints standing cross-session authority, so
    // `spawn connected=…` is Owner-only, while a plain/`cwd=` spawn keeps WriteInput.
    assert_eq!(
        escalated_op("spawn", "connected=controller of=s-abc"),
        Some(OwnerOnly)
    );
    assert_eq!(
        escalated_op("spawn", "connected=controlled place=tab of=s-abc cwd=/tmp"),
        Some(OwnerOnly)
    );
    assert_eq!(escalated_op("spawn", ""), None);
    assert_eq!(escalated_op("spawn", "cwd=/tmp"), None);
    // `identity=` (a name, or the `-` opt-out) opens or declines a login, so it is
    // fenced like `connected=`, in either form, quoted or not, at any position.
    assert_eq!(
        escalated_op("spawn", "identity=worker"),
        Some(OwnerOnly),
        "a named identity is Owner-only"
    );
    assert_eq!(
        escalated_op("spawn", "cwd=/tmp identity=-"),
        Some(OwnerOnly)
    );
    assert_eq!(
        escalated_op("spawn", "\"identity=worker\" cwd=/tmp"),
        Some(OwnerOnly),
        "a quoted identity= must not walk past the Owner fence"
    );
    assert_eq!(
        escalated_op(
            "spawn",
            "connected=controlled place=tab of=s-abc identity=worker"
        ),
        Some(OwnerOnly)
    );

    // The fence classifies with the parser's tokenizer (`split_quoted_tokens`), not
    // a whitespace split: every spelling `parse_spawn_args` accepts as a connected
    // spawn must escalate.
    assert_eq!(
        escalated_op("spawn", "\"connected=controller\" of=s-abc"),
        Some(OwnerOnly),
        "a quoted connected= must not walk past the Owner fence"
    );
    assert_eq!(
        escalated_op("spawn", "of=s-abc \"connected=controlled\" place=tab"),
        Some(OwnerOnly),
        "position must not matter either"
    );
    // Unparseable input is fenced too: the fence is never weaker than the
    // parser, and an unterminated quote reaches the parser as an error.
    assert_eq!(
        escalated_op("spawn", "\"connected=controller"),
        Some(OwnerOnly)
    );
    // Every aggregated-graph surface carries the `flows` Owner gate: the map
    // (`open connections`), its readout (`controls connections`) and its capture
    // (`window connections`), including the `close` dismiss. Benign targets keep
    // their base class.
    assert_eq!(escalated_op("open", "connections"), Some(OwnerOnly));
    assert_eq!(escalated_op("controls", "connections"), Some(OwnerOnly));
    assert_eq!(escalated_op("window", "connections"), Some(OwnerOnly));
    assert_eq!(escalated_op("open", "connections close"), Some(OwnerOnly));
    assert_eq!(escalated_op("controls", "front"), None);
    assert_eq!(escalated_op("window", ""), None);
    assert_eq!(escalated_op("window", "front shot.png"), None);

    // `meta` (base Read) escalates its write sub-forms to WriteInput; the bare/status
    // read does not escalate.
    assert_eq!(
        escalated_op("meta", "set title build agent"),
        Some(EOp(Op::WriteInput))
    );
    assert_eq!(
        escalated_op("meta", "unset icon"),
        Some(EOp(Op::WriteInput))
    );
    // The typed role/attention keys ride the same write escalation.
    assert_eq!(
        escalated_op("meta", "set role operator"),
        Some(EOp(Op::WriteInput))
    );
    assert_eq!(
        escalated_op("meta", "unset attention"),
        Some(EOp(Op::WriteInput))
    );
    assert_eq!(escalated_op("meta", ""), None);
    assert_eq!(escalated_op("timeline", "10 since=3"), None);
    // `log` is a plain read whatever it asks for (`tier=content` included: the
    // OSC title `text` and `timeline` already show at ReadScreen); `brief`
    // reads its target, and `brief all` the whole roster, Owner-only.
    assert_eq!(escalated_op("log", "tier=content --json"), None);
    assert_eq!(escalated_op("brief", "t3 budget=100"), None);
    assert_eq!(escalated_op("brief", "all"), Some(Escalation::OwnerOnly));
    // `all` may stand anywhere among `brief`'s arguments (its parser takes it
    // there), so the gate escalates it there too.
    assert_eq!(
        escalated_op("brief", "budget=100 all"),
        Some(Escalation::OwnerOnly)
    );
    assert_eq!(escalated_op("brief", "budget=100 r1"), None);
    assert_eq!(escalated_op("brief", ""), None);
    // Fail-closed: what the parser does not read as a target's brief is
    // Owner-only (it answers its usage line to the Owner alone).
    assert_eq!(
        escalated_op("brief", "budget=x all"),
        Some(Escalation::OwnerOnly)
    );

    // The full dispatch decision (`dispatch_authorized`): effective op = base op
    // escalated by the argument, Owner short-circuiting. Not the verb-only
    // `gate_allows`: the escalation replaces the base op, so a fine-op edge passes
    // and a WriteInput edge is fenced.
    let dispatch_allows = dispatch_authorized;

    let ctx = test_ctx();
    let owner = Scope::Owner;
    let edge_write = edge_granted(Op::WriteInput, &ctx);
    let edge_clip = edge_granted(Op::ClipboardWrite, &ctx);
    let edge_cfg = edge_granted(Op::ConfigWrite, &ctx);

    // A WriteInput edge may `invoke NewTab` but is denied the clipboard/config
    // escalations and `open prefs`, so it cannot tunnel to ClipboardWrite/ConfigWrite.
    assert!(
        dispatch_allows(edge_write, "invoke", "NewTab", &ctx),
        "write edge: invoke NewTab (benign)"
    );
    assert!(
        !dispatch_allows(edge_write, "invoke", "Copy", &ctx),
        "write edge DENIED invoke Copy (clipboard escalation)"
    );
    assert!(
        !dispatch_allows(edge_write, "invoke", "ToggleSettings", &ctx),
        "write edge DENIED invoke ToggleSettings (config escalation)"
    );
    assert!(
        !dispatch_allows(edge_write, "open", "prefs", &ctx),
        "write edge DENIED open prefs (config escalation)"
    );
    assert!(
        !dispatch_allows(edge_write, "open", "app settings /security", &ctx),
        "write edge DENIED native Settings route"
    );
    assert!(
        !dispatch_allows(
            edge_write,
            "act",
            "app/v1 view 42 settings/control/allow_osc52_query settings/set/allow_osc52_query true",
            &ctx,
        ),
        "write edge DENIED semantic Settings action"
    );
    assert!(
        dispatch_allows(edge_write, "open", "about", &ctx),
        "write edge: open about (benign)"
    );
    // A plain WriteInput edge is denied the privileged MenuActions reachable via
    // `invoke`: the palette gateway, update re-exec twins, settings surface and
    // clipboard exfil.
    for rest in [
        "OpenPalette",
        "ApplyUpdate",
        "SoftwareUpdate",
        "ToggleSettings",
        "Preferences",
        "Copy",
        "Paste",
        "SelectAll",
    ] {
        assert!(
            !dispatch_allows(edge_write, "invoke", rest, &ctx),
            "write edge DENIED invoke {rest}"
        );
    }
    // The palette/update `open` twins are Owner-only too.
    for rest in ["menu", "palette", "update"] {
        assert!(
            !dispatch_allows(edge_write, "open", rest, &ctx),
            "write edge DENIED open {rest} (owner-only surface)"
        );
    }

    // Owner keeps full power over every seam, INCLUDING the owner-only ones.
    for (verb, rest) in [
        ("invoke", "Copy"),
        ("invoke", "ToggleSettings"),
        ("invoke", "OpenPalette"),
        ("invoke", "ApplyUpdate"),
        ("open", "prefs"),
        ("open", "menu"),
        ("open", "update"),
    ] {
        assert!(
            dispatch_allows(owner, verb, rest, &ctx),
            "Owner: {verb} {rest}"
        );
    }

    // An explicitly-granted fine-op edge (Owner minted one via `grant`) works: the
    // fence is about DEFAULT inheritance, not an absolute ban.
    assert!(
        dispatch_allows(edge_clip, "invoke", "Copy", &ctx),
        "explicit clipboard-write edge: invoke Copy"
    );
    assert!(
        dispatch_allows(edge_cfg, "invoke", "ToggleSettings", &ctx),
        "explicit config-write edge: invoke ToggleSettings"
    );
    assert!(
        dispatch_allows(edge_cfg, "open", "prefs", &ctx),
        "explicit config-write edge: open prefs"
    );
    assert!(
        dispatch_allows(edge_cfg, "open", "app settings /security", &ctx),
        "explicit config-write edge: native Settings route"
    );
    assert!(
        dispatch_allows(
            edge_cfg,
            "act",
            "app/v1 view 42 settings/control/allow_osc52_query settings/set/allow_osc52_query true",
            &ctx,
        ),
        "explicit config-write edge: semantic Settings action"
    );
    // Owner-only actions are denied even to a granted fine-op edge.
    assert!(
        !dispatch_allows(edge_cfg, "invoke", "OpenPalette", &ctx),
        "config-write edge STILL DENIED invoke OpenPalette (owner-only)"
    );
    assert!(
        !dispatch_allows(edge_cfg, "invoke", "ApplyUpdate", &ctx),
        "config-write edge STILL DENIED invoke ApplyUpdate (owner-only)"
    );
    assert!(
        !dispatch_allows(edge_clip, "invoke", "OpenPalette", &ctx),
        "clipboard-write edge STILL DENIED invoke OpenPalette (owner-only)"
    );

    // The aggregated-graph surfaces and connected spawn are Owner-only through the
    // full dispatch decision: a write edge (and a granted read edge, for the
    // Read-class controls/window) is denied, while keeping the verb's benign targets.
    let edge_read = edge_granted(Op::ReadScreen, &ctx);
    for (verb, rest) in [
        ("open", "connections"),
        ("controls", "connections"),
        ("window", "connections"),
        ("spawn", "connected=controller of=s-abc"),
        ("brief", "all"),
        // `all` anywhere among the arguments, as `brief`'s parser takes it.
        ("brief", "budget=100 all"),
    ] {
        assert!(
            !dispatch_allows(edge_write, verb, rest, &ctx),
            "write edge DENIED {verb} {rest} (owner-only surface)"
        );
        assert!(
            !dispatch_allows(edge_read, verb, rest, &ctx),
            "read edge DENIED {verb} {rest} (owner-only surface)"
        );
        assert!(
            dispatch_allows(owner, verb, rest, &ctx),
            "Owner: {verb} {rest}"
        );
    }
    assert!(
        dispatch_allows(edge_read, "controls", "front", &ctx),
        "read edge keeps benign controls targets"
    );
    assert!(
        dispatch_allows(edge_read, "brief", "budget=100", &ctx),
        "read edge keeps its own target's brief"
    );
    assert!(
        dispatch_allows(edge_write, "spawn", "cwd=/tmp", &ctx),
        "write edge keeps the plain (non-minting) spawn"
    );
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
    let before = app.native_config_service.snapshot();
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
    assert_eq!(
        app.native_config_service.snapshot().text,
        before.text,
        "denied scoped input cannot mutate config"
    );
    assert_eq!(
        front_drive_escalation(Scope::Owner, "key", app.front_control_surface()),
        None,
        "Owner keeps the established input path"
    );
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

    // The semantic path: `act` on a session's Autopilot button.
    let press = "app/v1 view 42 harness/session/7/autopilot/cruise \
                 harness/session/7/autopilot/cruise";
    assert_eq!(escalated_op("act", press), Some(Escalation::OwnerOnly));
    assert!(
        !dispatch_authorized(config_edge, "act", press, &ctx),
        "a ConfigWrite edge is DENIED a session's Autopilot press"
    );
    assert!(
        dispatch_authorized(owner, "act", press, &ctx),
        "Owner presses"
    );
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
    use super::control_input::{apply_copy_on_select_policy, scope_suppresses_copy_on_select};

    // The predicate: NON-OWNER suppresses, OWNER is exempt.
    assert!(
        scope_suppresses_copy_on_select(edge()),
        "a scoped edge must suppress copy-on-select (exfil fence)"
    );
    assert!(
        !scope_suppresses_copy_on_select(Scope::Owner),
        "Owner (god token / owner automation) keeps copy-on-select"
    );

    // The application onto the event: a scoped-edge release carries the
    // suppression flag; the SAME event from Owner leaves it false.
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

/// An Owner passes every verb (the gate's `(Owner, _)` arm short-circuits before
/// lookup). A `ReadScreen` Edge is denied write/signal/privilege verbs and allowed
/// read-side verbs.
#[test]
fn op_scope_gate_owner_full_power_edge_read_only() {
    // The active session the gate evaluates against; the edges below are GRANTED
    // on it (a real connection's token is authorized against its session).
    let ctx = test_ctx();
    let owner = Scope::Owner;
    let edge_read = edge_granted(Op::ReadScreen, &ctx);

    // Owner: every verb is permitted, including grant/revoke/whoami and image.
    let all_verbs = [
        "text", "image", "scroll", "select", "feed", "signal", "send", "resize", "grant", "revoke",
        "whoami",
    ];
    for v in all_verbs {
        assert!(gate_allows(owner, v, &ctx), "Owner must pass {v}");
    }

    // ReadScreen Edge (granted on ctx): read-side verbs pass; write/signal denied.
    assert!(gate_allows(edge_read, "text", &ctx), "read edge: text");
    // A read edge sees B: the gate admits `image` (its `image read` form
    // serves B's own pixels), but the framebuffer form is a whole window's
    // frame and the dispatch arm refuses it to an edge, naming why.
    assert!(gate_allows(edge_read, "image", &ctx), "read edge: image");
    let refusal = image_frame_refusal(&ctx);
    assert!(
        refusal.starts_with("ERR denied: ") && refusal.contains("edge token"),
        "read edge: framebuffer image refused by name: {refusal}"
    );
    assert!(gate_allows(edge_read, "select", &ctx), "read edge: select");
    assert!(!gate_allows(edge_read, "feed", &ctx), "read edge: NOT feed");
    assert!(
        !gate_allows(edge_read, "signal", &ctx),
        "read edge: NOT signal"
    );
    assert!(!gate_allows(edge_read, "send", &ctx), "read edge: NOT send");

    // No Edge — regardless of op — may grant/revoke/whoami (Owner-only, None-op).
    for op in [Op::ReadScreen, Op::WriteInput, Op::Signal] {
        let e = edge_granted(op, &ctx);
        assert!(!gate_allows(e, "grant", &ctx), "no edge may grant");
        assert!(!gate_allows(e, "revoke", &ctx), "no edge may revoke");
        assert!(!gate_allows(e, "whoami", &ctx), "no edge may whoami");
        assert!(!gate_allows(e, "bogus", &ctx), "no edge: unknown verb");
    }

    // A WriteInput edge mirrors the split: it may write but not read or signal.
    let edge_write = edge_granted(Op::WriteInput, &ctx);
    assert!(gate_allows(edge_write, "feed", &ctx), "write edge: feed");
    assert!(
        !gate_allows(edge_write, "text", &ctx),
        "write edge: NOT read"
    );
    assert!(
        !gate_allows(edge_write, "signal", &ctx),
        "write edge: NOT signal"
    );

    // The fine ops are unreachable by an inherited edge: a WriteInput edge cannot
    // persist config via `settings`, a read edge cannot exfiltrate via `copy`;
    // Owner keeps both.
    assert!(
        !gate_allows(edge_write, "settings", &ctx),
        "write edge: NOT settings (config-write is not carried by a write edge)"
    );
    assert!(
        !gate_allows(edge_read, "copy", &ctx),
        "read edge: NOT copy (clipboard-write is not carried by a read edge)"
    );
    // Neither op-inheritance leaks the OTHER fine op either.
    assert!(
        !gate_allows(edge_read, "settings", &ctx),
        "read edge: NOT settings"
    );
    assert!(
        !gate_allows(edge_write, "copy", &ctx),
        "write edge: NOT copy"
    );
    // The in-session Owner (the user driving their own terminal) keeps full power.
    assert!(gate_allows(owner, "settings", &ctx), "Owner: settings");
    assert!(gate_allows(owner, "copy", &ctx), "Owner: copy");

    // A DELIBERATELY-granted fine-op edge (Owner minting one via `grant`) works:
    // the split is about DEFAULT inheritance, not an absolute ban.
    let edge_cfg = edge_granted(Op::ConfigWrite, &ctx);
    assert!(
        gate_allows(edge_cfg, "settings", &ctx),
        "explicit config-write edge: settings"
    );
    let edge_clip = edge_granted(Op::ClipboardWrite, &ctx);
    assert!(
        gate_allows(edge_clip, "copy", &ctx),
        "explicit clipboard-write edge: copy"
    );
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
    assert_eq!(
        cmd_revoke(&ctx, owner, "src="),
        "ERR usage: revoke <edge-hex> | revoke src=<sid>\n"
    );
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
/// AI can navigate by command: exit codes, output row range, command text.
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
    // `blocktext 0` reads block 0's OUTPUT directly (no coordinate math).
    let txt = cmd_blocktext(&host, 0, "0");
    assert!(
        txt.starts_with("OK ") && txt.contains("hi"),
        "block 0 output wrong: {txt}"
    );
    assert_eq!(cmd_blocktext(&host, 0, "99"), "ERR no such block\n");
}

/// `wait` blocks until the in-flight command completes, then reports it; with
/// no completion at all it times out.
#[test]
fn wait_verb_blocks_until_command_completes() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let reg = subscribe::new_registry();
    let host = GuiHost::new(0, &term, None, &reg);
    // No command in flight and nothing ever completed -> a short wait times out.
    assert_eq!(cmd_wait(&host, 0, "0"), "OK timeout\n");
    // Start a command (executing), then complete it from another thread,
    // firing the SAME notify the GUI's Wake::Output hook fires on output.
    term.lock()
        .unwrap()
        .process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep\n\x1b]133;C\x07");
    let bg = term.clone();
    let reg_bg = reg.clone();
    let h = std::thread::spawn(move || {
        // The completion lands while `wait` is PARKED — its subscriber is
        // registered — a fact, where 40 ms of sleep let a slow `wait`
        // find the completion at entry and never block at all.
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "`wait` parks on the session",
            || (reg_bg.lock().unwrap().watchers(0) == 1).then_some(()),
        );
        bg.lock().unwrap().process(b"\x1b]133;D;0\x07");
        reg_bg.lock().unwrap().notify(0);
    });
    let resp = cmd_wait(&host, 0, "60000");
    h.join().unwrap();
    assert!(
        resp.starts_with("OK complete ") && resp.contains("exit=0"),
        "wait should report the completed command: {resp}"
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

/// A term scrolled off the visible screen into scrollback is still found, the
/// match's absolute row resolves (via `line`) to the same content, and a regex
/// query returns the expected matches (`TerminalSearch` over scrollback + visible).
#[test]
fn search_finds_scrollback_and_regex() {
    // Searches a needle scrolled deep into history, so it assumes the default (large)
    // cap — serialize against the cap-mutation test that lowers the process-global cap.
    let _serial = super::search_cap_test_guard();
    // Small grid so content scrolls into history quickly.
    let term = Arc::new(Mutex::new(Terminal::new(4, 40)));
    // Print a unique needle, then enough lines to push it off-screen.
    term.lock().unwrap().process(b"NEEDLE_alpha\r\n");
    for i in 0..20 {
        term.lock()
            .unwrap()
            .process(format!("filler line {i}\r\n").as_bytes());
    }
    // The needle is no longer on the visible 4-row screen.
    let visible = cmd_text(&term);
    assert!(
        !visible.contains("NEEDLE_alpha"),
        "needle should have scrolled off-screen: {visible}"
    );
    // But search (which indexes scrollback) finds it.
    let s = cmd_search(&term, "NEEDLE_alpha", &RewrapGauge::default());
    assert!(
        s.starts_with("OK 1"),
        "scrolled-off needle must be found: {s}"
    );
    let hit = s.lines().nth(1).expect("match row");
    let abs_row: u64 = hit.split(' ').next().unwrap().parse().expect("abs row");
    // The `line` verb resolves that absolute row to the needle's content.
    let line_out = cmd_line(&term, &abs_row.to_string());
    assert!(
        line_out.contains("NEEDLE_alpha"),
        "line {abs_row} must resolve to the needle (got {line_out})"
    );

    // Regex: a single-token pattern plus the `regex` flag; `fill[a-z]+` matches
    // every "filler" row.
    let rx = cmd_search(&term, "fill[a-z]+ regex", &RewrapGauge::default());
    assert!(
        rx.starts_with("OK "),
        "regex search should succeed (regex feature enabled): {rx}"
    );
    let count: usize = rx
        .lines()
        .next()
        .and_then(|h| h.strip_prefix("OK "))
        .and_then(|n| n.split(' ').next())
        .and_then(|n| n.parse().ok())
        .expect("count");
    assert!(
        count >= 2,
        "regex `fill[a-z]+` should match many filler rows: {rx}"
    );

    // Case sensitivity: default is insensitive, `case` flips it.
    let ci = cmd_search(&term, "needle_alpha", &RewrapGauge::default());
    assert!(
        ci.starts_with("OK 1"),
        "case-insensitive default must match: {ci}"
    );
    let cs = cmd_search(&term, "needle_alpha case", &RewrapGauge::default());
    assert!(
        cs.starts_with("OK 0"),
        "case-sensitive must NOT match lowercased: {cs}"
    );
}

/// The search pattern is the rest of the line: a literal with spaces matches
/// verbatim, tail flags still parse after it, and a single-token pattern is
/// unchanged.
#[test]
fn search_matches_a_spaced_literal() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    term.lock()
        .unwrap()
        .process(b"zsh: command not found: frobnicate\r\nCOMMAND NOT FOUND\r\n");
    // The spaced literal is ONE pattern (case-insensitive default: both rows).
    let s = cmd_search(&term, "command not found", &RewrapGauge::default());
    assert!(
        s.starts_with("OK 2"),
        "a spaced literal must match verbatim: {s}"
    );
    // A tail flag still parses after a spaced pattern.
    let cs = cmd_search(&term, "COMMAND NOT FOUND case", &RewrapGauge::default());
    assert!(
        cs.starts_with("OK 1"),
        "case flag after a spaced literal: {cs}"
    );
    // A single remaining token is always the PATTERN, never a flag.
    let bare = cmd_search(&term, "case", &RewrapGauge::default());
    assert!(
        bare.starts_with("OK 0"),
        "bare `search case` searches for the literal: {bare}"
    );
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
        edges: std::sync::Mutex::new(EdgeTable::new()),
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
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        generation_look: Default::default(),
        reset_lane: Default::default(),
        update_parked: Default::default(),
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

/// (a) `@.` (explicit self) resolves to the EXACT self tuple, so a read of `@.`
/// is byte-identical to the verbatim self read (zero-change guarantee).
#[test]
fn at_dot_selector_resolves_self_verbatim() {
    let store = session_store::new_store();
    let self_h = registered_session(0, -1, b"hello-self\r\n");
    store.write().unwrap().register(self_h.clone());

    let self_tuple: Target = (
        self_h.term.clone(),
        self_h.master,
        self_h.local_id,
        self_h.ctx.clone(),
    );

    // `@.` and `@` (empty body) both name self.
    for body in ["", "."] {
        let sel = Selector::parse(body);
        let (t, m, id, _ctx) = resolve_target(&self_tuple, &store, &sel).expect("self resolves");
        assert!(
            Arc::ptr_eq(&t, &self_h.term),
            "@{body} is the same Arc as self"
        );
        assert_eq!(m, self_h.master);
        assert_eq!(id, self_h.local_id);
    }

    // The read through `@.` equals the verbatim self read — byte-for-byte.
    let sel = Selector::parse(".");
    let (t, _, _, _) = resolve_target(&self_tuple, &store, &sel).unwrap();
    assert_eq!(cmd_text(&t), cmd_text(&self_h.term), "@. text == self text");
}

/// (b) A SECOND registered session is readable via `@<local>` and `@<sid>` by an
/// Owner connection, and returns ITS state (its text), not self's.
#[test]
fn owner_reads_a_sibling_via_local_and_sid_selectors() {
    let store = session_store::new_store();
    let self_h = registered_session(0, -1, b"I-am-self\r\n");
    let peer_h = registered_session(7, -1, b"I-am-peer\r\n");
    store.write().unwrap().register(self_h.clone());
    store.write().unwrap().register(peer_h.clone());

    let self_tuple: Target = (
        self_h.term.clone(),
        self_h.master,
        self_h.local_id,
        self_h.ctx.clone(),
    );

    let self_text = cmd_text(&self_h.term);
    let peer_text = cmd_text(&peer_h.term);
    assert_ne!(self_text, peer_text, "the two sessions read distinctly");

    // By process-local id.
    let by_local = resolve_target(&self_tuple, &store, &Selector::parse("7")).expect("by local");
    assert!(
        Arc::ptr_eq(&by_local.0, &peer_h.term),
        "resolved the peer term"
    );
    assert_eq!(
        cmd_text(&by_local.0),
        peer_text,
        "@7 returns the PEER's state"
    );
    assert_ne!(cmd_text(&by_local.0), self_text, "@7 is NOT self's state");

    // By stable SessionId.
    let by_sid =
        resolve_target(&self_tuple, &store, &Selector::parse(peer_h.sid.as_str())).expect("by sid");
    assert!(
        Arc::ptr_eq(&by_sid.0, &peer_h.term),
        "@s-... resolved the peer term"
    );
    assert_eq!(
        cmd_text(&by_sid.0),
        peer_text,
        "@s-... returns the PEER's state"
    );

    // Owner is authorized to read the sibling (same trust domain).
    assert!(
        cross_session_authorized(Scope::Owner, "text", &peer_h.ctx),
        "Owner may read a sibling",
    );

    // An unknown selector fails closed (no such session).
    assert!(resolve_target(&self_tuple, &store, &Selector::parse("999")).is_none());
    assert!(resolve_target(&self_tuple, &store, &Selector::parse("s-nope")).is_none());
}

/// An Edge connection without an authorizing edge into the target is denied
/// `@other`; with a granted edge for the right op it is allowed, and ops stay split
/// (a read edge cannot write).
#[test]
fn edge_cross_session_is_fail_closed_without_edge_and_allowed_with_one() {
    let peer_h = registered_session(7, -1, b"peer\r\n");

    // A connection presenting some token NOT recorded in the peer's table.
    let stray = EdgeToken::generate();
    let read_scope = Scope::Edge(stray);
    assert!(
        !cross_session_authorized(read_scope, "text", &peer_h.ctx),
        "no edge in the target table => DENY (fail-closed)",
    );

    // The peer (as Owner of its own table) grants a ReadScreen edge from the
    // controller's source id into itself, returning the bearer token.
    let src = SessionId::new("s-controller");
    let granted = {
        let mut tbl = peer_h.ctx.edges.lock().unwrap();
        tbl.grant(
            src.clone(),
            peer_h.ctx.self_id.clone(),
            Op::ReadScreen,
            peer_h.ctx.nonce,
        )
    };

    // The bearer presenting THAT token is now authorized to READ the peer...
    let auth_read = Scope::Edge(granted);
    assert!(
        cross_session_authorized(auth_read, "text", &peer_h.ctx),
        "a granted ReadScreen edge authorizes a cross-session read",
    );
    // ...but the op-split denies a WRITE through a read edge.
    assert!(
        !cross_session_authorized(auth_read, "send", &peer_h.ctx),
        "a ReadScreen edge may NOT write the target (read != write)",
    );

    // A restarted target (nonce mismatch) fails the same edge closed (the
    // confused-deputy guard); checked via decide_edge against a fresh nonce.
    let restarted_nonce = LaunchNonce::generate();
    let tbl = peer_h.ctx.edges.lock().unwrap();
    assert_eq!(
        decide_edge(
            &tbl,
            &granted,
            &peer_h.ctx.self_id,
            Op::ReadScreen,
            &restarted_nonce
        ),
        aterm_session::EdgeDecision::Deny,
        "an edge bound to the old nonce fails closed across a restart",
    );
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

/// `sessions` lists the registry: a single-session store yields one line, a family
/// one line per registered session with parent and state.
#[test]
fn sessions_verb_lists_the_registry() {
    let store = session_store::new_store();
    let root = registered_session(0, -1, b"");
    let root_ctx = root.ctx.clone();
    store.write().unwrap().register(root.clone());

    // Base case: one session, one data line.
    let one = cmd_sessions(&root_ctx, &store, None);
    let mut lines = one.lines();
    assert_eq!(lines.next(), Some("OK 1"), "header counts one session");
    let only = lines.next().expect("one data line");
    assert!(only.starts_with("0 "), "local id 0 first: {only}");
    assert!(only.contains(root.sid.as_str()), "carries the sid: {only}");
    assert!(only.contains(" - alive "), "no parent, alive: {only}");
    assert_eq!(lines.next(), None, "exactly one data line");

    // Family case: a child links to the root and the listing is sorted by local.
    let mut child = registered_session(1, -1, b"");
    child.parent = Some(root.sid.clone());
    store.write().unwrap().register(child.clone());
    let two = cmd_sessions(&root_ctx, &store, None);
    let mut l = two.lines();
    assert_eq!(l.next(), Some("OK 2"));
    assert!(l.next().unwrap().starts_with("0 "), "root first");
    let child_line = l.next().unwrap();
    assert!(child_line.starts_with("1 "), "child second");
    assert!(
        child_line.contains(root.sid.as_str()),
        "child names its parent sid"
    );
}

/// `meta` round-trip: `meta set` stores a pct-round-trippable Unicode value, bare
/// `meta` reports it (with engine title/state), a same-value re-set is not a change
/// (no repaint/notify fan-out), and `meta unset` clears to `-`. The `sessions`
/// listing's trailing `meta=` bit tracks whether any field is set.
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
    assert!(read0.contains(" description=- "), "unset reads -: {read0}");
    assert!(read0.contains(" icon=- "), "unset reads -: {read0}");
    assert!(read0.contains(" role=- "), "unset reads -: {read0}");
    assert!(read0.contains(" attention=- "), "unset reads -: {read0}");
    assert!(read0.contains(" state=alive "), "{read0}");
    assert!(
        cmd_sessions(&h.ctx, &store, None)
            .lines()
            .nth(1)
            .unwrap()
            .contains(" meta=0 "),
        "no user metadata yet -> meta=0"
    );

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

    // Description + icon fill their slots; sessions now flags meta=1.
    let (_, c1) = cmd_meta(
        &h.term,
        &store,
        0,
        &h.ctx,
        "set description builds the release",
    );
    let (_, c2) = cmd_meta(&h.term, &store, 0, &h.ctx, "set icon 🚀");
    assert!(c1 && c2);
    let (read2, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read2.contains(&format!(
        " description={} ",
        pct_encode("builds the release")
    )));
    assert!(read2.contains(&format!(" icon={} ", pct_encode("🚀"))));
    assert!(
        cmd_sessions(&h.ctx, &store, None)
            .lines()
            .nth(1)
            .unwrap()
            .contains(" meta=1 "),
        "user metadata present -> meta=1"
    );

    // The typed role/attention keys ride the same set/read/unset cycle.
    let (_, c3) = cmd_meta(&h.term, &store, 0, &h.ctx, "set role operator");
    let (_, c4) = cmd_meta(
        &h.term,
        &store,
        0,
        &h.ctx,
        "set attention needs human: approval box",
    );
    assert!(c3 && c4);
    let (read_typed, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read_typed.contains(&format!(" role={} ", pct_encode("operator"))));
    assert!(read_typed.contains(&format!(
        " attention={} ",
        pct_encode("needs human: approval box")
    )));
    // A STORED "-" is not the unset sentinel: it reads back escaped.
    let (_, dash) = cmd_meta(&h.term, &store, 0, &h.ctx, "set role -");
    assert!(dash);
    let (read_dash, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read_dash.contains(" role=%2D "), "{read_dash}");
    let (_, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "set role operator");
    let (_, cleared) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset attention");
    assert!(cleared);
    let (read_cleared, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read_cleared.contains(" attention=- "), "{read_cleared}");

    // Unset clears back to '-' (labels fall back down the chain); a second
    // unset of the same field is a no-change.
    let (r, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset title");
    assert_eq!(r, "OK\n");
    assert!(changed);
    let (read3, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read3.contains(" user_title=- "), "cleared: {read3}");
    let (_, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset title");
    assert!(!changed, "second unset is a no-change");

    // Unknown fields / malformed forms answer honest usage ERRs.
    let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "set colour red");
    assert!(r.starts_with("ERR unknown meta field") && !c);
    let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset colour");
    assert!(r.starts_with("ERR unknown meta field") && !c);
    let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "set title");
    assert!(r.starts_with("ERR usage") && !c, "value required: {r}");
    let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "bogus");
    assert!(r.starts_with("ERR usage") && !c);
}

/// The question-policy field on the wire: four words (`ask`, and the supervisor's
/// priority words `recommended`, `long-term`, `short-term`) with one usage line for
/// anything else, stored case-folded, read back by key as the last field (after
/// `supervisor=`), `ls meta=1` while it alone is set, and `unset` clearing it to `-`
/// (back to the global `[harness]` policy). A session principal or a misspelled
/// priority word (`long_term`) is refused.
#[test]
fn meta_questions_is_a_closed_set_stored_case_folded_and_read_last() {
    const USAGE: &str = "ERR usage: meta set questions ask|recommended|long-term|short-term\n";
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let (read0, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(
        read0.ends_with(
            " supervisor=- supervisor_proven=0 questions=- agent_cwd=- autopilot=- land_at=-\n"
        ),
        "appended after supervisor=, unset reads -: {read0}"
    );

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
    assert!(
        read1.ends_with(" questions=recommended agent_cwd=- autopilot=- land_at=-\n"),
        "{read1}"
    );
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
    assert!(
        read2.ends_with(" questions=ask agent_cwd=- autopilot=- land_at=-\n"),
        "{read2}"
    );
    // The priority words, case folded like the rest.
    for (typed, stored) in [("Long-Term", "long-term"), ("short-term", "short-term")] {
        let (r, c) = cmd_meta(
            &h.term,
            &store,
            0,
            &h.ctx,
            &format!("set questions {typed}"),
        );
        assert_eq!(r, "OK\n", "{typed}");
        assert!(c, "{typed}");
        let (read, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
        assert!(
            read.ends_with(&format!(
                " questions={stored} agent_cwd=- autopilot=- land_at=-\n"
            )),
            "{read}"
        );
    }

    let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset questions");
    assert_eq!(r, "OK\n");
    assert!(c);
    let (read3, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(
        read3.ends_with(" questions=- agent_cwd=- autopilot=- land_at=-\n"),
        "{read3}"
    );
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
    const USAGE: &str = "ERR usage: meta set autopilot cruise|land|off\n";
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
        assert_eq!(r, USAGE, "{bad:?}");
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

/// Metadata byte caps are hard refusals, never truncation: title > 120B,
/// description > 1024B, icon > 64B, role > 64B, attention > 256B each answer
/// `ERR … too long` naming the cap, leaving the stored value untouched.
#[test]
fn meta_caps_reject_over_cap_values() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let over = |n: usize| "x".repeat(n + 1);

    let (r, changed) = cmd_meta(
        &h.term,
        &store,
        0,
        &h.ctx,
        &format!("set title {}", over(120)),
    );
    assert_eq!(r, "ERR title too long (max 120 bytes)\n");
    assert!(!changed);
    let (r, _) = cmd_meta(
        &h.term,
        &store,
        0,
        &h.ctx,
        &format!("set description {}", over(1024)),
    );
    assert_eq!(r, "ERR description too long (max 1024 bytes)\n");
    let (r, _) = cmd_meta(
        &h.term,
        &store,
        0,
        &h.ctx,
        &format!("set icon {}", over(64)),
    );
    assert_eq!(r, "ERR icon too long (max 64 bytes)\n");
    let (r, _) = cmd_meta(
        &h.term,
        &store,
        0,
        &h.ctx,
        &format!("set role {}", over(64)),
    );
    assert_eq!(r, "ERR role too long (max 64 bytes)\n");
    let (r, _) = cmd_meta(
        &h.term,
        &store,
        0,
        &h.ctx,
        &format!("set attention {}", over(256)),
    );
    assert_eq!(r, "ERR attention too long (max 256 bytes)\n");
    // Nothing was stored by any refused write.
    assert!(!h.ctx.meta.lock().unwrap().any_set());
    // AT-cap values are accepted (the cap is inclusive, after trim).
    let (r, changed) = cmd_meta(
        &h.term,
        &store,
        0,
        &h.ctx,
        &format!("set title {}", "x".repeat(120)),
    );
    assert_eq!(r, "OK\n");
    assert!(changed);
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
            reply.contains("single-line") && reply.contains("control, bidi, or invisible"),
            "hostile value gets an explicit refusal: {reply:?}"
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
/// in order with monotonic ids by the store mutators and `meta set`, and the verb's
/// `<n>` / `since=<id>` grammar mirrors `history`.
#[test]
fn timeline_records_lifecycle_in_order_and_honors_n_and_since() {
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

    // `<n>` keeps the LAST n; `since=<id>` is a strict suffix; both compose.
    let last_two = cmd_timeline(&h.ctx, "2");
    assert!(last_two.starts_with("OK 2\n"));
    assert!(last_two.contains("kind=meta-change") && last_two.contains("kind=state-change"));
    let since = cmd_timeline(&h.ctx, "since=3");
    assert_eq!(since.lines().count(), 2, "{since}");
    assert!(since.starts_with("OK 1\n") && since.contains("event 4 "));
    // Malformed args answer a usage ERR, mirroring `history`.
    assert!(cmd_timeline(&h.ctx, "since=bogus").starts_with("ERR usage"));
    assert!(cmd_timeline(&h.ctx, "wat").starts_with("ERR usage"));

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

/// A SURVIVING COPY OF THE CLIENT'S END — what a sibling test's fork holds
/// until it execs — must not keep a push loop alive after its client hangs
/// up, and the hang-up must not need a write to be noticed.
///
/// The negative control is the teardown every push test here used to run:
/// `drop` the client while a copy lives. The server end then reads no EOF,
/// so the production probe cannot see the client go, and a write to it
/// still succeeds, so a loop waiting for a failed write never sees it
/// either. A `try_clone` stands in for the forked copy: both are one more
/// descriptor on the same open socket, which is all the kernel counts. The
/// second control (macOS) is the both-ways `shutdown` [`hang_up`] used to
/// make, which Darwin refuses, shutting nothing, once the server end has
/// half-closed — every relay test's teardown after its child answered.
#[test]
fn a_hung_up_client_ends_its_push_loop_though_a_copy_of_its_end_survives() {
    use std::io::Write as _;

    let quick = |server: &CtlStream| {
        server
            .set_read_timeout(Some(std::time::Duration::from_millis(1)))
            .unwrap();
    };

    // NEGATIVE CONTROL: dropped, not hung up, with a copy alive.
    let (client, server) = CtlStream::pair().unwrap();
    quick(&server);
    let forked_copy = client.try_clone().unwrap();
    drop(client);
    assert!(
        !subscription_peer_gone(&server),
        "a dropped descriptor whose socket a copy holds open reads as a live peer"
    );
    assert!(
        (&server).write_all(b"DELTA 0 seq=1\n").is_ok(),
        "and a write into it succeeds, so the write path never learns either"
    );
    drop(forked_copy);

    // The fix, at the probe: hung up with a copy alive is gone at once.
    let (client, server) = CtlStream::pair().unwrap();
    quick(&server);
    let forked_copy = client.try_clone().unwrap();
    hang_up(client);
    assert!(
        subscription_peer_gone(&server),
        "a hang-up reaches the server end whatever copies of the client end survive"
    );
    drop(forked_copy);

    // And after the server end half-closed toward the client first — a
    // relay passes its child's EOF on so — where a both-ways `shutdown`
    // is refused on Darwin and shuts nothing, the write half included.
    let (client, server) = CtlStream::pair().unwrap();
    quick(&server);
    let forked_copy = client.try_clone().unwrap();
    server.shutdown(std::net::Shutdown::Write).unwrap();
    #[cfg(target_os = "macos")]
    {
        // NEGATIVE CONTROL: the both-ways shutdown `hang_up` used to make.
        assert!(
            client.shutdown(std::net::Shutdown::Both).is_err(),
            "Darwin refuses a both-ways shutdown of a socket its peer half-closed"
        );
        assert!(
            !subscription_peer_gone(&server),
            "and that refusal shut no half: the server end reads no EOF"
        );
    }
    hang_up(client);
    assert!(
        subscription_peer_gone(&server),
        "a hang-up reaches a server end that half-closed first"
    );
    drop(forked_copy);

    // End to end, through the production subscribe path, with the terminal
    // quiet after the catch-up: only the probe can end this loop.
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);
    let registry = subscribe::new_registry();
    let (client, server) = CtlStream::pair().unwrap();
    let forked_copy = client.try_clone().unwrap();
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
    let acc = read_until(&client, String::new(), |s| s.contains("DELTA 0 seq="));
    assert!(acc.contains("OK subscribe 1\n"), "subscribe ack: {acc:?}");
    hang_up(client);
    join_after_hang_up(
        join,
        "the push loop to end on a hang-up while a copy of the client end survives",
    );
    assert_eq!(
        registry.lock().unwrap().watched_sessions(),
        0,
        "deregistered on return"
    );
    drop(forked_copy);
}

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

/// An adopted session's `topic ls` answers the carried set, through the verb.
/// `spawn_session` re-seeds the same way (`set_topics(parse_topics(&adopted.topics))`);
/// a real handoff needs a PTY this test lacks.
#[test]
fn an_adopted_session_answers_the_carried_topics() {
    let store = session_store::new_store();
    let subscribers = crate::subscribe::new_registry();
    let h = registered_session(0, -1, b"");
    let sid = h.sid.clone();
    let carried = vec!["build.failed head".to_string(), "sat-comp @42".to_string()];
    h.ctx
        .fabric
        .set_topics(crate::fabric::parse_topics(&carried));
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
    assert_eq!(
        owner("ls"),
        "OK 2 seen=0\ntopic build.failed since=head\ntopic sat-comp since=@42\n"
    );
    assert_eq!(owner("add fresh"), "OK fresh since=head added=1\n");
    assert!(
        owner("ls").contains("topic fresh since=head\n"),
        "{}",
        owner("ls")
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
    let full = format!("ERR topics full (max {MAX_TOPICS}): drop one first\n");
    assert_eq!(owner("add task-over"), full);
    assert_eq!(pushed(&h_ctx), before, "a refused add pushes nothing");
    assert!(owner("ls").starts_with(&format!("OK {MAX_TOPICS} seen=0\n")));
    assert!(!owner("ls").contains("task-over"));
    // A topic already held still answers as one.
    assert_eq!(owner("add task-0"), "OK task-0 since=head added=0\n");
    // Drop one, and a new one goes in.
    assert_eq!(owner("drop task-0"), "OK task-0 dropped=1\n");
    assert_eq!(owner("add task-over"), "OK task-over since=head added=1\n");
    assert_eq!(owner("add task-0"), full);

    // A carried set past the bound is taken whole.
    let carried = (0..MAX_TOPICS + 5)
        .map(|n| format!("old-{n} head"))
        .collect::<Vec<_>>();
    h_ctx
        .fabric
        .set_topics(crate::fabric::parse_topics(&carried));
    assert!(owner("ls").starts_with(&format!("OK {} seen=0\n", MAX_TOPICS + 5)));
    assert_eq!(owner("add fresh"), full);
    for n in 0..6 {
        assert_eq!(
            owner(&format!("drop old-{n}")),
            format!("OK old-{n} dropped=1\n")
        );
    }
    assert_eq!(owner("add fresh"), "OK fresh since=head added=1\n");

    // The verb's own help names the bound and the refusal.
    let spec = aterm_types::control_verbs::VERBS
        .iter()
        .find(|spec| spec.name == "topic")
        .expect("topic is a verb");
    assert!(
        spec.detail
            .contains(&format!("at most {MAX_TOPICS} topics"))
            && spec.detail.contains(full.trim_end()),
        "{}",
        spec.detail
    );
}

/// Ruling 441 (j): a `messages` row's `state=` is [`aterm_messages::wire::state_word`]
/// (design §5.1, ruling 175) — `held`, `live`, `review`, or how the row retired
/// ([`aterm_messages::Retired::as_word`]; ruling 267 named `quit`). The verb's
/// help is the grammar a client validates against, so it lists every word. The
/// retirements are walked by an exhaustive `match`: a new `Retired` fails to
/// compile here until it joins the walk, and so the check.
#[test]
fn the_messages_help_lists_every_state_word() {
    use aterm_messages::{MessageId, Outcome, Retired};
    fn after(prev: Option<&Retired>) -> Option<Retired> {
        let by = MessageId::from_raw(1).expect("a valid id");
        match prev {
            None => Some(Retired::Folded),
            Some(Retired::Folded) => Some(Retired::Stale),
            Some(Retired::Stale) => Some(Retired::Unseen),
            Some(Retired::Unseen) => Some(Retired::Superseded { by }),
            Some(Retired::Superseded { .. }) => Some(Retired::Resolved(Outcome::Ok)),
            Some(Retired::Resolved(Outcome::Ok)) => Some(Retired::Resolved(Outcome::Warn)),
            Some(Retired::Resolved(Outcome::Warn)) => Some(Retired::Dismissed),
            Some(Retired::Dismissed) => Some(Retired::Answered {
                label: "Not now".into(),
            }),
            Some(Retired::Answered { .. }) => Some(Retired::Evicted),
            Some(Retired::Evicted) => Some(Retired::Carried),
            Some(Retired::Carried) => Some(Retired::Withdrawn),
            Some(Retired::Withdrawn) => Some(Retired::Recorded),
            Some(Retired::Recorded) => Some(Retired::Quit),
            Some(Retired::Quit) => None,
        }
    }
    let spec = aterm_types::control_verbs::VERBS
        .iter()
        .find(|v| v.name == "messages")
        .expect("the messages verb");
    let help = spec.help_line();
    let grammar = help
        .split("state=<")
        .nth(1)
        .and_then(|rest| rest.split('>').next())
        .expect("a state= grammar");
    let listed: Vec<&str> = grammar.split('|').collect();
    let retired: Vec<Retired> = std::iter::successors(after(None), |r| after(Some(r))).collect();
    let missing: Vec<&str> = ["held", "live", "review"]
        .into_iter()
        .chain(retired.iter().map(Retired::as_word))
        .filter(|word| !listed.contains(word))
        .collect();
    assert!(
        missing.is_empty(),
        "`messages` prints state={missing:?}, which its help's grammar omits: {grammar}"
    );
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

    // A broadcast row says so on the push line: `topic=` answers why the row is in
    // this mailbox (addressed rows because someone chose this session, broadcasts
    // because it chose the topic) without the body this stream never carries.
    let mixed = watch(
        "mail",
        &[
            ("h-andrew", "task", ""),
            ("s-peer", "note", "topic=build.failed "),
        ],
        2,
    );
    assert!(
        mixed.contains("MAIL 0 id=1 off=500 from=h-andrew kind=task\n"),
        "an addressed row carries no topic: {mixed:?}"
    );
    assert!(
        mixed.contains("MAIL 0 id=2 off=501 from=s-peer kind=note topic=build.failed\n"),
        "{mixed:?}"
    );

    // …AND `mail:topic=` NARROWS TO IT. The addressed row is stepped over,
    // which is what makes this a filter rather than a label.
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

/// (a) SELF subscribe to `screen`: a write to the term pushes a sid-tagged DELTA
/// carrying the live text; a subsequent PURE viewport scroll (which never bumps
/// `content_seq`) pushes NOTHING. End-to-end through `run_subscribe_socket`
/// (auth + flip) and the push loop over a real socket, with the production notify hook
/// (the registry `notify`) driving the wake.
#[test]
fn subscribe_self_screen_delta_on_write_none_on_scroll() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);
    let registry = subscribe::new_registry();

    let (client, server) = CtlStream::pair().unwrap();
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

    // The ack confirms the flip to push mode (accumulate past any immediate
    // catch-up frame that may coalesce into the same read).
    let acc = read_until(&client, String::new(), |s| s.contains("OK subscribe 1\n"));
    assert!(acc.contains("OK subscribe 1\n"), "subscribe ack: {acc:?}");

    // Produce real content, then fire the SAME notify the GUI's Wake::Output
    // hook fires. The push loop re-reads the latest grid and emits a DELTA.
    crate::term_lock(&h.term).process(b"hello-live");
    registry.lock().unwrap().notify(0);
    let frame = read_until(&client, acc, |s| s.contains("hello-live"));
    assert!(
        frame.contains("DELTA 0 seq="),
        "screen delta pushed: {frame:?}"
    );
    assert!(
        frame.contains("hello-live"),
        "delta carries live text: {frame:?}"
    );

    // A PURE viewport scroll does not bump content_seq -> no further DELTA even
    // though we notify (a coalesced/spurious wake reads unchanged content).
    crate::term_lock(&h.term).scroll_display(1);
    registry.lock().unwrap().notify(0);
    // Positive by proxy: asserting silence would pass when the push thread did
    // not run. Instead, follow the scroll with a
    // known content change and require the next delta to be that change; a
    // scroll-derived delta would arrive first, and a starved thread times out
    // rather than passing.
    crate::term_lock(&h.term).process(b"after-scroll");
    registry.lock().unwrap().notify(0);
    let next = read_until(&client, String::new(), |s| s.contains("after-scroll"));
    let first_delta = next
        .split("DELTA ")
        .nth(1)
        .expect("a delta must follow the content change");
    assert!(
        first_delta.contains("after-scroll"),
        "the first delta after a pure viewport scroll must be the CONTENT change, \
         not a scroll-derived one: {next:?}"
    );

    // Hang up: the loop's peer probe reads it on the next tick, and the loop
    // returns and deregisters.
    hang_up(client);
    join_after_hang_up(join, "the push loop to end on its client's hang-up");
    assert_eq!(
        registry.lock().unwrap().watched_sessions(),
        0,
        "deregistered on drop"
    );
}

/// A push-only client can disappear while every watched terminal is quiet. The
/// server must discover that from the socket's read side, not from a future
/// write, or each crash permanently occupies one of the four reserved workers.
/// Run more sequential crash cycles than the pool size, each client crashing
/// with every frame read and nothing left to write; every loop must return
/// and deregister on its own.
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
                "subscribe @. screen",
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

/// A live subscriber keeps its write half open, so the server can tell a quiet
/// reader from a dead or half-closed one without heartbeat traffic.
#[test]
fn subscription_probe_keeps_quiet_peer_and_rejects_write_half_close() {
    let (client, server) = CtlStream::pair().expect("probe socket pair");
    server
        .set_read_timeout(Some(std::time::Duration::from_millis(1)))
        .expect("bound liveness read");
    assert!(
        !subscription_peer_gone(&server),
        "a fully-open quiet peer remains subscribed"
    );

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

/// The other half of audit 4.4: Owner KEEPS the `sessions` stream (it already
/// reads the roster through the `sessions`/`who` verbs), and the stream really
/// reports a SIBLING the subscriber never named — `@.` watches session 0 while
/// the `EVENT *` line announces a session registered afterwards. End-to-end
/// through `run_subscribe_socket` + the push loop, so it also pins that the
/// narrowing did not sever the wire path.
#[test]
fn owner_subscription_keeps_the_instance_sessions_stream() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);
    let registry = subscribe::new_registry();

    let (client, server) = CtlStream::pair().unwrap();
    let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
    let join = std::thread::spawn(move || {
        let mut w = server;
        run_subscribe_socket(
            "subscribe @. screen,sessions",
            &active_t,
            &store_t,
            &reg_t,
            Scope::Owner,
            &mut w,
        );
    });
    // The ack is the barrier: the push loop seeds the roster watermark first
    // and writes the ack after, so a sibling registered once the ack is read
    // cannot land in the baseline (where it would correctly never be
    // announced). The catch-up frame that follows it is read here too.
    let acc = read_until(&client, String::new(), |s| s.contains("DELTA 0 seq="));
    assert!(acc.contains("OK subscribe 1\n"), "subscribe ack: {acc:?}");
    assert!(acc.contains("DELTA 0 seq="), "catch-up frame: {acc:?}");

    // The sibling is not a target and fires no notify, so the instance diff reports
    // it on the bounded 250 ms tick.
    let sib = registered_session(2, -1, b"");
    store.write().unwrap().register(sib.clone());
    let seen = read_until(&client, acc, |s| s.contains("session-created"));
    assert!(
        seen.contains(&format!("EVENT * session-created {}\n", sib.sid.as_str())),
        "Owner sees the sibling spawn: {seen:?}"
    );

    // Tear down: hang up, and the loop's peer probe ends it on the next tick.
    hang_up(client);
    join_after_hang_up(
        join,
        "the sessions push loop to end on its client's hang-up",
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

/// The same scoped `Edge` holding a `ReadScreen` grant on the sibling's table
/// (minted by the owner, presented as the connection token) subscribes to the
/// sibling and receives sid-tagged deltas, via the cross-session read path
/// (`decide_edge` against the target table + nonce).
#[test]
fn subscribe_sibling_with_read_edge_pushes_deltas() {
    let store = session_store::new_store();
    let self_h = registered_session(0, -1, b"");
    let sib = registered_session(2, -1, b"");
    store.write().unwrap().register(self_h.clone());
    store.write().unwrap().register(sib.clone());
    let active = active_for(&self_h);
    let registry = subscribe::new_registry();

    // Owner mints a ReadScreen edge (self -> sibling). The bearer presents that
    // token as the connection's Edge scope.
    let tok = {
        let mut edges = sib.ctx.edges.lock().unwrap();
        edges.grant(
            self_h.sid.clone(),
            sib.sid.clone(),
            Op::ReadScreen,
            sib.nonce,
        )
    };
    // Sanity: the gate would PERMIT this exact (token, target) pair.
    assert!(
        cross_session_authorized(Scope::Edge(tok), "subscribe", &sib.ctx),
        "minted edge authorizes the cross subscribe",
    );

    let (client, server) = CtlStream::pair().unwrap();
    let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
    let scope = Scope::Edge(tok);
    let join = std::thread::spawn(move || {
        let mut w = server;
        run_subscribe_socket(
            "subscribe @2 screen",
            &active_t,
            &store_t,
            &reg_t,
            scope,
            &mut w,
        );
    });

    let ack = read_until(&client, String::new(), |s| s.contains("OK subscribe 1\n"));
    assert!(
        ack.contains("OK subscribe 1\n"),
        "edge subscribe authorized: {ack:?}"
    );

    crate::term_lock(&sib.term).process(b"from-sibling");
    registry.lock().unwrap().notify(2);
    let frame = read_until(&client, ack, |s| s.contains("from-sibling"));
    assert!(
        frame.contains("DELTA 2 seq="),
        "sibling delta tagged with its sid: {frame:?}"
    );
    assert!(
        frame.contains("from-sibling"),
        "carries the sibling's screen: {frame:?}"
    );

    hang_up(client);
    join_after_hang_up(join, "the edge push loop to end on its client's hang-up");
}

/// (d) MULTIPLEX: one connection subscribing to `@0,@2` receives DELTAs tagged
/// with EACH session's own sid, so the client demultiplexes by the leading token.
#[test]
fn subscribe_multiplex_two_sids_tags_each() {
    let store = session_store::new_store();
    let a = registered_session(0, -1, b"");
    let b = registered_session(2, -1, b"");
    store.write().unwrap().register(a.clone());
    store.write().unwrap().register(b.clone());
    let active = active_for(&a);
    let registry = subscribe::new_registry();

    let (client, server) = CtlStream::pair().unwrap();
    let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
    let join = std::thread::spawn(move || {
        let mut w = server;
        // `@0` is self (active), `@2` a sibling; Owner reaches both.
        run_subscribe_socket(
            "subscribe @0,@2 screen",
            &active_t,
            &store_t,
            &reg_t,
            Scope::Owner,
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

/// `subscribe <streams>` with no `@<sel>` targets the connection's own session
/// (`@.`) like every selectorless verb: a valid stream list as the first token is
/// the streams, not a selector.
#[test]
fn subscribe_without_selector_defaults_to_self() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);
    let registry = subscribe::new_registry();

    let (client, server) = CtlStream::pair().unwrap();
    let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
    let join = std::thread::spawn(move || {
        let mut w = server;
        // NOTE: no `@<sel>` — just the stream list. Must resolve to self (`@.`).
        run_subscribe_socket(
            "subscribe screen",
            &active_t,
            &store_t,
            &reg_t,
            Scope::Owner,
            &mut w,
        );
    });

    let h_sid = h.sid.as_str().to_string();
    let ack = read_until(&client, String::new(), |s| s.contains("OK subscribe 1\n"));
    assert!(
        ack.contains("OK subscribe 1\n"),
        "selectorless subscribe acks one (self) target: {ack:?}"
    );
    assert!(
        ack.contains(&format!("sub 0 {h_sid}\n")),
        "the single target maps to the connection's own sid: {ack:?}"
    );

    crate::term_lock(&h.term).process(b"self-default");
    registry.lock().unwrap().notify(0);
    let frame = read_until(&client, ack, |s| s.contains("self-default"));
    assert!(
        frame.contains("DELTA 0 seq=") && frame.contains("self-default"),
        "self delta pushed for the selectorless subscribe: {frame:?}"
    );

    hang_up(client);
    join_after_hang_up(
        join,
        "the selectorless push loop to end on its client's hang-up",
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

/// `json_escape` produces RFC 8259 string bodies: quotes, backslashes and
/// whitespace controls get two-char escapes, other C0 bytes `\u00XX`, and other
/// text (including non-ASCII) is verbatim.
#[test]
fn json_escape_handles_quotes_controls_and_unicode() {
    assert_eq!(json_escape("plain"), "plain");
    assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
    assert_eq!(json_escape("tab\tnl\ncr\r"), "tab\\tnl\\ncr\\r");
    assert_eq!(json_escape("\u{0001}"), "\\u0001");
    // Non-ASCII is emitted verbatim (JSON strings are UTF-8).
    assert_eq!(json_escape("café 日本"), "café 日本");
}

/// `text --json` emits the documented schema: a `rows` array of the same
/// grapheme-faithful tail-trimmed lines the text form emits, plus `cursor`, `dims`
/// and `seq` (content_seq). Without the flag the text path is unchanged.
#[test]
fn text_json_mode_matches_text_rows_and_carries_cursor_seq() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    term.lock()
        .unwrap()
        .process(b"line-zero\r\nsecond \"quoted\"");

    // Text form (the byte-identical baseline) and JSON form.
    let text = cmd_text(&term);
    let json = cmd_text_json(&term);

    // Framing: `OK 1\n<json>\n` (one body line), and the body is well-formed JSON.
    assert!(json.starts_with("OK 1\n"), "json framing: {json}");
    let body = json.strip_prefix("OK 1\n").unwrap().trim_end();
    assert_well_formed_json(body);

    // Rows carry the same visible content as the text form's data lines.
    let text_rows: Vec<&str> = text.lines().skip(1).collect();
    assert!(body.contains("\"rows\":["), "has rows array: {body}");
    assert!(
        body.contains(&format!("\"{}\"", text_rows[0])),
        "row0 present: {body}"
    );
    // The quote in row 1 is escaped in JSON, NOT in text.
    assert!(
        text_rows[1].contains("second \"quoted\""),
        "text keeps raw quote"
    );
    assert!(
        body.contains("second \\\"quoted\\\""),
        "json escapes the quote: {body}"
    );

    // cursor + dims + seq members are present and consistent with the verbs.
    let c = term.lock().unwrap().cursor();
    assert!(
        body.contains(&format!("\"row\":{}", c.row)),
        "cursor row: {body}"
    );
    assert!(
        body.contains("\"dims\":{\"rows\":24,\"cols\":80}"),
        "dims: {body}"
    );
    assert!(body.contains("\"seq\":"), "carries content_seq: {body}");
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
    for (verb, refusal) in [
        ("offscreen", "ERR json: not supported for offscreen\n"),
        ("modes", "ERR json: not supported for modes\n"),
    ] {
        assert_eq!(json_unsupported(verb), Some(refusal.to_string()), "{verb}");
    }
    for v in ["selection", "colors", "cell", "line", "history", "wait"] {
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
/// their text forms in a well-formed JSON body.
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
    // `family_with_a_sid_is_owner_only_at_the_dispatch`), so the handler has no
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
    assert_eq!(
        raise_target(&store, "s-nope"),
        Err("ERR no such session\n".to_string())
    );
    assert_eq!(
        raise_target(&store, ""),
        Err("ERR usage: raise <sid>\n".to_string())
    );
    assert_eq!(
        raise_target(&store, "s-a s-b"),
        Err("ERR usage: raise <sid>\n".to_string())
    );
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
/// framing, transform or auth case, mirroring `serve`: a `BufReader` over the
/// control input stream, the request line first, then the binary frame run as an
/// Owner or an ungranted Edge.
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
        owner: bool,
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
            owner: true,
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
            owner: true,
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
            owner: true,
            keep: false,
            reply: Reply::Exact("ERR feed-bin too large: the cap is 262144 bytes\n"),
            pty: Some(b""),
            next: None,
        },
        // A ReadScreen Edge is denied the write (WriteInput required), but the N
        // payload bytes are still consumed so the stream stays framed, and nothing
        // reaches the PTY.
        Row {
            label: "a read edge is denied but the payload is consumed",
            input: b"feed-bin 3\nABCafter\n".to_vec(),
            verb: "feed-bin",
            line: None,
            frame: Frame::Unchecked,
            owner: false,
            keep: true,
            reply: Reply::Exact("ERR denied\n"),
            pty: Some(b""),
            next: Some("after"),
        },
        // A malformed length replies `ERR usage` WITHOUT consuming any payload:
        // the next line is whatever followed the bad request line verbatim.
        Row {
            label: "a bad length consumes no payload",
            input: b"feed-bin notanumber\nfollowing\n".to_vec(),
            verb: "feed-bin",
            line: None,
            frame: Frame::Unchecked,
            owner: true,
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
        let scope = if row.owner {
            Scope::Owner
        } else {
            Scope::Edge(EdgeToken::generate())
        };
        let keep = run_feed_bin(
            &line,
            row.verb,
            &mut reader,
            &active,
            &store,
            scope,
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

/// Why the dead-peer tests kill their peer in the kernel instead of dropping a
/// reader.
///
/// A stranger holding a duplicate of the read end means the peer is not dead: the
/// write lands, and the seam truthfully says `Ok` (the false failure
/// [`cloexec_pipe`]'s note describes, a test-premise defect). A pipe cannot tell
/// whose descriptor a duplicate is, so the child a racing `posix_spawn` would get
/// is modelled by `dup(2)` in this process.
///
/// Both halves are asserted with no sleep or thread between them: `Ok` while the
/// stranger holds the read end, and `WriteFailed` after [`kill_peer_in_kernel`]
/// while that stranger still holds it (a `shutdown` no descriptor can undo).
#[test]
#[cfg(unix)]
fn a_stranger_holding_the_read_end_cannot_revive_a_kernel_killed_peer() {
    use std::os::fd::AsRawFd;
    let (h, rx) = pipe_session(75);
    // SAFETY: `rx` is live across this call, and `F_DUPFD_CLOEXEC` either
    // returns a new descriptor for the same read end — close-on-exec from
    // birth, so the one stranger is this test's own — or -1.
    let stranger = unsafe { libc::fcntl(rx.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    assert!(stranger >= 0, "dup the read end: a stranger's duplicate");
    drop(rx);
    assert_eq!(
        cross_raw_input(&h.term, &h.ctx, send_bytes("ls")),
        Ok(InputOutcome::Ok),
        "negative control: with a stranger on the read end the peer is NOT dead, \
         the bytes really land, and OK is the honest answer — this is the reading \
         a dropped reader alone can hand a dead-peer test"
    );
    kill_peer_in_kernel(h.master, "a pipe-backed session whose reader is duplicated");
    assert_eq!(
        cross_raw_input(&h.term, &h.ctx, send_bytes("ls")),
        Ok(InputOutcome::WriteFailed),
        "the kernel-killed peer is dead while the stranger STILL holds its duplicate"
    );
    // SAFETY: this test's own duplicate, closed exactly once.
    assert_eq!(unsafe { libc::close(stranger) }, 0, "close the stranger");
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
/// observed dead. `reading_stranger` plants the second case on purpose (the
/// negative control).
#[cfg(unix)]
fn paste_while_the_peer_closes_mid_write(reading_stranger: bool) -> MidWriteClose {
    use std::os::fd::AsRawFd;
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

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stranger = reading_stranger.then(|| {
        // SAFETY: `reader` is live here, and `F_DUPFD_CLOEXEC` either
        // returns a new descriptor for the same read end, close-on-exec from
        // birth, or -1.
        let fd = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        assert!(fd >= 0, "dup the read end: the stranger's duplicate");
        aterm_pty::set_nonblocking(fd, true).expect("nonblocking stranger");
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                // SAFETY: a read into a live local buffer of exactly its
                // length, from the stranger's own descriptor.
                let read = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                if read <= 0 {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            // SAFETY: the stranger's own duplicate, closed exactly once.
            unsafe { libc::close(fd) };
        })
    });
    // MID-WRITE IS A FACT, not 25 ms of hoping: the peer closes once the
    // paste is queued behind the older frame — the spill holds more than
    // that frame. With a READING stranger the spill can drain before the
    // paste arrives, which then goes straight to the kernel, so that world
    // closes once the paste was attempted at all (the sink's input epoch
    // moved). A panic here drops `reader` as it unwinds, so the paste
    // below is released either way.
    let older = handle.ctx.sink.spilled_bytes();
    let before_paste = handle.ctx.sink.input_epoch();
    let sink = Arc::clone(&handle.ctx.sink);
    let closer = std::thread::spawn(move || {
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the paste is in flight behind the older frame",
            || {
                let in_flight = if reading_stranger {
                    sink.input_epoch() != before_paste
                } else {
                    sink.spilled_bytes() > older
                };
                in_flight.then_some(())
            },
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
    stop.store(true, std::sync::atomic::Ordering::Release);
    if let Some(stranger) = stranger {
        stranger.join().expect("stranger reader");
    }
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
        let run = paste_while_the_peer_closes_mid_write(false);
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

/// Negative control for the probe: a stranger that holds and reads the read end
/// lets the spill reach the kernel, so the paste is answered `OK` and the probe
/// classifies the run void, not dead.
#[test]
#[cfg(unix)]
fn a_reading_stranger_voids_the_mid_write_paste_run() {
    let run = paste_while_the_peer_closes_mid_write(true);
    assert_eq!(
        run.reply, "OK\n",
        "with a live reader the bytes landed, and OK is the honest reply"
    );
    assert!(
        !run.peer_observed_dead,
        "the probe must see the stranger, or a void run would be read as a verdict"
    );
    assert!(
        run.reached_kernel,
        "the stranger drained the spill to the kernel"
    );
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
    let handle = session_on(78, master);
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

/// The shipping binary-frame parser and native input dispatcher compose through to
/// the focused Editor minibuffer: the `aterm-ctl paste --stdin` route including
/// exact payload framing.
#[test]
fn native_paste_bin_drives_focused_editor_minibuffer_and_keeps_stream_framed() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-control-native-paste-bin-{}-{:?}",
        std::process::id(),
        std::thread::current().id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("native-paste-bin.md");
    std::fs::write(&path, "base\n").unwrap();
    app.open_document_tab(
        crate::native_app::AppKind::Editor,
        // The shipping encoder: a hand-rolled `format!` is malformed on Windows
        // (drive letter and backslashes after the authority slot).
        &crate::native_document_host::path_to_file_uri(&path).unwrap(),
    )
    .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_event(
        wid,
        crate::native_app::AppEvent::EditorCommand(
            crate::native_editor::EditorCommand::IncrementalSearch,
        ),
    )
    .unwrap();

    let active: ActiveHandle = Arc::new(Mutex::new(None));
    let store = session_store::new_store();
    let input = b"paste-bin 6\nBinaryafter\n";
    let mut reader = BufReader::new(std::io::Cursor::new(input));
    let line = read_request_line(&mut reader).expect("paste-bin request line");
    let mut out = Vec::new();
    let mut dispatch_front_input = |event, _session| Ok(app.input(wid, event, Source::Controller));
    let mut clear_license = |_session| "OK\n".to_string();
    assert!(run_feed_bin_routed(
        &line,
        "paste-bin",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
        },
        &mut dispatch_front_input,
        &mut clear_license,
        &mut out,
    ));
    assert_eq!(String::from_utf8_lossy(&out), "OK 6 bytes\n");
    assert_eq!(
        read_request_line(&mut reader).as_deref(),
        Some("after"),
        "the following request remains framed",
    );

    let Some(crate::native_app::AppViewState::Editor(state)) = app.native_runtime.view_state(view)
    else {
        panic!("editor view remains live");
    };
    assert!(matches!(
        state.buffer.as_ref().unwrap().minibuffer,
        crate::native_editor::Minibuffer::Search { ref query, .. } if query == "Binary"
    ));
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "base\n",
        "the focused minibuffer, not the document, owns binary paste",
    );
    let _ = std::fs::remove_dir_all(dir);
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
        assert_eq!(
            ready(tail),
            "ERR usage: ready [<ms>|timeout=<ms>]\n",
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
        assert_eq!(
            momentum(tail),
            "ERR usage: await momentum <floor 0..=1> [timeout <ms>]\n",
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

/// `turn` happy path: the text is delivered once with paste semantics, one Enter
/// verifiably lands (the echo advances content_seq), the reply settles, and the
/// settled rows ride the reply.
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
        "idle=50 timeout=60000 hello peer",
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
        verdict.contains(" dur_ms=") && verdict.contains(" hash="),
        "turn verdict carries dur_ms= and hash=: {verdict}"
    );
    assert!(out.contains("response-line"), "settled rows in the reply");
    assert_eq!(
        pasted.borrow().as_slice(),
        ["hello peer"],
        "the message text (options stripped) is delivered exactly once"
    );
    assert_eq!(presses.get(), 1, "one press sufficed — no blind re-press");
}

/// `turn if-gen=<epoch>.<seq>` pastes only while the screen is still the generation
/// the caller judged, so a keystroke between the read and the turn cannot be pasted
/// in front of the text; a moved screen answers `OK 0 turn skipped reason=changed`
/// with nothing pasted or pressed. Negative controls: the current generation types,
/// and a malformed fence is the usage line before anything is typed.
#[test]
fn turn_if_gen_types_only_on_the_judged_screen() {
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
    let judged = crate::control::screen_gen(&term.lock().unwrap());
    // A person's keystroke lands between the caller's read and its turn.
    term.lock().unwrap().process(b"x");
    let registry = subscribe::new_registry();
    let out = cmd_turn(
        term,
        &store,
        0,
        &format!("if-gen={judged} idle=50 timeout=5000 hello peer"),
        &registry,
        &h.ctx,
        &io,
    );
    assert!(
        out.starts_with("OK 0 turn skipped reason=changed submitted=0 seq="),
        "{out}"
    );
    assert!(
        pasted.borrow().is_empty(),
        "nothing pasted over the moved screen"
    );
    assert_eq!(presses.get(), 0, "nothing pressed");
    // The generation read now: typed as any turn is.
    let now = crate::control::screen_gen(&term.lock().unwrap());
    let out = cmd_turn(
        term,
        &store,
        0,
        &format!("if-gen={now} idle=50 timeout=60000 hello peer"),
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
    let out = cmd_turn(
        term,
        &store,
        0,
        "if-gen=12 idle=50 hello",
        &registry,
        &h.ctx,
        &io,
    );
    assert!(out.starts_with("ERR usage: turn"), "{out}");
    assert_eq!(pasted.borrow().len(), 1, "a bad fence types nothing");
}

/// `turn if-rows=<first>-<last>:<hex16>`, the narrow fence: `if-gen=` refuses on any
/// output since the read, so a self-repainting screen (spinner, timer) would refuse
/// every time. The rows fence types while only rows outside its range moved and
/// refuses a change inside it (a keystroke echoed in the input box) with nothing
/// pasted or pressed. Negative controls: `if-gen=` over the same outside repaint
/// refuses; a range past the grid is `changed`; a malformed fence is the usage
/// line. Parity: rows from `text --json`, hashed by the client's
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
    // A malformed fence is the usage line, before anything is typed.
    let out = cmd_turn(
        term,
        &store,
        0,
        "if-rows=3-1:0123456789abcdef idle=50 hello",
        &registry,
        &h.ctx,
        &io,
    );
    assert!(out.starts_with("ERR usage: turn"), "{out}");
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
/// absolute schedule (`t0 + i·cadence`), never early and never drifting.
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
    // THE CADENCE: never early (absolute schedule), and the whole line
    // lands at n·cadence with only the sleeper's own lateness on top.
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
    let total = (seen.last().unwrap().1 - seen[0].1).as_millis();
    let nominal = u128::from(cadence_ms) * (want.len() as u128 - 1);
    assert!(
        total < nominal + 150,
        "the schedule is absolute, so lateness does not accumulate: {total} ms for {nominal} nominal ({iois:?})"
    );
}

/// The typed prefix is capped and the remainder is one paste (§28): 300 graphemes
/// are 240 presses and one paste of the other 60, which the seam voices as one
/// up-strum (`the_remainder_past_the_cap_is_one_strum_not_a_verse` in the synth).
#[test]
fn the_typed_prefix_is_capped_and_the_remainder_is_one_paste() {
    use std::cell::{Cell, RefCell};
    use std::time::Instant;

    use aterm_grapheme::GraphemeClusters;

    use crate::input::InputEvent;

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
        &format!("typed=1 cadence=1 submit=none idle=20 timeout=60000 {text}"),
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            key: &key,
            momentum: &momentum,
            driver: None,
        },
    );
    let verdict = out.lines().next().unwrap_or("");
    assert!(verdict.ends_with(" typed=240 pasted=60"), "{verdict}");
    assert_eq!(keys.get(), control_session::TYPED_CAP);
    let pasted = pasted.borrow();
    assert_eq!(pasted.len(), 1, "one paste, not one per line: {pasted:?}");
    assert_eq!(pasted[0].graphemes().count(), 60);
    assert_eq!(
        format!("{}{}", "abcde ".repeat(40), pasted[0]),
        text,
        "the paste is exactly the text past the cap"
    );
}

/// `cap=<n>` bounds a typed turn's presses without pasting its tail: the same 300
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
            },
        );
        let verdict = out.lines().next().unwrap_or("").to_string();
        (verdict, keys.get(), pasted.into_inner())
    };

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
        assert!(verdict.contains("[cap=<n>]"), "{bad}: {verdict}");
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
                h.ctx.human_input.note(crate::metrics::now_us());
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
/// signal, so the driver would retry for ever. NEGATIVE CONTROL: the
/// composer's own three rows arm it — the `rm` box drawn in the
/// composer's place stops the same text part-way.
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
    // Negative control: the frame's own rows, a box drawn over it.
    let rule = composer_rule();
    let boxed = |k: usize, c: &Composer, h: &SessionHandle| {
        if k == 5 {
            let at = c.at.get();
            h.term.lock().unwrap().process(
                format!(
                    "\x1b[?2026h\x1b[{};1H\x1b[J{rule}\r\n Do you want to proceed?\r\n ❯ 1. \
                     Yes\r\n   2. No\r\n Esc to cancel\x1b[?2026l",
                    at + 1
                )
                .as_bytes(),
            );
        }
    };
    let t = composer_turn_with(&rule, 76, "timeout=60000", text, &boxed);
    assert!(
        t.verdict.contains("skipped reason=changed typed=5 "),
        "{}",
        t.verdict
    );
    assert_eq!((t.keys.concat().as_str(), t.enters), ("echo ", 0));
}

/// A typed turn is never submitted in part. A head whose schedule ends past the
/// deadline is not begun (`typed=0`, nothing pressed), and one a slow route lets the
/// deadline overtake stops with no Enter: `skipped reason=deadline typed=<k>`, k
/// short of the text. Negative controls: a schedule that fits types all 11 and
/// submits, as does the slow route given the time.
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
    // NEGATIVE CONTROLS.
    let (verdict, keys, enters) = typed_turn_with("cadence=10 timeout=60000", false, &|_, _| {});
    assert!(verdict.contains(" submitted=1 "), "{verdict}");
    assert_eq!((keys, enters), (11, 1));
    let (verdict, keys, enters) = typed_turn_with("cadence=1 timeout=60000", false, &slow);
    assert!(verdict.contains(" submitted=1 "), "{verdict}");
    assert_eq!((keys, enters), (11, 1));
}

/// `await momentum` returns when the ribbon exhales and never polls: with the metric
/// at 0.40 and a floor of 0.30 the release law puts the crossing at
/// `2·ln(0.40/0.30)` = 575 ms, and the verb takes exactly two readings (the one it
/// solved from and the confirming one). If the human re-lights the ribbon during the
/// park, it solves the next crossing from that reading and takes a third; a ribbon
/// that never exhales answers `OK timeout` with one deadline read.
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

    // 4 — a dark ribbon returns at once, and the grammar refuses nonsense.
    let read = || Ok((0.0, Instant::now()));
    assert_eq!(
        cmd_await_momentum(
            &h.term,
            &store,
            0,
            "momentum 0.3",
            &subscribe::new_registry(),
            &read
        ),
        "OK momentum 0.00\n"
    );
    assert!(
        cmd_await_momentum(
            &h.term,
            &store,
            0,
            "momentum 1.5",
            &subscribe::new_registry(),
            &read
        )
        .starts_with("ERR usage"),
    );
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
        // A pattern that never appears within submit_window degrades to the idle
        // settle instead of latching at arm, so the reply carries output painted
        // after the submit; a wrong pattern behaves like a plain turn, and the idle
        // fallback settles, not the timeout.
        Row {
            label: "the pattern never appears: idle fallback",
            press_paint: b"\r\x1b[2Ksubmitted\r\n",
            script: &[(150, b"line-1\r\n"), (150, b"line-2\r\n")],
            options: "settle=gone:never-painted submit_window=600 idle=100 timeout=60000 do it",
            must: &["line-1", "line-2"],
            must_not: &[],
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
/// swallowed press).
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
            options: "idle=50 timeout=60000 msg",
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
            options: "idle=50 timeout=60000 submit_verify=block run",
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

/// `turn submit=none` is type-only: no press fires, `submitted=0` is reported, and
/// settle still runs so the caller gets the painted screen. A message whose first
/// word contains `=` is not eaten by the option parser.
#[test]
fn turn_submit_none_types_without_pressing_and_keeps_odd_text() {
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
        true
    };
    let out = cmd_turn(
        term,
        &store,
        0,
        "idle=50 timeout=60000 submit=none x=1 stays verbatim",
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            ..TurnIo::paste_only()
        },
    );
    assert!(
        out.contains("submitted=0 status=settled"),
        "type-only turn settles with an honest submitted=0: {}",
        out.lines().next().unwrap_or("")
    );
    assert_eq!(presses.get(), 0, "submit=none never presses");
    assert_eq!(
        pasted.borrow().as_slice(),
        ["x=1 stays verbatim"],
        "an unknown k=v token ends option parsing and stays in the text"
    );
}

/// The turn lease: a second driver colliding with an in-flight turn is refused with
/// the holder's id, the turn reports that id on its status line, and the lease is
/// released on completion so the next turn proceeds.
#[test]
fn turn_lease_refuses_concurrent_writers_and_releases() {
    use std::cell::Cell;
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;

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
        out, "ERR busy turn=41\n",
        "held lease refuses a second turn"
    );
    *h.ctx.turn_lease.lock().unwrap() = None;

    // A completed turn reports its id and leaves the lease RELEASED.
    let presses = Cell::new(0u32);
    let paste2 = |text: &str| {
        term.lock().unwrap().process(text.as_bytes());
        true
    };
    let press2 = |_: &str| {
        presses.set(presses.get() + 1);
        term.lock().unwrap().process(b"\r\nok\r\n");
        true
    };
    let out = cmd_turn(
        term,
        &store,
        0,
        "idle=50 timeout=60000 hello",
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste2,
            press: &press2,
            ..TurnIo::paste_only()
        },
    );
    assert!(
        out.contains("submitted=1 status=settled") && out.contains(" id="),
        "turn reports its lease id: {}",
        out.lines().next().unwrap_or("")
    );
    assert_eq!(
        *h.ctx.turn_lease.lock().unwrap(),
        None,
        "lease released after the turn"
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
/// Negative controls: a lapsed hard lease blocks nothing, and `lease status` says
/// `hard=1` only of a hard one.
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
    });
    assert_eq!(block(Some(8)), None, "a lapsed hard lease holds nothing");
    SERVING_CONNECTION.with(|s| s.set(None));
}

/// An expired cooperative lease reads as idle (`who`/`status` = none), is stealable
/// by a new acquire, and no longer blocks a `turn`.
#[test]
fn expired_cooperative_lease_is_free() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());

    // Plant an ALREADY-expired cooperative lease (expiry in the past).
    *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
        holder: "stale".to_string(),
        expires_us: crate::metrics::now_us().saturating_sub(1),
        conn: None,
        hard: true,
    });
    assert_eq!(
        cmd_lease(&h.ctx, "status"),
        "OK lease none\n",
        "expired reads as none"
    );
    // A fresh acquire by anyone succeeds (steals the lapsed slot).
    assert!(cmd_lease(&h.ctx, "acquire holder=fresh").starts_with("OK lease acquired"));
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

/// `family <sid>` walks the tree from an arbitrary node, so it is Owner-only at the
/// dispatch gate with the other argument-dependent escalations; the no-argument form
/// stays the read edge's.
#[test]
fn family_with_a_sid_is_owner_only_at_the_dispatch() {
    let ctx = test_ctx();
    let read = edge_granted(Op::ReadScreen, &ctx);
    assert_eq!(escalated_op("family", ""), None);
    assert_eq!(escalated_op("family", "  "), None);
    assert_eq!(
        escalated_op("family", "s-other"),
        Some(Escalation::OwnerOnly)
    );
    assert!(dispatch_authorized(read, "family", "", &ctx));
    assert!(!dispatch_authorized(read, "family", "s-other", &ctx));
    assert!(dispatch_authorized(Scope::Owner, "family", "s-other", &ctx));
}

/// `pointer click` presses band capsules, so it is Owner-only at the dispatch gate
/// like `notice act`; moving the pointer, its status and `leave` keep `Write`.
#[test]
fn pointer_click_is_owner_only_at_the_dispatch() {
    let ctx = test_ctx();
    let write = edge_granted(Op::WriteInput, &ctx);
    for rest in ["", "status", "leave", "move 1 2", "move -1 4"] {
        assert_eq!(escalated_op("pointer", rest), None, "pointer {rest:?}");
        assert!(
            dispatch_authorized(write, "pointer", rest, &ctx),
            "{rest:?}"
        );
    }
    assert_eq!(
        escalated_op("pointer", "click"),
        Some(Escalation::OwnerOnly)
    );
    assert_eq!(
        escalated_op("pointer", "  click  "),
        Some(Escalation::OwnerOnly)
    );
    assert!(!dispatch_authorized(write, "pointer", "click", &ctx));
    assert!(dispatch_authorized(Scope::Owner, "pointer", "click", &ctx));
}

/// The accept loop backs off on exhaustion: `EMFILE` returns at once on every retry,
/// so it waits 10 ms, doubling to a 1 s cap; a per-connection error (peer reset
/// before accept) is retried at once.
#[cfg(unix)]
#[test]
fn the_accept_loop_backs_off_on_descriptor_exhaustion_only() {
    use std::time::Duration;
    let emfile = std::io::Error::from_raw_os_error(libc::EMFILE);
    let mut wait = None;
    let mut seen = Vec::new();
    for _ in 0..10 {
        wait = accept_error_backoff(&emfile, wait);
        seen.push(wait.expect("exhaustion backs off"));
    }
    assert_eq!(seen[0], Duration::from_millis(10));
    assert_eq!(seen[1], Duration::from_millis(20));
    assert_eq!(*seen.last().unwrap(), Duration::from_secs(1), "capped");
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
    // The scope split it rests on, stated once: Bridge is Owner PLUS the two
    // bridge verbs, and an Edge is neither.
    assert!(Scope::Owner.is_owner_class() && Scope::Bridge.is_owner_class());
    assert!(!Scope::Edge(EdgeToken::generate()).is_owner_class());
}

/// The halt lives on the control seams only: the verb dispatch, the
/// `feed-bin`/`paste-bin` frame, the operator proposal frame and the App lane
/// (`dispatch_before_session`, answering `invoke` before any session exists), the
/// set of paths by which a socket client reaches the PTY. The keyboard's input seam
/// (`App::input`/`input.rs`) must not mention it: a fleet halt stops drivers, never
/// locks a person out of their own terminal.
#[test]
fn the_halt_gate_sits_only_on_the_socket_seams_inbox_hold() {
    // The PRODUCTION half of this file: everything before its `mod tests`, so
    // this test's own references (and the guard test's below) are not counted.
    let control = include_str!("control.rs")
        .split_once("\n#[cfg(test)]\n#[path = \"control_tests.rs\"]\nmod tests;")
        .map(|(production, _)| production)
        .expect("control.rs has a tests module");
    let calls = control.matches("fabric::halt_refusal(").count();
    assert_eq!(
        calls, 3,
        "the SESSION halt gate has exactly three production call sites: the verb \
         dispatch, the feed-bin/paste-bin frame, and the operator proposal frame"
    );
    // The operator proposal asks the session its body names (the one it would
    // paste into), never the front tab.
    assert!(
        control.contains("crate::fabric::halt_refusal(&target.ctx, \"operator-propose-bin\")"),
        "the operator proposal's halt gate reads the proposal's target session"
    );
    // The App lane is a separate gate because an App-target verb never resolves a
    // session (`invoke Paste` is answered in `dispatch_before_session`).
    //
    // Two sites: the second, in the session dispatch, covers aimed App forms
    // (`@<sid> tab …`), which `aimed_app_lane` routes past the first site so the
    // selector reaches the resolver, and which drive the hosting window rather
    // than the aimed session.
    assert_eq!(
        control.matches("fabric::app_halt_refusal(").count(),
        2,
        "the App-lane halt gate has exactly two production call sites: \
         dispatch_before_session before dispatch_app_verb, and the session \
         dispatch for the aimed App forms"
    );
    for (name, src) in [
        ("input.rs", include_str!("input.rs")),
        ("app_input.rs", include_str!("app_input.rs")),
        ("control_input.rs", include_str!("control_input.rs")),
    ] {
        assert!(
            !src.contains("halt_refusal"),
            "{name} must not gate the human input path on a fleet halt"
        );
    }
    // The set itself is derived from the verb table in
    // `crate::fabric::tests::the_halt_set_is_derived_from_the_verb_table`, which
    // walks every row whatever its target. This test is about where the halt is
    // asked, and where it must never be.
}

/// The unread-input gate (`input_stall`) lives on the socket seams only: the same
/// four the halt sits on, each after the halt (a held session says `ERR halted`
/// first), and in the dispatch after usage refusals and before the guard and the
/// `id=` claim, so a refused write consumes no sequence. The window keyboard's
/// seams must not mention it: a person typing into a frozen program is refused
/// nothing.
#[test]
fn the_unread_input_gate_sits_only_on_the_socket_seams() {
    let control = include_str!("control.rs")
        .split_once("\n#[cfg(test)]\n#[path = \"control_tests.rs\"]\nmod tests;")
        .map(|(production, _)| production)
        .expect("control.rs has a tests module");
    assert_eq!(
        control.matches("input_stall::refusal(").count()
            + control.matches("input_stall::admission(").count(),
        4,
        "the unread-input gate has exactly four production call sites: the verb \
         dispatch, the feed-bin/paste-bin frame, the operator proposal frame and \
         the App lane"
    );
    let after = |from: &str, needle: &str| -> usize {
        let base = control.find(from).unwrap_or_else(|| panic!("{from}"));
        base + control[base..]
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} after {from}"))
    };
    // The dispatch: halt < usage < this gate < guard compile < id= claim.
    let halt = after("\nfn handle(", "crate::fabric::halt_refusal(ctx, verb)");
    let usage = after("\nfn handle(", "if let Some(refusal) = leading.refusal");
    let gate = after(
        "\nfn handle(",
        "crate::input_stall::admission(ctx, verb, rest, leading.unread_ok)",
    );
    let compile = after("\nfn handle(", "control_input::compile_guard(");
    let claim = after(
        "\nfn handle(",
        "crate::pty_idem::guarded(ctx, scope, verb, idem_key.as_deref()",
    );
    assert!(
        halt < usage && usage < gate && gate < compile && compile < claim,
        "halt ({halt}) < usage ({usage}) < unread gate ({gate}) < guard ({compile}) \
         < id= claim ({claim})"
    );
    // The binary frame: after its halt, before its claim.
    let frame = "\nfn run_feed_bin_routed<";
    assert!(
        after(frame, "crate::fabric::halt_refusal(&ctx, verb)")
            < after(
                frame,
                "crate::input_stall::admission(&ctx, verb, \"\", false)"
            )
            && after(frame, "crate::input_stall::admission(")
                < after(frame, "crate::pty_idem::guarded(")
    );
    // The operator proposal: after its halt, before any durable step.
    let proposal = "\nfn run_operator_proposal(";
    assert!(
        after(
            proposal,
            "halt_refusal(&target.ctx, \"operator-propose-bin\")"
        ) < after(
            proposal,
            "input_stall::admission(&ctx, \"operator-propose-bin\""
        ) && after(proposal, "input_stall::admission(")
            < after(proposal, "operator.begin_action_activity()")
            && after(proposal, "input_stall::with_warning(resp, warning")
                < after(proposal, "\nfn run_operator_proposal_admitted(")
    );
    // The App lane: after its halt, before the verb runs.
    let lane = "\nfn dispatch_before_session(";
    assert!(
        after(lane, "crate::fabric::app_halt_refusal(store, verb)")
            < after(
                lane,
                "crate::input_stall::admission(ctx, verb, rest, unread_ok)"
            )
            && after(lane, "crate::input_stall::admission(")
                < after(lane, "dispatch_app_verb(verb, rest")
    );
    // A slow read is written and warned: each seam that asks `admission` carries
    // its warning onto its reply (the dispatch after `seq=`, the frame on
    // `OK <n> bytes`, the App lane on the gesture's reply) (owner, 2026-09-29).
    assert!(
        after(
            "\nfn handle(",
            "crate::input_stall::with_warning(stamp_input_seq(verb, resp, term), \
             warning.as_deref())"
        ) > claim
    );
    assert!(
        after(
            frame,
            "crate::input_stall::with_warning(response, warning.as_deref())"
        ) > after(frame, "crate::pty_idem::guarded(")
    );
    assert!(
        after(lane, ".map(|reply| reply.warned(warning.as_deref()))")
            > after(lane, "dispatch_app_verb(verb, rest")
    );
    for (name, src) in [
        ("input.rs", include_str!("input.rs")),
        ("app_input.rs", include_str!("app_input.rs")),
        ("control_input.rs", include_str!("control_input.rs")),
        (
            "aterm-session sink.rs",
            include_str!("../../aterm-session/src/sink.rs"),
        ),
    ] {
        assert!(
            !src.contains("input_stall::refusal(") && !src.contains("input_stall::admission("),
            "{name} must not gate the human input path on unread input"
        );
    }
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

    // The gate sits in the session dispatch, after the session halt gate and
    // before the arm that drives the hosting window; this is the assertion that
    // fails if the dispatch stops consulting it.
    let production = include_str!("control.rs")
        .split_once("\n#[cfg(test)]\n#[path = \"control_tests.rs\"]\nmod tests;")
        .map(|(production, _)| production)
        .expect("control.rs has a tests module");
    let gate = production
        .rfind("is_aimed_app_verb(verb)")
        .expect("the aimed App-lane halt gate is in the session dispatch");
    let session_gate = production
        .find("crate::fabric::halt_refusal(ctx, verb)")
        .expect("the session halt gate");
    let aimed_tab_arm = production
        .find("\"tab\" if is_cross =>")
        .expect("the aimed tab dispatch arm");
    assert!(
        session_gate < gate && gate < aimed_tab_arm,
        "the aimed App-lane halt gate belongs between the session halt gate \
         and the arm that hands the request to the hosting window",
    );
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
    // And the gate is wired: the dispatch calls the predicate.
    let production = include_str!("control.rs")
        .split_once("\n#[cfg(test)]\n#[path = \"control_tests.rs\"]\nmod tests;")
        .map(|(p, _)| p)
        .expect("control.rs has a tests module");
    assert_eq!(
        production
            .matches("lease_forges_fabric_holder(scope, rest)")
            .count(),
        1,
        "the `lease` dispatch arm must be the one production caller of the fence"
    );
}

/// Every refusal of `hold` is audited, from both dispatch entries.
///
/// The generic owner-only gate of `dispatch_before_session` and `handle` refuses
/// silently, so both entries hand `hold` to its decision function before that gate,
/// where edge-token, selector and Owner refusals are logged. This pins the order in
/// the source and that each of the three refusals is logged;
/// `the_local_owner_may_hold_its_session_and_never_a_fleet_hold_inbox_hold` drives
/// the refusals themselves.
#[test]
fn the_hold_refusals_are_audited_from_both_dispatch_entries() {
    let production = include_str!("control.rs")
        .split_once("\n#[cfg(test)]\n#[path = \"control_tests.rs\"]\nmod tests;")
        .map(|(p, _)| p)
        .expect("control.rs has a tests module");
    let decision = production
        .split_once("fn dispatch_hold_verb(")
        .and_then(|(_, after)| after.split_once("\nconst FABRIC_USAGE"))
        .map(|(body, _)| body)
        .expect("dispatch_hold_verb precedes FABRIC_USAGE");
    assert_eq!(
        decision.matches("log_denial(").count(),
        3,
        "three refusals, three audit lines"
    );
    // In each entry, the hand-off to the decision function comes BEFORE the
    // generic owner-class gate — so the gate never sees a `hold`.
    for (entry, block) in [
        ("fn dispatch_before_session(", "Access::OwnerOnly {"),
        ("fn handle(", "is_owner_only(verb) {"),
    ] {
        let body = production
            .split_once(entry)
            .map(|(_, after)| after)
            .and_then(|after| after.split_once(block))
            .map(|(_, after)| after)
            .unwrap_or_else(|| panic!("{entry} has its owner-only block"));
        let hand_off = body
            .find("dispatch_hold_verb(")
            .unwrap_or_else(|| panic!("{entry} hands `hold` to dispatch_hold_verb"));
        let gate = body
            .find("is_owner_class()")
            .unwrap_or_else(|| panic!("{entry} keeps its owner-class gate"));
        assert!(
            hand_off < gate,
            "{entry}: `hold` must reach dispatch_hold_verb BEFORE the silent gate"
        );
    }
    // One definition, one hand-off per entry, and no third caller for a
    // refusal to slip past.
    assert_eq!(
        production.matches("dispatch_hold_verb(").count(),
        3,
        "the definition and exactly two hand-offs"
    );
}

/// `@<sid> key notakey` answers [`control_input::KEY_USAGE`], not a bare `ERR`: both
/// cross-session `key` arms use the same usage line as the flagless and guarded
/// arms. Pinned on the dispatch source, since `handle()` needs a live window.
#[test]
fn cross_session_key_arms_answer_the_key_usage_line() {
    let production = include_str!("control.rs")
        .split_once("\n#[cfg(test)]\n#[path = \"control_tests.rs\"]\nmod tests;")
        .map(|(p, _)| p)
        .expect("control.rs has a tests module");
    let (_, from) = production
        .split_once("\"key\" if targets_front")
        .expect("the front-routed cross-session `key` arm");
    let (arms, _) = from
        .split_once("\"hwkey\"")
        .expect("`hwkey` follows the `key` arms");
    assert_eq!(
        arms.matches("control_input::KEY_USAGE").count(),
        2,
        "both cross-session `key` arms answer KEY_USAGE:\n{arms}"
    );
    assert!(
        !arms.contains(r#""ERR\n""#),
        "a cross-session `key` must not answer a bare ERR:\n{arms}"
    );
}

/// The front-routed cross-session `mouse` arm answers the parser's refusal, like
/// the self verb and the background cross arm (one grammar, one reply for a bad
/// modifier). Pinned on the source, since `front_routed_input` needs a live
/// `EventLoopProxy`; with `mouse_grammar_and_mods_refusal` it shows the parser
/// refuses by name and no arm discards the name.
#[test]
fn the_front_routed_cross_mouse_arm_forwards_the_parsers_refusal() {
    let production = include_str!("control.rs")
        .split_once("\n#[cfg(test)]\n#[path = \"control_tests.rs\"]\nmod tests;")
        .map(|(p, _)| p)
        .expect("control.rs has a tests module");
    let (_, from) = production
        .split_once("\"mouse\" if targets_front")
        .expect("the front-routed cross-session `mouse` arm");
    let (arm, _) = from
        .split_once("\"mouse\" if is_cross =>")
        .expect("the background cross-session `mouse` arm follows it");
    assert!(
        arm.contains("match control_input::parse_mouse(rest)"),
        "the front-routed `mouse` arm must match the parser's Result:\n{arm}"
    );
    assert!(
        arm.contains("Err(refusal) => refusal"),
        "the front-routed `mouse` arm must answer the parser's refusal verbatim:\n{arm}"
    );
    assert!(
        !arm.contains(".ok()"),
        "the front-routed `mouse` arm must not discard the refusal with `.ok()`:\n{arm}"
    );
    // The refusal that arm now forwards — the one the mouse doc promises.
    let refused = control_input::parse_mouse("press left 4 4 mods=cmd")
        .expect_err("`cmd` is not a modifier a mouse report can carry");
    assert!(
        refused.starts_with("ERR bad modifier"),
        "the parser names the modifier it refused: {refused:?}"
    );
    assert_ne!(
        refused,
        control_input::MOUSE_USAGE,
        "the refusal is specific, never the usage line the arm used to answer"
    );
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
    assert_eq!(
        post_frame_len("post to=h-a kind=note len=99999999"),
        Some(Err(
            "ERR too large: a post body is capped at 262144 bytes\n".to_string()
        ))
    );
    assert_eq!(
        post_frame_len("post to=h-a kind=note len=abc"),
        Some(Err(
            "ERR usage: post len=<n> must be a byte count\n".to_string()
        )),
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

    assert_eq!(
        feed_bin_once(
            &active,
            &store,
            &format!("@1 feed-bin 0 id={key}"),
            b"",
            &seen
        ),
        "ERR usage: id= needs a non-empty payload\n",
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

    // Both keyed seams refuse an empty payload BEFORE they claim.
    let control = include_str!("control.rs");
    let after = |from: &str, needle: &str| -> usize {
        let base = control.find(from).unwrap_or_else(|| panic!("{from}"));
        base + control[base..]
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} after {from}"))
    };
    for (frame, claim) in [
        (
            "\nfn handle(",
            "crate::pty_idem::guarded(ctx, scope, verb, idem_key.as_deref()",
        ),
        (
            "\nfn run_feed_bin_routed<",
            "crate::pty_idem::guarded(&ctx, route.scope, verb",
        ),
    ] {
        assert!(
            after(frame, "crate::pty_idem::empty_payload_refusal(") < after(frame, claim),
            "{frame}: the empty-payload refusal must precede the claim"
        );
    }
}

/// A relaunched session is a fresh epoch with an empty high-water: an id minted for
/// the dead incarnation can neither suppress a live keystroke nor apply to the new
/// shell. It is refused by name (`ERR epoch`).
#[test]
#[cfg(unix)]
fn feed_idempotent_a_relaunched_session_starts_a_fresh_high_water() {
    let store = session_store::new_store();
    let (dead, _rx_a) = pipe_session(1);
    let dead_key = format!("{}:9:1", dead.ctx.nonce.to_hex());
    store.write().unwrap().register(dead.clone());
    let active = active_for_handle(&dead);
    let seen = std::cell::RefCell::new(Vec::new());
    assert_eq!(
        feed_bin_once(
            &active,
            &store,
            &format!("@1 feed-bin 3 id={dead_key}"),
            b"ABC",
            &seen
        ),
        "OK 3 bytes\n"
    );
    assert_eq!(
        feed_bin_once(
            &active,
            &store,
            &format!("@1 feed-bin 3 id={dead_key}"),
            b"ABC",
            &seen
        ),
        "OK dup=1\n"
    );
    assert_eq!(seen.borrow().len(), 1);

    // The session is relaunched: a new launch nonce, a new endpoint.
    let store = session_store::new_store();
    let (live, _rx_b) = pipe_session(2);
    assert_ne!(live.ctx.nonce, dead.ctx.nonce, "a relaunch mints a nonce");
    store.write().unwrap().register(live.clone());
    let active = active_for_handle(&live);
    let seen = std::cell::RefCell::new(Vec::new());

    // The dead session's id is refused, not obeyed.
    assert_eq!(
        feed_bin_once(
            &active,
            &store,
            &format!("@2 feed-bin 3 id={dead_key}"),
            b"ABC",
            &seen
        ),
        crate::pty_idem::EPOCH_REFUSAL
    );
    assert!(seen.borrow().is_empty(), "a dead epoch types nothing");

    // The SAME producer and sequence under the LIVE epoch is fresh — the mark
    // did not survive the relaunch, so a live keystroke is not suppressed.
    let live_key = format!("{}:9:1", live.ctx.nonce.to_hex());
    assert_eq!(
        feed_bin_once(
            &active,
            &store,
            &format!("@2 feed-bin 3 id={live_key}"),
            b"ABC",
            &seen
        ),
        "OK 3 bytes\n",
        "a fresh epoch starts a fresh high-water"
    );
    assert_eq!(seen.borrow().len(), 1);
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
/// text", and `pty_idem`'s tests bind that sentence to `KEYED_VERBS`. This is
/// the DELIVERY half of the claim: a `paste` stamped with a well-formed key
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
    let line = format!("id={key} hi");
    assert!(!crate::pty_idem::is_keyed_verb("paste"));

    // The dispatch's scan takes nothing: the tail is the whole line.
    let (leading, rest) = control_input::take_leading_options("paste", &line);
    assert_eq!(leading.idem, None);
    assert!(leading.refusal.is_none());
    assert_eq!(rest, line);

    // The `paste` arm's event, delivered through the real sink: the key
    // lands on the PTY as text, byte for byte.
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
        line.as_bytes(),
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
}

/// Box A, then box B drawn over it: two approval boxes the text guard
/// cannot tell apart — the swap a supervisor's `key 1` must not land on.
#[cfg(unix)]
const BOX_A: &[u8] = b"\x1b[2J\x1b[H Bash command\r\n   ls -la /tmp\r\n\r\n Do you want to proceed?\r\n \xe2\x9d\xaf 1. Yes\r\n   2. No\r\n";
#[cfg(unix)]
const BOX_B: &[u8] = b"\x1b[2J\x1b[H Bash command\r\n   rm -rf /tmp/work\r\n\r\n Do you want to proceed?\r\n \xe2\x9d\xaf 1. Yes\r\n   2. No\r\n";

/// The person fence (`if-human=<n>`): after a supervisor read the person count, a
/// person's key (unread by the program, so `if-gen=` still passes) makes
/// `key if-human=<count> 1` answer `OK skipped reason=person` and write nothing,
/// ahead of the screen fences. Negative controls: fenced before the person keyed,
/// fenced on the count read after, and a screen fence alone all press.
#[test]
#[cfg(unix)]
fn a_key_fenced_on_the_person_count_never_lands_after_a_persons_key() {
    use control_input::InputFence;
    let (h, rx) = guarded_pipe_session(1);
    crate::term_lock(&h.term).process(BOX_A);
    let gen_a = screen_gen(&crate::term_lock(&h.term));
    let press = |fence: InputFence<'_>| control_input::cmd_key_fenced(&h.term, &h.ctx, &fence, "1");
    let read = h.ctx.human_input.seq();
    assert_eq!(
        press(InputFence {
            human: Some(read),
            ..InputFence::default()
        }),
        "OK\n",
        "nobody keyed: the fence holds"
    );
    assert_eq!(drain_pipe(&rx), b"1");
    // A person's key reaches the session (the App seam's stamp).
    h.ctx.human_input.note(crate::metrics::now_us());
    for fence in [
        InputFence {
            human: Some(read),
            ..InputFence::default()
        },
        InputFence {
            human: Some(read),
            generation: Some(gen_a),
            ..InputFence::default()
        },
    ] {
        assert_eq!(press(fence), "OK skipped reason=person\n");
        assert!(
            drain_pipe(&rx).is_empty(),
            "a person keyed: nothing written"
        );
    }
    // Negative controls: the screen fence alone cannot see it, and the count read
    // after the person's key presses.
    assert_eq!(
        press(InputFence {
            generation: Some(gen_a),
            ..InputFence::default()
        }),
        "OK\n"
    );
    assert_eq!(drain_pipe(&rx), b"1");
    assert_eq!(
        press(InputFence {
            human: Some(h.ctx.human_input.seq()),
            ..InputFence::default()
        }),
        "OK\n"
    );
    assert_eq!(drain_pipe(&rx), b"1");
}

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
    let seq_b = gen_b.seq;
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
    // The dispatch stamps the skip like every input reply.
    assert_eq!(
        stamp_input_seq("key", "OK skipped reason=changed\n".to_string(), &h.term),
        format!("OK skipped reason=changed seq={seq_b}\n")
    );

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

/// A `reason=changed` skip writes nothing, so it gives its `id=` sequence back: the
/// retry, fenced on the current screen, is a first attempt that presses.
#[test]
#[cfg(unix)]
fn a_changed_skip_gives_the_idempotency_sequence_back() {
    use control_input::InputFence;
    let (h, rx) = guarded_pipe_session(1);
    crate::term_lock(&h.term).process(BOX_A);
    let gen_a = screen_gen(&crate::term_lock(&h.term));
    crate::term_lock(&h.term).process(BOX_B);
    let gen_b = screen_gen(&crate::term_lock(&h.term));
    let key = format!("{}:9:1", h.ctx.nonce.to_hex());
    let attempt = |generation: ScreenGen| {
        crate::pty_idem::guarded(&h.ctx, Scope::Owner, "key", Some(key.as_str()), || {
            control_input::cmd_key_fenced(
                &h.term,
                &h.ctx,
                &InputFence {
                    generation: Some(generation),
                    ..InputFence::default()
                },
                "1",
            )
        })
    };
    assert_eq!(attempt(gen_a), "OK skipped reason=changed\n");
    assert!(drain_pipe(&rx).is_empty());
    assert_eq!(attempt(gen_b), "OK\n", "the sequence was given back");
    assert_eq!(drain_pipe(&rx), b"1");
    assert_eq!(attempt(gen_b), "OK dup=1\n");
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
        stamp_input_seq("key", reply, &term),
        format!("OK skipped reason=person seq={seq}\n")
    );
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
        line.ends_with(
            " attention_owner=human attention_owners=2 supervisor=- supervisor_proven=0 questions=- \
             agent_cwd=- autopilot=- land_at=-\n"
        ),
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
    assert!(cmd_meta(&h.term, &store, 0, &h.ctx, "").0.ends_with(
        " supervisor=sup-a supervisor_proven=0 questions=- agent_cwd=- autopilot=- land_at=-\n"
    ));
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

/// The `trail` verb's help lists exactly the keys `trail status` emits, the only
/// place a driver learns them. Set equality: a key dropped from the row fails as
/// loudly as one added.
#[test]
fn trail_status_help_enumerates_exactly_the_keys_the_row_emits() {
    use aterm_effects::cursor_glow::{
        AdmissionTally, BlockFill, BlockFillOwner, GlowStyle, TrailStatus,
    };

    let row = TrailStatus {
        style_raw: "rainbow kitty pet",
        style: GlowStyle::RainbowKitty,
        config_enabled: true,
        effective: true,
        focused: true,
        motion_stage: "full",
        motion_mode: "auto",
        shed: 1.0,
        intensity: 0.7,
        sound_seam: true,
        tally: AdmissionTally {
            licensed: 8,
            declined: 1,
            last_decline_reason: None,
        },
        spawns: 8,
        ribbon_look: "underline",
        ribbon_segments: 4,
        ribbon_hue_bands: 4,
        ribbon_drawn: 4,
        ribbon_curtain_ms: None,
        field: 1.0,
        sparks: 4,
        momentum: 0.5,
        momentum_display: 0.44,
        momentum_glow: 0.0,
        glow_active: true,
        pet_active: true,
        pet_action: "purr",
        pet_content: 0.42,
        pet_pending: 1,
        pet_focus: "rest",
        pet_reason: "quiet",
        pet_anchor: None,
        pet_event_seq: 0,
        pet_pose: "pet_sit",
        pet_body: Some((12, 68, 30, 64)),
        cat_active: false,
        // A body PRESENT, so the fields that only a claimed caret fills are
        // in the row being compared rather than silently absent.
        block_fill: Some(BlockFill {
            owner: BlockFillOwner::Rainbow,
            fill: 0x0000_F022,
            base: Some(0x0000_FF00),
        }),
        // A run in progress, so `flow=`/`combo=`/`combo_best=` are in the
        // row being compared rather than silently absent.
        flow: aterm_effects::rainbow_kitty::Flow {
            heat: 1.0,
            combo: 24,
            best: 24,
        },
        inserts: aterm_effects::cursor_glow::InsertTally {
            delivered: 2,
            lit: 1,
            retracted: 1,
            last_cells: 63,
        },
        in_flight: aterm_effects::cursor_glow::InFlightTally {
            licensed: 1,
            forgotten: 2,
            credits: 3,
            swallowed_no_echo: 4,
            park_returns: 5,
            park_flushed: 6,
        },
    }
    .line();
    let emitted = row
        .split_whitespace()
        .filter_map(|token| token.split_once('='))
        .map(|(key, _)| key)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        emitted.contains("block_fill") && emitted.len() > 20,
        "the fixture stopped producing a full row: {row}"
    );

    // The help spells the row as one backticked run beginning `trail style=`;
    // `help_line()` (summary + detail) is the bytes `help --full` and
    // `help trail` print.
    let help = aterm_types::control_verbs::spec("trail")
        .expect("the trail verb ships")
        .help_line();
    let run = help
        .split_once("`trail style=")
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(run, _)| run)
        .expect("help documents the `trail status` row");
    let documented = std::iter::once("style")
        .chain(
            run.split_whitespace()
                .filter_map(|token| token.strip_suffix('=')),
        )
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        documented,
        emitted,
        "`trail status` help and the row it documents disagree:\n  \
         help-only: {:?}\n  row-only: {:?}",
        documented.difference(&emitted).collect::<Vec<_>>(),
        emitted.difference(&documented).collect::<Vec<_>>(),
    );
}

/// The same contract for `tone`, the audibility oracle, including `seam=` and its
/// six witnesses. Set equality both ways.
#[test]
fn tone_status_help_enumerates_exactly_the_keys_the_row_emits() {
    let row = crate::tone_status::ToneStatus {
        tone: aterm_effects::tone::Tone::Technical,
        effective: aterm_effects::tone::Tone::Technical,
        knob: true,
        sounds: true,
        volume: 0.4,
        audio: crate::trail_audio::HostState::Running,
        active: true,
        window_chars: 12,
        inferences: 3,
        dropped: 0,
        seam: None,
        engine_sound: true,
        trail: true,
        focused: true,
        serious_sound: true,
        motion_stage: "full",
        shed: 1.0,
        revives: 0,
        reopens_left: 6,
    };
    let line = row.line();
    let emitted = line
        .split_whitespace()
        .filter_map(|pair| pair.split_once('=').map(|(k, _)| k))
        .collect::<std::collections::BTreeSet<_>>();

    let help = aterm_types::control_verbs::spec("tone")
        .expect("the tone verb ships")
        .help_line();
    let run = help
        .split_once("(prints tone=")
        .and_then(|(_, rest)| rest.split_once(';'))
        .map(|(run, _)| run)
        .expect("help documents the `tone` row");
    let documented = std::iter::once("tone")
        .chain(
            run.split_whitespace()
                .filter_map(|token| token.strip_suffix('=')),
        )
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        documented,
        emitted,
        "`tone` help and the row it documents disagree:\n  \
         help-only: {:?}\n  row-only: {:?}",
        documented.difference(&emitted).collect::<Vec<_>>(),
        emitted.difference(&documented).collect::<Vec<_>>(),
    );
}
