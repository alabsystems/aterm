// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// How long a test waits for something that must happen: a hang detector, not a latency
/// budget, so a passing test waits only as long as the event takes.
const EVENTUALLY: std::time::Duration = std::time::Duration::from_secs(60);

/// A pid alive on every host and never this process: 1 (init/launchd) on Unix, 4
/// (System) on Windows, where pid 1 is not a process
/// (`identity_claim::tests::LIVE_FOREIGN_PID`).
const LIVE_FOREIGN_PID: u32 = if cfg!(windows) { 4 } else { 1 };

/// A discovery entry a live foreign instance would have written.
fn live_foreign_entry() -> String {
    format!("sock /nonexistent/aterm.sock\nnonce ab\npid {LIVE_FOREIGN_PID}\n")
}

/// [`proxy_forward_plan`] for a request line, read as a lane reads it.
fn plan_for(line: &str, scope: Scope, store: &Store, dir: &std::path::Path) -> Option<String> {
    proxy_forward_plan(&Request::parse(line.to_string()), scope, store, dir)
}

/// [`is_wait_request`] for a request line, read as a lane reads it.
fn request_waits(line: &str, scope: Scope, store: &Store) -> bool {
    is_wait_request(&Request::parse(line.to_string()), scope, store)
}

/// [`run_feed_bin_routed`] for a counted frame's header line, read as a lane
/// reads it.
#[cfg(unix)]
fn feed_bin_routed<W: Write, F>(
    line: &str,
    reader: &mut impl BufRead,
    route: FeedBinRoute<'_>,
    dispatch_front_input: &mut F,
    writer: &mut W,
) -> bool
where
    F: FnMut(InputEvent, Option<u64>) -> Result<InputOutcome, String>,
{
    let request = Request::parse(line.to_string());
    let frame = request.frame().expect("a counted frame's header");
    run_feed_bin_routed(&request, frame, reader, route, dispatch_front_input, writer)
}

/// [`deliver`] by the background route, as a `verb`'s arm calls it for a session
/// not on screen: the seam is never asked.
#[cfg_attr(not(unix), allow(dead_code))] // its tests are unix-only
fn background(
    verb: &str,
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: InputEvent,
) -> Result<InputOutcome, String> {
    deliver(
        InputRoute::Background,
        input_of(verb),
        |_, _, _| panic!("the background route never takes the seam"),
        term,
        ctx,
        ev,
        None,
    )
}

/// The ambiguity refusal belongs to the resolver, so seams that skip
/// `dispatch_request` cannot skip it: `post` with a binary body, `feed-bin`/
/// `paste-bin` at the header fence and the effect boundary, and `subscribe`'s
/// target list all resolve without it, and they write input and stream output. The gate lives in
/// [`resolve`], whose [`Resolution`] hands no seam a hosted target past it.
///
/// This pins the resolver's three answers; the type checker ensures every seam
/// is bound by them.
#[test]
fn an_ambiguous_id_is_refused_by_the_resolver_every_seam_shares() {
    use super::{Resolution, Selector, resolve};
    use aterm_session::{LaunchNonce, SessionId};

    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    let _override = crate::identity_claim::rendezvous_override(dir.path());

    // A hosted session under an adopted id: the only kind that can be doubly held.
    let store = crate::session_store::new_store();
    let mut handle = crate::session_store::test_handle(1);
    let sid = SessionId::generate();
    handle.sid = sid.clone();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    crate::identity_claim::note_adopted(&sid);
    let sel = Selector::parse(sid.as_str());

    // Our own entry, nobody else's: the ordinary cross-session address.
    crate::proxy::write_graph_entry(
        dir.path(),
        &sid,
        "/nonexistent/aterm.sock",
        &LaunchNonce::generate(),
    );
    assert!(
        matches!(resolve(&store, &sel), Resolution::Hosted(_)),
        "the gate must not refuse the ids this instance published"
    );

    // A live foreign instance publishing the same id (`LIVE_FOREIGN_PID`: alive on every
    // host, never us).
    std::fs::write(
        dir.path().join("graph").join(sid.as_str()),
        live_foreign_entry(),
    )
    .expect("plant a foreign entry");
    let Resolution::Ambiguous(refusal) = resolve(&store, &sel) else {
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
        matches!(
            resolve(&store, &Selector::parse(stranger.as_str())),
            Resolution::Missing
        ),
        "an id we do not host is a miss, not an ambiguity"
    );
}

/// An id handed to this process's own update candidate is served here until the Commit:
/// the successor publishes every carried id before the predecessor commits. The real
/// refusal must equal the `HandoffAddressOwner` model's `refused` in every state.
#[cfg(unix)]
#[test]
fn an_id_handed_to_this_processs_update_candidate_is_served_here_until_commit() {
    use super::{Resolution, Selector, ambiguous_sid_refusal, resolve};
    use crate::identity_claim::project_handoff_address_owner as project;
    use aterm_session::SessionId;

    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    let _override = crate::identity_claim::rendezvous_override(dir.path());
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
    let sel = Selector::parse(sid.as_str());

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
        let resolved = resolve(&store, &sel);
        assert_eq!(
            refusal.is_some(),
            matches!(resolved, Resolution::Ambiguous(_)),
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

    // The lane registered its candidate: served, by the dispatch probe and the resolver.
    let reg = crate::identity_claim::register_handoff_candidate(cand);
    assert!(conforms(&model, "registered").is_none());
    assert!(
        matches!(resolve(&store, &sel), Resolution::Hosted(_)),
        "served here until the Commit"
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

    // A minted id is never contestable, whatever its entry says.
    let minted = SessionId::generate();
    std::fs::write(
        dir.path().join("graph").join(minted.as_str()),
        format!("sock /nonexistent/aterm.sock\nnonce ab\npid {cand}\n"),
    )
    .expect("plant the entry");
    assert!(ambiguous_sid_refusal(&Selector::parse(minted.as_str())).is_none());

    // The candidate dead (a rejected attempt's reap): its entry holds nothing.
    let _ = candidate.kill();
    let _ = candidate.wait();
    assert!(conforms(&model, "dead").is_none());
}

/// An explicit selector naming the tab on screen (`@self` expands to `@<sid>`) routes
/// through the App input seam, not straight to the sink, or it skips that seam's effects.
#[test]
fn an_explicit_selector_for_the_front_tab_is_routed_through_the_input_seam() {
    use super::{InputRoute, front_routed};
    // Named the tab on screen -> the seam, whoever asks.
    for owner_class in [true, false] {
        assert!(front_routed(true, owner_class, Some(7), 7));
        // Named a background tab -> the direct egress.
        assert!(!front_routed(true, owner_class, Some(7), 9));
        // No front tab (headless, every tab closed) -> the background path.
        assert!(!front_routed(true, owner_class, None, 7));
    }
    // An owner's flagless request follows the front.
    assert!(!front_routed(false, true, Some(7), 7));
    assert!(!front_routed(false, true, None, 7));
    // A scoped caller's flagless input is pinned to the session its edge was checked
    // against (`front_routed`).
    assert!(front_routed(false, false, Some(7), 7));
    assert!(front_routed(false, false, Some(9), 7));

    // One route per request, which every event of a composite `turn` takes, so
    // paste and Return cannot part. Explicit `@self` (which arrives as a cross
    // selector naming the visible sid) is the pinned seam, as is a scoped caller's
    // flagless request, while a genuinely hidden target stays on the background
    // sink. The Owner's flagless request takes the seam: a gesture follows the front
    // (an overlay in front may take it), the program's own bytes stay pinned to the
    // session (`Input::PINNED`: `send`, `feed`, the frames). Negative control: the
    // same flagless Owner request of a gesture and of a pinned verb route apart.
    let pinned = InputRoute::Seam { pin: Some(7) };
    let follows = InputRoute::Seam { pin: None };
    for verb in [
        "key", "ctrl", "paste", "mouse", "focus", "resize", "scroll", "turn",
    ] {
        let class = input_of(verb);
        assert_eq!(InputRoute::of(class, true, true, 7), pinned, "{verb}");
        assert_eq!(
            InputRoute::of(class, true, false, 7),
            InputRoute::Background,
            "{verb}"
        );
        assert_eq!(InputRoute::of(class, false, true, 7), pinned, "{verb}");
        assert_eq!(InputRoute::of(class, false, false, 7), follows, "{verb}");
    }
    for verb in ["send", "feed", "feed-bin", "paste-bin"] {
        let class = input_of(verb);
        assert_eq!(InputRoute::of(class, true, true, 7), pinned, "{verb}");
        assert_eq!(
            InputRoute::of(class, true, false, 7),
            InputRoute::Background,
            "{verb}"
        );
        assert_eq!(InputRoute::of(class, false, false, 7), pinned, "{verb}");
    }

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

/// A verb's answer queued as a connection queues it
/// ([`control_lanes::Outbox::attach`]).
fn outbox_of(reply: ControlReply) -> control_lanes::Outbox {
    let mut outbox = control_lanes::Outbox::default();
    outbox.attach(reply);
    outbox
}

/// Deliver `outbox` into `writer` waiting, as a lane with no parker and the
/// bridge deliver to their socket, a write timeout being nothing to arm.
fn deliver_waiting(
    outbox: &mut control_lanes::Outbox,
    writer: &mut impl Write,
) -> std::io::Result<bool> {
    outbox.deliver_to(writer, false, |_| Ok(()))
}

/// The acknowledgement a delivered guarded answer awaits, read waiting from
/// `server` and judged, as a lane with no parker and the bridge read it.
fn await_ack(outbox: &mut control_lanes::Outbox, server: &CtlStream) -> ArtifactAckOutcome {
    let mut reader = BufReader::new(server);
    outbox
        .finish_waiting(server, &mut reader)
        .expect("the answer is already out")
        .expect("an acknowledgement is owed")
}

#[test]
fn artifact_handoff_permits_bound_and_release_capacity() {
    const CHARGE: usize = 2;
    const DESCRIPTOR_LIMIT: usize = 6;
    let pool = Arc::new(UnitBudget::new(
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        observe_artifact_handoff,
    ));
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
    // `try_take`, a reconcile or a drop left. The charge buckets and
    // `selected` stay drive coordinates (the pool has no per-permit view).
    let validate = |action: &str, before: CapacityState, after: CapacityState| {
        let usage = pool.usage();
        assert_eq!(
            (usage.live, usage.units),
            (after.0, after.1),
            "{action}: the pool the shipping operation left"
        );
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
        permits.push(pool.try_take(CHARGE).unwrap());
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
        pool.usage(),
        UnitUsage {
            live: descriptor_slots,
            units: DESCRIPTOR_LIMIT
        }
    );
    assert!(pool.try_take(CHARGE).is_none());
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
            let usage = pool.usage();
            (usage.live, usage.units)
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
    assert_eq!(pool.usage(), UnitUsage::default());

    let mut reconciled = pool.try_take(CHARGE).expect("provisional charge fits");
    validate(
        "Acquire",
        (0, 0, [0, 0, 0], CHARGE),
        (1, CHARGE, [0, 1, 0], CHARGE),
    );
    assert!(reconciled.reconcile(CHARGE + 1));
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
    assert_eq!(pool.usage(), UnitUsage::default());
    // The same drop returning the provisional charge — a reconcile that
    // never recorded the grown charge on the permit — strands one unit.
    rejects(
        "Release",
        (1, CHARGE + 1, [0, 0, 1], CHARGE + 1),
        (0, 1, [0, 0, 0], CHARGE + 1),
        "artifact handoff provisional-release negative control",
    );

    let mut reconciled = pool
        .try_take(CHARGE)
        .expect("second provisional charge fits");
    assert!(reconciled.reconcile(CHARGE - 1));
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
    assert_eq!(pool.usage(), UnitUsage::default());

    let in_memory = pool
        .try_take(CHARGE)
        .expect("byte reply provisional charge fits");
    struct AccountedCapabilityProbe {
        pool: Arc<UnitBudget>,
        expected_units: usize,
    }

    impl Drop for AccountedCapabilityProbe {
        fn drop(&mut self) {
            assert_eq!(
                self.pool.usage().units,
                self.expected_units,
                "the accounted capability must close before its charge is given back"
            );
        }
    }

    let in_flight = Arc::new(Slots::new(1));
    let retention = ReplyRetentionPermit {
        in_flight: in_flight.try_take().expect("a place in flight"),
        descriptors: Some(in_memory),
    }
    .release_path_and_retain_in_memory(AccountedCapabilityProbe {
        pool: Arc::clone(&pool),
        expected_units: CHARGE,
    });
    assert_eq!(
        pool.usage(),
        UnitUsage::default(),
        "dropping the path capability gives back its whole descriptor chain"
    );
    validate(
        "Release",
        (1, CHARGE, [0, 1, 0], CHARGE),
        (0, 0, [0, 0, 0], CHARGE),
    );
    assert_eq!(in_flight.used(), 1, "the body keeps its place in flight");
    drop(retention);
    assert_eq!(in_flight.used(), 0, "and gives it back once flushed");

    let mut grown = pool
        .try_take(CHARGE)
        .expect("mixed-charge first permit fits");
    assert!(grown.reconcile(CHARGE + 1));
    let ordinary = pool
        .try_take(CHARGE)
        .expect("mixed-charge second permit fits");
    validate("Acquire", (1, 3, [0, 0, 1], 3), (2, 5, [0, 1, 1], 2));
    drop(ordinary);
    validate("Release", (2, 5, [0, 1, 1], 2), (1, 3, [0, 0, 1], 2));
    validate("SelectThree", (1, 3, [0, 0, 1], 2), (1, 3, [0, 0, 1], 3));
    drop(grown);
    validate("Release", (1, 3, [0, 0, 1], 3), (0, 0, [0, 0, 0], 3));
    assert_eq!(pool.usage(), UnitUsage::default());

    let mut small = pool
        .try_take(CHARGE)
        .expect("far-refusal selected permit fits");
    assert!(small.reconcile(1));
    let ordinary_a = pool
        .try_take(CHARGE)
        .expect("far-refusal first competing permit fits");
    let ordinary_b = pool
        .try_take(CHARGE)
        .expect("far-refusal second competing permit fits");
    assert_eq!(pool.usage(), UnitUsage { live: 3, units: 5 });
    assert!(
        !small.reconcile(3),
        "a direct one-to-three jump cannot consume two units of one-unit headroom"
    );
    assert_eq!(
        pool.usage(),
        UnitUsage { live: 3, units: 5 },
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
    assert_eq!(pool.usage(), UnitUsage::default());

    let count_pool = Arc::new(UnitBudget::new(
        ARTIFACT_HANDOFF_LIMIT,
        usize::MAX,
        observe_artifact_handoff,
    ));
    let mut count_permits = Vec::new();
    for _ in 0..ARTIFACT_HANDOFF_LIMIT {
        count_permits.push(
            count_pool
                .try_take(1)
                .expect("count slot remains available"),
        );
    }
    assert!(
        count_pool.try_take(1).is_none(),
        "object count refuses work independently of descriptor capacity"
    );
    drop(count_permits);
    assert_eq!(count_pool.usage(), UnitUsage::default());

    // Partial headroom: a charge past what is left of the descriptor budget is
    // refused before the object-count cap, and a single charge past the whole
    // budget is refused outright (the headroom subtraction cannot underflow).
    let deep = pool
        .try_take(DESCRIPTOR_LIMIT - 2)
        .expect("one deep request fits the descriptor budget");
    assert!(
        pool.try_take(3).is_none(),
        "descriptor saturation refuses work before the object-count cap"
    );
    drop(deep);
    assert!(
        pool.try_take(DESCRIPTOR_LIMIT + 1).is_none(),
        "a single over-budget descriptor charge must be refused"
    );
    assert_eq!(pool.usage(), UnitUsage::default());
}

/// Video retention is a reservation of its own: path artifacts at their cap
/// leave every armed video's sweep its capacity, and videos at theirs leave
/// path artifacts theirs. Each budget refuses at its own cap (the control).
#[test]
fn video_retention_is_reserved_apart_from_path_artifacts() {
    let budgets = ArtifactBudgets::shipping();
    let path = std::path::Path::new("/captures/shot.png");
    let paths: Vec<_> = std::iter::from_fn(|| budgets.try_reserve_for_path(path)).collect();
    assert_eq!(
        paths.len(),
        ARTIFACT_HANDOFF_LIMIT,
        "path artifacts fill their cap"
    );
    let videos: Vec<_> =
        std::iter::from_fn(|| budgets.try_reserve_video_retention_for_path(path)).collect();
    assert_eq!(
        videos.len(),
        ARTIFACT_HANDOFF_LIMIT,
        "path artifacts at their cap leave every video its slot"
    );
    drop(paths);
    assert!(
        budgets.try_reserve_for_path(path).is_some(),
        "videos at their cap leave path artifacts theirs"
    );
    drop(videos);
    assert_eq!(
        budgets.video.usage(),
        UnitUsage::default(),
        "each video gives back its slot and its units"
    );
}

/// How long a test waits for the quarantine reaper to drop a guard whose quarantine has
/// run out: the house hang detector. Each wait also drives the sweep from the test thread
/// (`sweep_artifact_quarantine_for_test`).
const REAPER_PATIENCE: std::time::Duration = EVENTUALLY;

/// A quarantine a test holds while it looks at it: a day, so the guard is still in
/// quarantine whenever the test reads it, however late the thread runs. [`end_quarantine`]
/// ends it; a short grace would race the reaper.
const HELD_QUARANTINE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// The ticket of the guard this thread put in quarantine last, its grace now
/// held for [`HELD_QUARANTINE`] (the shipping 30 s, re-dated before the test
/// looks): [`end_quarantine`] ends it.
fn last_quarantine_ticket() -> u64 {
    let ticket = super::LAST_QUARANTINE_TICKET
        .with(std::cell::Cell::take)
        .expect("this thread quarantined a guard");
    let quarantine = artifact_reply_quarantine();
    let mut entries = quarantine
        .entries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    entries
        .iter_mut()
        .find(|entry| entry.ticket == ticket)
        .expect("the quarantine is still held")
        .until = std::time::Instant::now() + HELD_QUARANTINE;
    ticket
}

/// End quarantine `ticket`'s grace now and ring the reaper: the shipping expiry path with
/// only this entry's deadline moved.
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

/// Drive a path artifact's reservation through a real failed ACK. Its
/// descriptor chain must remain charged while its guard is in the
/// process-global quarantine, and return only when the quarantine's expiry
/// sweep drops that guard; its place in flight returns at the failed ACK.
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
    let budgets = ArtifactBudgets::shipping();
    let path = std::path::Path::new("/captures/shot.png");
    let retain = |alive: &Arc<AtomicBool>| {
        budgets.try_reserve_for_path(path).map(|permit| {
            permit.retain(WireProbe {
                alive: Arc::clone(alive),
                prepared: Arc::new(AtomicBool::new(false)),
                fail_prepare: false,
            })
        })
    };
    let probe = || Arc::new(AtomicBool::new(true));

    let alive = probe();
    let retention = retain(&alive).expect("first production permit is available");
    let mut outbox = outbox_of(ControlReply::guarded(
        "OK guarded\n".to_string(),
        Some(retention),
    ));
    assert!(deliver_waiting(&mut outbox, &mut Vec::new()).expect("wire write"));
    let (mut client, server) = CtlStream::pair().unwrap();
    client.write_all(b"ACK wrong\n").unwrap();
    client.flush().unwrap();
    assert_eq!(
        await_ack(&mut outbox, &server),
        ArtifactAckOutcome::AcknowledgementQuarantined
    );
    // Held for a day: expiry is the sweep's instant below, never elapsed time.
    last_quarantine_ticket();
    assert!(alive.load(Ordering::Acquire));
    assert_eq!(
        budgets.in_flight.used(),
        0,
        "the failed ACK gives back the capture's place in flight"
    );

    let mut admitted = Vec::new();
    for _ in 1..ARTIFACT_HANDOFF_LIMIT {
        admitted
            .push(retain(&probe()).expect("capacity below the quarantined reply remains usable"));
    }
    assert!(
        retain(&probe()).is_none(),
        "quarantine must consume its production permit"
    );

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
    assert!(retain(&probe()).is_none(), "and so is its permit");
    drop(admitted);

    let expired = sweep(std::time::Instant::now() + HELD_QUARANTINE);
    assert_eq!(expired.len(), 1, "the expired reply leaves the quarantine");
    drop(expired);
    assert!(!alive.load(Ordering::Acquire), "its guard is released");
    let reopened: Vec<_> = (0..ARTIFACT_HANDOFF_LIMIT)
        .map(|_| retain(&probe()).expect("expired quarantine returns every permit"))
        .collect();
    drop(reopened);
}

/// Path captures whose acknowledgements failed sit in quarantine with their
/// descriptor chains, but none keeps a place in flight, so an in-memory
/// `--bytes` capture is admitted while they wait. Negative control: at that
/// same moment the budget the quarantine holds refuses a path capture; and
/// while the captures were still in flight, `--bytes` was refused too, which
/// is what keeps captures off every request lane.
#[test]
fn a_quarantined_path_capture_does_not_block_bytes() {
    let budgets = ArtifactBudgets::shipping();
    let path = std::path::Path::new("/captures/shot.png");
    let in_flight: Vec<_> = (0..ARTIFACT_HANDOFF_LIMIT)
        .map(|_| budgets.try_reserve_for_path(path).expect("below the caps"))
        .collect();
    assert!(
        budgets.try_reserve_in_memory().is_none(),
        "captures in flight fill every place, the in-memory kind included"
    );

    let mut tickets = Vec::new();
    for permit in in_flight {
        let mut outbox = outbox_of(guarded_probe_reply(permit.retain(WireProbe {
            alive: Arc::new(AtomicBool::new(true)),
            prepared: Arc::new(AtomicBool::new(false)),
            fail_prepare: false,
        })));
        assert!(deliver_waiting(&mut outbox, &mut Vec::new()).expect("wire write"));
        let (mut client, server) = CtlStream::pair().unwrap();
        client.write_all(b"ACK wrong\n").unwrap();
        client.flush().unwrap();
        assert_eq!(
            await_ack(&mut outbox, &server),
            ArtifactAckOutcome::AcknowledgementQuarantined
        );
        tickets.push(last_quarantine_ticket());
    }

    assert!(
        budgets.try_reserve_for_path(path).is_none(),
        "the quarantined chains keep refusing path captures (fail-closed)"
    );
    assert!(
        budgets.try_reserve_in_memory().is_some(),
        "a quarantined capture holds no place in flight: --bytes is admitted"
    );
    for ticket in tickets {
        end_quarantine(ticket);
    }
}

/// A peer that takes `limit` bytes and then breaks: every write past them
/// fails, as a socket whose peer went away does.
struct FailPast {
    limit: usize,
    bytes: Vec<u8>,
}

impl Write for FailPast {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let room = self.limit - self.bytes.len();
        if room == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "injected write failure",
            ));
        }
        let taken = room.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..taken]);
        Ok(taken)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn in_memory_handoff_releases_after_flush_without_ack_or_quarantine() {
    let budgets = ArtifactBudgets {
        in_flight: Arc::new(Slots::new(1)),
        ..ArtifactBudgets::shipping()
    };

    struct UsageCheckingWriter {
        in_flight: Arc<Slots>,
        bytes: usize,
        fail_flush: bool,
    }

    impl Write for UsageCheckingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            assert_eq!(
                self.in_flight.used(),
                1,
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
                self.in_flight.used(),
                1,
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
        let permit = budgets
            .try_reserve_in_memory()
            .expect("write-only handoff capacity");
        let retention = permit.release_path_and_retain_in_memory(());
        ControlReply::with_handoff(
            "x".repeat(1024 * 1024),
            Some(ReplyHandoff::WriteOnly(retention)),
        )
    };

    let mut writer = UsageCheckingWriter {
        in_flight: Arc::clone(&budgets.in_flight),
        bytes: 0,
        fail_flush: false,
    };
    let mut outbox = outbox_of(make_reply());
    assert!(deliver_waiting(&mut outbox, &mut writer).expect("write-only reply"));
    assert!(
        !outbox.holds_handoff_for_test(),
        "an in-memory reply has no pending ACK"
    );
    assert_eq!(writer.bytes, 1024 * 1024);
    assert_eq!(budgets.in_flight.used(), 0);

    let mut failing = UsageCheckingWriter {
        in_flight: Arc::clone(&budgets.in_flight),
        bytes: 0,
        fail_flush: true,
    };
    let error = match deliver_waiting(&mut outbox_of(make_reply()), &mut failing) {
        Err(error) => error,
        Ok(_) => panic!("flush failure must propagate without quarantine"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    assert_eq!(
        budgets.in_flight.used(),
        0,
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
    let retention = ReplyRetentionPermit::from_own_budget().release_path_and_retain_in_memory(());
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
    let mut outbox = outbox_of(inline);
    assert!(
        outbox
            .deliver_to(&mut inline_wire, false, |value| {
                *setter_timeout.lock().unwrap() = value;
                Ok(())
            })
            .unwrap()
    );
    assert!(!outbox.holds_handoff_for_test());
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
        outbox_of(ordinary)
            .deliver_to(&mut ordinary_wire, false, |value| {
                *setter_timeout.lock().unwrap() = value;
                Ok(())
            })
            .unwrap()
    );
    assert_eq!(ordinary_wire.bytes, b"OK next\n");
}

/// A reply carrying a [`ReplyWritten`] releases its [`ReplyWire`] once its bytes are
/// written and flushed (not before, and not only at a guarded reply's ACK), and on
/// a failed write too; a seamless update's Commit waits on this for a `video` reply.
/// A reply dropped unwritten releases it as well, queued or not.
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
    let mut outbox = outbox_of(reply);
    assert!(!wire.is_released(), "owed until written");
    assert!(deliver_waiting(&mut outbox, &mut writer).unwrap());
    assert!(writer.flushed && wire.is_released(), "written, so released");
    assert!(wire.wait_released(std::time::Instant::now()));

    // A failed write releases it too: nothing more is coming.
    let (wire, written) = ReplyWire::new();
    let reply = ControlReply::from("OK a\n").releasing_on_write(written);
    let mut failing = FailPast {
        limit: 0,
        bytes: Vec::new(),
    };
    assert!(deliver_waiting(&mut outbox_of(reply), &mut failing).is_err());
    assert!(wire.is_released());

    // Through `serve_session_verb`'s tail, as the `image` arm hands it on.
    let (wire, written) = ReplyWire::new();
    let mut tail = ReplyTail::default();
    let body = ControlReply::from("OK b\n")
        .releasing_on_write(written)
        .into_body_retaining(&mut tail);
    assert!(!wire.is_released(), "the tail keeps it owed");
    let mut bytes = Vec::new();
    assert!(
        deliver_waiting(
            &mut outbox_of(ControlReply::with_tail(body, tail)),
            &mut bytes
        )
        .unwrap()
    );
    assert_eq!(bytes, b"OK b\n");
    assert!(wire.is_released());

    // Dropped unwritten: released, and a waiter is not left hanging.
    let (wire, written) = ReplyWire::new();
    drop(ControlReply::from("OK c\n").releasing_on_write(written));
    assert!(wire.wait_released(std::time::Instant::now()));
    // Queued, and its connection ended before a byte went out: the same.
    let (wire, written) = ReplyWire::new();
    drop(outbox_of(
        ControlReply::from("OK d\n").releasing_on_write(written),
    ));
    assert!(wire.wait_released(std::time::Instant::now()));
    // CONTROL: an unreleased wire's wait ends at its deadline.
    let (wire, _written) = ReplyWire::new();
    assert!(!wire.wait_released(std::time::Instant::now() + std::time::Duration::from_millis(20)));
}

// ---- `ArtifactReplyPublication` over the shipping capture reply ----
// One automatic image capture is driven through the real seams in order (the
// `CaptureCancellation` CAS and final-name write, `send_capture_reply_after_validation`,
// this module's writer and ACK wait); after each step the reply is read back onto the
// model. Where one shipping call crosses two model steps, the intermediate is the model's
// own successor.

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
    let mut outbox = outbox_of(ControlReply::with_handoff(body.clone(), handoff));
    let mut wire = Vec::new();
    assert!(deliver_waiting(&mut outbox, &mut wire).expect("the complete frame is written"));
    let nonce = outbox
        .nonce_for_test()
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
    );
    let (mut client, server) = CtlStream::pair().unwrap();
    client.write_all(ack.as_bytes()).unwrap();
    client.flush().unwrap();
    assert_eq!(
        await_ack(&mut outbox, &server),
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
    let queued_state = observe_capture_reply(
        &model,
        &path,
        &path,
        b"",
        &body,
        capture_reply_observation(3),
    );
    let mut outbox = outbox_of(ControlReply::with_handoff(body.clone(), handoff));
    let mut wire = Vec::new();
    assert!(deliver_waiting(&mut outbox, &mut wire).unwrap());
    assert!(outbox.nonce_for_test().is_some(), "guarded reply");
    let prepared = reply_successor(&model, "PrepareWire", &queued_state);
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
    assert_eq!(
        await_ack(&mut outbox, &server),
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
    let queued_state = observe_capture_reply(
        &model,
        &path,
        &path,
        b"",
        &body,
        capture_reply_observation(3),
    );
    // The peer takes the body and breaks before the challenge.
    let mut writer = FailPast {
        limit: body.len(),
        bytes: Vec::new(),
    };
    let error = match deliver_waiting(
        &mut outbox_of(ControlReply::with_handoff(body.clone(), handoff)),
        &mut writer,
    ) {
        Err(error) => error,
        Ok(_) => panic!("the trailer write fails after the complete body"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    let prepared = reply_successor(&model, "PrepareWire", &queued_state);
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
            let queued_state =
                observe_capture_reply(&model, &path, &path, b"", "", capture_reply_observation(3));
            swap();
            let mut outbox = outbox_of(ControlReply::with_handoff(body.to_string(), handoff));
            assert!(
                !outbox.holds_handoff_for_test(),
                "a failed preparation leaves nothing to acknowledge"
            );
            assert!(deliver_waiting(&mut outbox, &mut wire).expect("the failure reply is written"));
            assert!(
                String::from_utf8_lossy(&wire).starts_with("ERR artifact reply validation failed"),
                "no OK byte certifies the swapped identity"
            );
            (queued_state, "PrepareFailed")
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
    let mut outbox = control_lanes::Outbox::default();
    outbox.attach_within(reply, std::time::Duration::from_millis(100));
    let started = std::time::Instant::now();
    let error = match outbox.deliver(&server, false) {
        Err(error) => error,
        Ok(_) => panic!("a non-reading peer must exhaust the whole-write deadline"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    // A hang detector, not a budget: a write with no deadline armed blocks on this peer
    // forever.
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
    let mut outbox = control_lanes::Outbox::default();
    outbox.attach_within(reply, std::time::Duration::from_millis(250));
    assert!(deliver_waiting(&mut outbox, &mut Vec::new()).unwrap());
    let ack = format!(
        "{}{nonce}\n",
        aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX,
        nonce = outbox.nonce_for_test().expect("guarded response")
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
    assert_eq!(
        await_ack(&mut outbox, &server),
        ArtifactAckOutcome::AcknowledgementQuarantined
    );
    // The dribbled ack is the correct one, so a budget refreshed per byte would have
    // acknowledged it; the handshake proves its second byte reached a live socket, so the
    // quarantine is the deadline's verdict, not a dead pair's EOF.
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
    outbox: &mut control_lanes::Outbox,
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
    // The one-shot client exits here, its ACK unread in the server's receive queue.
    drop(client);
    assert_peer_detached(server);
    await_ack(outbox, server)
}

/// Assert the peer really detached. `drop` detaches it only when it closes the client end's
/// last descriptor, and in the shared lib-test process a child any sibling forks holds a
/// copy until it execs (macOS honors no `FD_CLOFORK`); while that copy lives the server sees
/// an attached peer and a drain test passes without entering the drain. So the tests that
/// need the detach run alone ([`crate::test_alone`]). The peer must be gone
/// ([`aterm_uds::hangup::peer_closed`], unix only), and on Darwin `SO_RCVTIMEO` must answer
/// `EINVAL`.
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

/// Sequential guarded replies, many more than the pool holds, each acknowledged by a client
/// that exits at once (every `aterm ctl image <name>`): each is admitted, its guard released
/// at the ACK, and the pool is empty at the end. An ACK already in the receive queue is
/// proof of consumption whether or not its writer is still attached. Runs alone
/// ([`assert_peer_detached`]).
#[test]
fn sequential_acknowledged_replies_never_exhaust_the_handoff_pool() {
    if !crate::test_alone::alone() {
        return;
    }
    // Charge each reply so the descriptor budget saturates at exactly the
    // live cap, the way a real capture path's pinned chain does.
    const CHARGE: usize = ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT / ARTIFACT_HANDOFF_LIMIT;
    const CAPTURES: usize = 120;
    let budgets = ArtifactBudgets::shipping();

    for capture in 0..CAPTURES {
        let permit = budgets
            .in_flight
            .try_take()
            .zip(budgets.descriptors.try_take(CHARGE))
            .map(|(in_flight, chain)| ReplyRetentionPermit {
                in_flight,
                descriptors: Some(chain),
            })
            .unwrap_or_else(|| {
                panic!(
                    "capture {capture} was refused admission: a released reply \
                     is still holding its handoff permit"
                )
            });
        let alive = Arc::new(AtomicBool::new(true));
        let (client, server) = CtlStream::pair().unwrap();
        let reply = guarded_probe_reply(permit.retain(WireProbe {
            alive: Arc::clone(&alive),
            prepared: Arc::new(AtomicBool::new(false)),
            fail_prepare: false,
        }));
        let mut outbox = outbox_of(reply);
        assert!(outbox.deliver(&server, false).unwrap(), "guarded response");
        assert_eq!(
            acknowledge_then_exit(client, &server, &mut outbox),
            ArtifactAckOutcome::PeerAcknowledged,
            "capture {capture} lost its acknowledgement"
        );
        assert!(
            !alive.load(Ordering::Acquire),
            "capture {capture} kept its guard past the acknowledgement"
        );
    }

    assert_eq!(
        (budgets.descriptors.usage(), budgets.in_flight.used()),
        (UnitUsage::default(), 0),
        "every one of {CAPTURES} sequential captures gave its permit back"
    );
}

/// Draining a detached peer recovers its bytes, not its verdict: a one-shot client that
/// exits without a valid ACK (silent, wrong line, unterminated) is still a failed handoff.
/// Runs alone ([`assert_peer_detached`]).
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
        let mut outbox = outbox_of(reply);
        assert!(outbox.deliver(&server, false).unwrap(), "guarded response");
        if let Some(farewell) = farewell {
            let mut writing = &client;
            writing.write_all(farewell.as_bytes()).unwrap();
            writing.flush().unwrap();
        }
        drop(client);
        assert_peer_detached(&server);
        assert_eq!(
            await_ack(&mut outbox, &server),
            ArtifactAckOutcome::AcknowledgementQuarantined,
            "{label} must not read as a consume acknowledgement"
        );
        // Held for a day, so the reaper cannot beat the assertion below to the
        // guard however long this thread stalls between the two: this test is
        // about the verdict, not about how quickly the grace expires. The
        // process is this test's alone, and ends before the grace does.
        last_quarantine_ticket();
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
            let mut reader = BufReader::new(&server);
            let outcome = outbox_of(reply)
                .finish_waiting(&server, &mut reader)
                .unwrap()
                .expect("guarded response");
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

/// A verb removed with the embedded operator or the network drive answers
/// an old client with what replaced it, never the bare unknown-verb line.
#[test]
fn removed_verbs_are_unadvertised_stable_tombstones() {
    for operator in ["operator", "operator-propose-bin"] {
        let said =
            retired_verb_error(operator).unwrap_or_else(|| panic!("{operator} has a tombstone"));
        assert!(
            said.starts_with(&format!("ERR verb {operator} was removed")),
            "{said}"
        );
        assert!(said.contains("spawn agent=claude"), "{operator}: {said}");
    }
    for dial in ["dial", "dial-list", "dial-token"] {
        let said = retired_verb_error(dial).unwrap_or_else(|| panic!("{dial} has a tombstone"));
        assert!(
            said.starts_with(&format!("ERR verb {dial} was removed")),
            "{said}"
        );
        assert!(said.contains("aterm fleet init"), "{dial}: {said}");
    }
    // NEGATIVE CONTROL: a verb nothing removed has no tombstone.
    assert_eq!(retired_verb_error("operators"), None);
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
    TextArgs, cmd_cell, cmd_colors, cmd_cursor_json, cmd_cwd, cmd_line, cmd_mainscreen, cmd_modes,
    cmd_screen_styled_json, cmd_search, cmd_text, cmd_text_json_opt, serialize_dims,
    serialize_dims_json, styled_image_json,
};
use super::control_selection::{cmd_blocks, cmd_blocks_json, cmd_wait};
use super::control_session::{
    TurnIo, cmd_await, cmd_cast, cmd_connect_in, cmd_disconnect_in, cmd_edges, cmd_edges_json,
    cmd_family, cmd_flows, cmd_grant, cmd_lease, cmd_meta, cmd_ready, cmd_revoke,
    cmd_sessions_store, cmd_timeline, cmd_turn, cmd_who, cmd_whoami, raise_target,
};
use super::*;
use crate::TabAction;
use crate::budget::UnitUsage;
use crate::input::InputEvent;
use crate::session_waits::RewrapGauge;
use aterm_control::wire::pct_decode;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_session::sink::SinkWriter;
use aterm_types::control_verbs::input_of;
use std::io::BufReader;
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

/// `task mark` is Owner-only at the dispatch gate ([`need_of`]): an edge
/// granted ANY op on the session is refused it — a manager acts on the record,
/// so no edge may forge one — while a read edge reads `task` and parks `await
/// task`, and an ungranted edge does neither.
#[test]
fn task_mark_is_owner_only_and_task_is_a_read() {
    let ctx = test_ctx();
    assert!(line_authorized(Scope::Owner, "task", "mark done", &ctx));
    for op in [Op::ReadScreen, Op::WriteInput, Op::Signal, Op::ConfigWrite] {
        let granted = edge_granted(op, &ctx);
        assert!(
            !line_authorized(granted, "task", "mark done why=marker", &ctx),
            "{op:?}"
        );
    }
    let read = edge_granted(Op::ReadScreen, &ctx);
    assert!(line_authorized(read, "task", "", &ctx));
    assert!(line_authorized(read, "await", "task since=1", &ctx));
    assert!(!line_authorized(edge(), "task", "", &ctx));
    assert!(!line_authorized(edge(), "await", "task", &ctx));
}

/// The list form of `await task` is the dispatcher's (no session of the
/// connection's own needed), and only a bare `await task` with an `@` word in it.
#[test]
fn the_await_task_list_form_is_told_from_every_other_await() {
    assert_eq!(
        await_task_list("task @s-a,@s-b all since=3"),
        Some(vec!["@s-a,@s-b", "all", "since=3"])
    );
    for other in [
        "task since=3",
        "task",
        "agent idle",
        "inbox since=1",
        "taskforce @s-a",
    ] {
        assert_eq!(await_task_list(other), None, "{other:?}");
    }
}

/// The list form resolves every target in THIS instance: one it does not host
/// fails the whole request (the client routes siblings), a scoped caller needs
/// a read edge on each, and a news-bearing worker answers through the same
/// rows `await_task` writes.
#[test]
fn dispatch_await_task_resolves_its_list_and_refuses_what_it_cannot() {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let store = session_store::new_store();
    let active: ActiveHandle = Arc::default();
    let a = registered_session(1, -1, b"");
    let b = registered_session(2, -1, b"");
    store.write().unwrap().register(a.clone());
    store.write().unwrap().register(b.clone());
    mark_session_task(&a.ctx, 1, &TaskMark::new(TaskState::Done)).expect("unfenced");
    let list = format!("@{},@2 since=0 timeout=60000", a.sid.as_str());
    let words: Vec<&str> = list.split_whitespace().collect();
    let reply = dispatch_await_task(&words, &active, &store, Scope::Owner);
    assert!(reply.starts_with("OK 2\n"), "{reply}");
    assert!(
        reply.contains(&format!("@{} rev=1 state=done ", a.sid.as_str()))
            && reply.contains(&format!("@{} rev=0 state=- ", b.sid.as_str())),
        "{reply}"
    );
    // One session named twice is two targets: a row each, in request order,
    // so a client reads row `i` as target `i`'s.
    let twice = format!("@1,@{} since=0 timeout=60000", a.sid.as_str());
    let words: Vec<&str> = twice.split_whitespace().collect();
    let reply = dispatch_await_task(&words, &active, &store, Scope::Owner);
    let rows: Vec<&str> = reply.lines().collect();
    assert_eq!(rows.len(), 3, "{reply}");
    assert_eq!(rows[0], "OK 2", "{reply}");
    assert!(
        rows[1].starts_with(&format!("@{} rev=1 state=done ", a.sid.as_str()))
            && rows[1] == rows[2],
        "{reply}"
    );
    let missing = dispatch_await_task(
        &["@2,@s-0000000000000000dead"],
        &active,
        &store,
        Scope::Owner,
    );
    assert_eq!(missing, "ERR no such session @s-0000000000000000dead\n");
    assert_eq!(
        dispatch_await_task(&["@."], &active, &store, Scope::Owner),
        NO_ACTIVE_TERMINAL
    );
    assert_eq!(
        dispatch_await_task(&["@2"], &active, &store, edge()),
        "ERR denied\n"
    );
    assert_eq!(
        dispatch_await_task(&["@2", "@1"], &active, &store, Scope::Owner),
        crate::task_record::AWAIT_TASK_USAGE
    );
}

/// Which requests move to a wait lane (`control_lanes.rs`): those that park on an
/// event or hand the connection to a relay. Every read, keystroke and `subscribe`
/// that flips to push here is request-lane work; a `subscribe` to one session
/// another instance hosts is relayed for the whole subscription, a wait, in every
/// spacing the request grammar reads. A spelling that is no session id (`@S-…`,
/// upper-case hex, a path) is answered here and relayed nowhere. Only an
/// Owner-class connection is relayed.
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
        &format!("@{remote_sid} text"),
        &format!("@{remote_sid}\ttext"),
        &format!("  @{remote_sid}  text"),
        "@s-0a1b2c3d4e5f60718293 text",
        // An owner verb that acts on a session is relayed like any other.
        &format!("@{remote_sid} topic triage"),
        // The selector-second grammar: relayed to the instance that hosts it,
        // classed a wait wherever it is relayed.
        &format!("subscribe @{remote_sid} events"),
        &format!("subscribe  @{remote_sid} events"),
        &format!("subscribe\t@{remote_sid}\tevents"),
        // Waits for the recording, then folds it whole (`cast drift`).
        "cast drift",
        &format!("@{local_sid} cast drift max_runs=4"),
        // Waits, bounded, for the console host's forwarded leave.
        "mainscreen",
        &format!("@{local_sid} mainscreen"),
        // A paste into another session waits, bounded, for its program to read
        // it (`background_input`): never a request lane's wait. So does a
        // `paste-bin` whatever it names: flagless, it takes the same wait when
        // the caller's own tab is not on screen.
        &format!("@{local_sid} paste hello"),
        "@3 paste hello",
        &format!("@{local_sid} paste-bin 5"),
        "paste-bin 5",
        "@. paste-bin 5",
        // A recording runs for its seconds (up to 60) and an update check waits
        // on the network for tens of seconds.
        "video 3",
        "video 3 full",
        "update check",
        // Parks for the owner to answer the sudo password sheet.
        "askpass",
        // Lays out a new Editor view on the main thread, then parks until that
        // view closes.
        &format!("@{local_sid} edit file:///tmp/x.txt"),
        "await edit ticket=1",
    ] {
        assert!(
            request_waits(line, Scope::Owner, &store),
            "{line:?} parks or relays"
        );
    }
    // An Edge is never relayed, so a line naming another instance's session is
    // refused at once on a request lane; what parks still parks.
    for line in [
        &format!("@{remote_sid} text"),
        &format!("subscribe @{remote_sid} events"),
    ] {
        assert!(
            !request_waits(line, edge, &store),
            "{line:?} from an Edge is refused, never relayed"
        );
    }
    assert!(request_waits("await seq 5 timeout 20000", edge, &store));
    assert!(request_waits(
        &format!("@{remote_sid} await seq 5"),
        edge,
        &store
    ));
    for line in [
        // An instance verb is answered here whatever it names, as the instance
        // hosting it would answer.
        &format!("@{remote_sid} who"),
        &format!("@{remote_sid} hold on"),
        // No session id, so no relay: answered here at once.
        "@s-not-hosted-here text",
        &format!("@S-{} text", &remote_sid[2..]),
        &format!("@./{remote_sid} text"),
        &format!("subscribe @./{remote_sid} events"),
        &format!("subscribe @S-{} events", &remote_sid[2..]),
        // Upper-case hex is no minted id: the lower-case spelling of this one
        // is relayed (above).
        "@s-0A1B2C3D4E5F60718293 text",
        "subscribe @s-0A1B2C3D4E5F60718293 events",
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
        // A recording's status, its stop and its frame listing answer at once,
        // as do the updater's status forms.
        "video status",
        "video stop",
        "video frames count=4",
        "update",
        "update status",
    ] {
        assert!(
            !request_waits(line, Scope::Owner, &store),
            "{line:?} is request-lane work"
        );
    }
}

/// The subscription cap is a sixteenth of the soft descriptor limit, within
/// `[CONTROL_SUBSCRIPTIONS_MIN, CONTROL_SUBSCRIPTIONS_MAX]`, beside the
/// connections' quarter: at launchd's 256, both together leave more than
/// two-thirds of the descriptors to the rest of the window.
#[test]
fn the_subscription_cap_is_a_share_of_the_descriptor_limit() {
    assert_eq!(subscriptions_cap_for(Some(256)), 16, "launchd's default");
    assert!(connections_cap_for(Some(256)) + subscriptions_cap_for(Some(256)) < 256 / 3);
    assert_eq!(subscriptions_cap_for(Some(2_048)), 128);
    assert_eq!(
        subscriptions_cap_for(Some(10_240)),
        CONTROL_SUBSCRIPTIONS_MAX
    );
    assert_eq!(
        subscriptions_cap_for(Some(u64::MAX)),
        CONTROL_SUBSCRIPTIONS_MAX,
        "RLIM_INFINITY"
    );
    assert_eq!(
        subscriptions_cap_for(Some(20)),
        CONTROL_SUBSCRIPTIONS_MIN,
        "never below the fixed pool it replaced"
    );
    assert_eq!(
        subscriptions_cap_for(None),
        CONTROL_SUBSCRIPTIONS_MIN,
        "unreadable"
    );
    #[cfg(unix)]
    assert_eq!(
        control_subscriptions_cap(),
        subscriptions_cap_for(soft_descriptor_limit())
    );
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
    let mut unread = control_lanes::Unread::default();
    let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
    assert_eq!(
        read_authenticated_request_line(&mut reader, &mut std::io::sink()).as_deref(),
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
    let mut unread = control_lanes::Unread::default();
    let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
    assert_eq!(
        read_authenticated_request_line(&mut reader, &mut std::io::sink()).as_deref(),
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
        let mut unread = control_lanes::Unread::default();
        let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
        let first = read_authenticated_request_line(&mut reader, &mut std::io::sink());
        let prefetched = !reader.buffer().is_empty();
        let second = read_authenticated_request_line(&mut reader, &mut std::io::sink());
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
/// (`control_lanes::Unread`). They were read from the socket already,
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
        let mut unread = control_lanes::Unread::holding(b"version\nstatus\n".to_vec());
        let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
        let first = read_authenticated_request_line(&mut reader, &mut std::io::sink());
        let second = read_authenticated_request_line(&mut reader, &mut std::io::sink());
        let _ = tx.send((first, second));
        read_authenticated_request_line(&mut reader, &mut std::io::sink())
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
        let mut unread = control_lanes::Unread::default();
        let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
        let tail = read_authenticated_request_line(&mut reader, &mut std::io::sink());
        let eof = read_authenticated_request_line(&mut reader, &mut std::io::sink());
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

/// A relay forwards what the lane reader already holds past its request line
/// before its own reads (`LaneReader::take_buffered`): exactly those bytes,
/// once, and never a read of the socket for more. A one-line forward's client
/// sends nothing until it is answered, so such a read would wait forever before
/// the relay starts (the `drain_buffered` `fill_buf` hang). The bytes the client
/// has sent before the first take are the witness: still in the socket, they
/// are not the reader's to hand over.
#[test]
fn a_lane_reader_hands_over_what_it_holds_without_reading_for_more() {
    use std::io::{BufRead, Write};

    let (mut client, server) = CtlStream::pair().expect("real control socket pair");
    client
        .write_all(b"verb line\nLEFTOVER")
        .expect("a request line and what was pipelined behind it");
    let mut unread = control_lanes::Unread::default();
    let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
    assert!(
        reader.take_buffered().is_empty(),
        "nothing held: what is still in the socket is not read for it"
    );
    let mut line = String::new();
    reader.read_line(&mut line).expect("the request line");
    assert_eq!(line, "verb line\n");
    assert_eq!(reader.take_buffered(), b"LEFTOVER");
    assert!(reader.take_buffered().is_empty(), "handed over once");
}

#[test]
#[cfg(unix)]
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
            // SGR 5 blink, which `cell` reports and the resolved colours do not
            // carry; then SGR 25 ends it, so the next cell has none.
            "blink",
            2,
            10,
            b"\x1b[5;4mB\x1b[25mN",
            &[
                "{\"glyph\":\"B\",\"fg\":",
                "\"attrs\":[\"underline\",\"blink\"]",
                "\"attrs\":[\"underline\"]",
            ],
        ),
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
    let out = cmd_screen_styled_json(&term, &crate::frame_memo::FrameSlot::default(), "");
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
    let out = cmd_screen_styled_json(&term, &crate::frame_memo::FrameSlot::default(), "");
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
    let out = cmd_screen_styled_json(&term, &crate::frame_memo::FrameSlot::default(), "");
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

/// AN OVER-CAP `screen` GATHERS AGAIN ONLY TO LEAVE OUT A PUBLISHED FRAME'S
/// PICTURES. A grid past the reply-line cap with no image is answered the `ERR`
/// line from the one gather the read made, or from none when a `cells` watch
/// published the frame; a frame the read gathered with pictures is rebuilt from
/// its own snapshot without them.
///
/// Negative control: a published frame that carries pictures is gathered afresh,
/// so the counter reads a second gather when one is made, and the published
/// frame keeps its payloads for its watchers.
#[test]
fn an_over_cap_screen_gathers_again_only_for_a_published_frames_pictures() {
    use crate::frame_memo::{FrameSlot, Styled};
    use crate::work_counts::take_styled_frames_gathered as gathered;
    use aterm_core::terminal::Terminal;
    let too_big = "ERR screen: frame too large";
    let truncated = |out: &str| {
        out.starts_with("OK 1\n") && out.matches("\"format\":\"truncated\"").count() == 2
    };

    let term = Arc::new(Mutex::new(Terminal::new(120, 300)));
    let slot = Arc::new(FrameSlot::default());
    gathered();
    assert!(cmd_screen_styled_json(&term, &slot, "").starts_with(too_big));
    assert_eq!(gathered(), 1, "one gather for the ERR line");
    let hold = FrameSlot::hold_cells(&slot);
    assert!(cmd_screen_styled_json(&term, &slot, "").starts_with(too_big));
    assert_eq!(gathered(), 1, "the read that published gathered once");
    assert!(cmd_screen_styled_json(&term, &slot, "").starts_with(too_big));
    assert_eq!(
        gathered(),
        0,
        "a published frame with no picture is not gathered"
    );
    drop(hold);

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
    let slot = Arc::new(FrameSlot::default());
    gathered();
    assert!(truncated(&cmd_screen_styled_json(&term, &slot, "")));
    assert_eq!(gathered(), 1, "its own snapshot, rebuilt without them");
    let _hold = FrameSlot::hold_cells(&slot);
    assert!(truncated(&cmd_screen_styled_json(&term, &slot, "")));
    assert_eq!(gathered(), 1, "the read that published gathered once");
    assert!(truncated(&cmd_screen_styled_json(&term, &slot, "")));
    assert_eq!(
        gathered(),
        1,
        "the negative control: a published frame's pictures are left out of a fresh gather"
    );
    let look = slot.look_frame();
    let t = term.lock().unwrap();
    let key = crate::frame_key::FrameKey::read(&t);
    match look.styled(&t, &key) {
        Styled::Hit(frame) => assert!(
            frame.carries_image_payloads(),
            "the published frame keeps its payloads"
        ),
        Styled::Miss(_) => panic!("the frame stays published"),
    }
}

/// A small inline image encodes normally; an oversized one (user-supplied OSC 1337)
/// is `truncated` with an empty payload but its real `nbytes`, so consumers learn
/// it exists without a multi-MB payload on every `image read` / styled frame.
#[test]
fn oversized_image_payload_is_truncated_not_encoded() {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageScaling};
    let small = Arc::new(ImageData {
        bytes: vec![1, 2, 3, 4],
        format: ImageFormat::Png,
        cols: 1,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    });
    let payload = image_payload(&small);
    assert_eq!(payload.format(), "png");
    assert!(
        !payload.b64().is_empty(),
        "small image must carry its payload"
    );

    let big = Arc::new(ImageData {
        bytes: vec![0u8; MAX_IMAGE_PAYLOAD_BYTES + 1],
        format: ImageFormat::Png,
        cols: 80,
        rows: 24,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    });
    let payload = image_payload(&big);
    assert_eq!(
        payload.format(),
        "truncated",
        "oversized image must be marked truncated"
    );
    assert!(
        payload.b64().is_empty(),
        "oversized image must NOT be base64-encoded"
    );
    // The line form still reports the real size (so the consumer can decide).
    let mut line = String::new();
    image_read_line(&mut line, 0, 0, 0, 0, &big, &payload);
    assert!(
        line.contains(&format!("truncated {}", MAX_IMAGE_PAYLOAD_BYTES + 1)),
        "{line}"
    );
    // And the JSON form is well-formed with an empty payload + real nbytes.
    let mut js = String::new();
    styled_image_json(&mut js, 0, 0, &big, &payload);
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

/// A NESTED CHILD IS REACHED WITH ITS OWN TOKEN. A session an inner aterm hosts
/// (one this process provisioned, holding the edges it minted for it in the
/// child's `ATERM_EDGE_TOKENS` file) is forwarded like any other instance's: to
/// the socket its graph entry names, confined to the runtime dir, presenting the
/// token beside that socket and the caller's line as it came, selector kept (a
/// `subscribe`'s streams and flags too). The edges minted for it are never
/// presented, so a relaunch under a fresh nonce is reached the same way.
/// Fail-closed: an Edge, a list of targets, an id with no entry and an entry
/// redirected out of the runtime dir plan nothing.
#[test]
fn a_nested_child_is_forwarded_with_its_own_token() {
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    let dir = dir.path();
    let store = crate::session_store::new_store();
    // The child as this process provisions it: its identity and its edges.
    let (env, _provisioned) = crate::spawn::provision_child_proxy(Some(dir));
    let get = |key: &str| {
        env.iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| panic!("the child's {key}"))
    };
    let child = SessionId::new(get(aterm_types::domain::ENV_SESSION_ID));
    let (read, write, signal) =
        crate::proxy::read_edge_tokens(&get(aterm_types::domain::ENV_EDGE_TOKENS))
            .expect("the child's edges");
    let edges = [read, write, signal];
    let nonce = LaunchNonce::generate();
    let sock = hosting_instance(dir, 77030, &child, "c0ffee01");
    // The forward dials the confined path: the canonical runtime dir.
    let confined = std::fs::canonicalize(dir)
        .unwrap()
        .join("aterm-77030.sock")
        .to_string_lossy()
        .into_owned();

    let line = format!("@{} screen", child.as_str());
    for relayed in [
        line.clone(),
        format!("@{} key up", child.as_str()),
        format!("subscribe @{} cells,bytes every-frame", child.as_str()),
        format!("subscribe  @{} cells", child.as_str()),
    ] {
        let plan = plan_for(&relayed, Scope::Owner, &store, dir)
            .unwrap_or_else(|| panic!("{relayed:?}: the Owner reaches the child"));
        assert_eq!(plan, confined);
        let first = presented(&plan, &relayed);
        assert_eq!(first, format!("TOKEN c0ffee01 {relayed}\n"));
        assert!(
            edges.iter().all(|edge| !first.contains(edge.as_str())),
            "{relayed:?}: an edge minted for the child was presented"
        );
    }

    // A subscribe with a comma-list of targets stays local (not one session).
    let mixed = format!("subscribe @.,@{} cells", child.as_str());
    assert!(plan_for(&mixed, Scope::Owner, &store, dir).is_none());
    // An id no instance published → no plan.
    let other = format!("@{} screen", SessionId::generate().as_str());
    assert!(plan_for(&other, Scope::Owner, &store, dir).is_none());

    // A relaunch (graph entry under a NEW nonce) is the same host to reach.
    crate::proxy::write_graph_entry(
        dir,
        &child,
        &sock.to_string_lossy(),
        &LaunchNonce::generate(),
    );
    assert_eq!(
        plan_for(&line, Scope::Owner, &store, dir).as_deref(),
        Some(confined.as_str()),
        "a relaunched child is reached with its own token"
    );

    // A graph entry redirected outside our runtime dir (a same-uid overwrite)
    // fails closed: nothing dials it, so no token is presented there.
    crate::proxy::write_graph_entry(dir, &child, "/tmp/evil-attacker.sock", &nonce);
    assert!(
        plan_for(&line, Scope::Owner, &store, dir).is_none(),
        "out-of-dir socket path must fail closed (token must not leak)",
    );
}

/// OWNER VERBS REACH A NESTED CHILD. The child hop presented the edge for a
/// verb's base op, so `topic` and `rekey` (owner verbs on a session) never
/// forwarded, and `settings`, `copy`, `meta set`, `inbox seen`, `spawn` and
/// `window` reached the child under an edge its own gates refuse. A child is
/// reached with its own token now: each line is planned to the child's socket
/// verbatim, and what the child's connect-time auth (`first_line_scope`) makes
/// of the presented line is an Owner connection, which those gates admit.
/// Negative control: an Edge plans none of them.
#[test]
fn owner_topic_and_settings_reach_a_nested_child() {
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    let dir = dir.path();
    let store = crate::session_store::new_store();
    let child = SessionId::generate();
    let child_token = "0f".repeat(32);
    hosting_instance(dir, 77031, &child, &child_token);
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    for verb in [
        "topic triage",
        "rekey",
        "story",
        "settings get font.size",
        "copy",
        "meta set title x",
        "inbox seen 3 handled",
        "spawn",
        "window",
    ] {
        let line = format!("@{} {verb}", child.as_str());
        let sock = plan_for(&line, Scope::Owner, &store, dir)
            .unwrap_or_else(|| panic!("{line:?}: the Owner reaches the child"));
        let first = presented(&sock, &line);
        let (scope, _, inline) =
            first_line_scope(first.trim_end_matches('\n'), &child_token, || None).unwrap_or_else(
                |why| panic!("{first:?} does not authenticate at the child: {why}"),
            );
        assert_eq!(
            scope,
            Scope::Owner,
            "{line:?}: the child serves it as its Owner"
        );
        assert_eq!(inline.as_deref(), Some(line.as_str()));
        assert!(
            plan_for(&line, edge, &store, dir).is_none(),
            "{line:?}: an Edge never forwards"
        );
    }
}

/// A socket bound at `path` and closed without ever listening: a connect to it
/// answers `ECONNREFUSED`, like a crashed instance's leftover socket file. A
/// listening descriptor in a shared test process could be inherited by another
/// test's child between listen and close, and the kernel would keep accepting
/// through that duplicate until its execve; one that never listened cannot.
#[cfg(unix)]
fn never_listening_socket(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    // SAFETY: three straight-line libc calls; the path is checked to fit
    // sun_path, and every return is checked.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        assert!(fd >= 0, "socket(AF_UNIX)");
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = path.as_os_str().as_encoded_bytes();
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
    // PRECONDITION, stated rather than assumed: the kernel must answer this
    // dial with ECONNREFUSED. Any other connect error (EMFILE under a
    // saturated test process, EINTR, EAGAIN) is the environment's, not the
    // code's: wait out that transient.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "PRECONDITION: the dead socket answers ECONNREFUSED (transient connect errors \
         persisting are the environment, not the proxy)",
        || match CtlStream::connect(path) {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => Some(()),
            Err(_) => None,
            Ok(_) => panic!("nobody should be listening on the dead socket"),
        },
    );
}

/// A STALE ENTRY IS NO HOP, AND THE FORWARD'S CONNECT IS ITS ONLY LOOK. An entry
/// whose socket nothing listens at (a crashed instance's leftover, or no socket
/// file at all) or that has no token beside its socket is not forwarded:
/// `try_proxy_forward` returns `false` with the connection untouched — its read
/// timeout as the lane set it, the request the client pipelined behind still in
/// the reader, nothing written — so the local dispatch answers `ERR no such
/// session` and then serves the next line. FAILS WITH THE CONNECT MADE AFTER the
/// timeout is cleared and the reader drained: the pipelined request is lost.
/// Negative control: the same request to a live host is forwarded, on its one
/// connection and with no silent one before it (the liveness probe this
/// replaces connected and hung up first, which the host counts).
#[test]
#[cfg(unix)]
fn a_stale_graph_entry_answers_no_such_session_without_a_probe() {
    use std::io::{BufRead, BufReader, Write};
    /// The read timeout a lane serves a connection under.
    const LANE_POLL: std::time::Duration = std::time::Duration::from_millis(250);
    let scratch = crate::test_scratch::short_dir("the control dir");
    let dir = scratch.path();
    let store = crate::session_store::new_store();
    let nonce = LaunchNonce::generate();
    // Nothing listening: a socket bound and closed without listening.
    let dead = SessionId::generate();
    let dead_sock = dir.join("aterm-77040.sock");
    never_listening_socket(&dead_sock);
    std::fs::write(dir.join("aterm-77040.token"), "beef\n").expect("token");
    crate::proxy::write_graph_entry(dir, &dead, &dead_sock.to_string_lossy(), &nonce);
    // A live listener with no token beside it.
    let tokenless = SessionId::generate();
    let tokenless_sock = dir.join("aterm-77041.sock");
    let _tokenless = aterm_uds::CtlListener::bind(&tokenless_sock).expect("bind");
    crate::proxy::write_graph_entry(dir, &tokenless, &tokenless_sock.to_string_lossy(), &nonce);
    // No socket file at all.
    let unbound = SessionId::generate();
    let unbound_sock = dir.join("aterm-77042.sock");
    std::fs::write(dir.join("aterm-77042.token"), "beef\n").expect("token");
    crate::proxy::write_graph_entry(dir, &unbound, &unbound_sock.to_string_lossy(), &nonce);

    for (what, sid) in [
        ("nothing listening", &dead),
        ("no token", &tokenless),
        ("no socket", &unbound),
    ] {
        let line = format!("@{} text", sid.as_str());
        let (client, server) = CtlStream::pair().expect("pair");
        server
            .set_read_timeout(Some(LANE_POLL))
            .expect("the lane's poll");
        (&client)
            .write_all(format!("{line}\nversion\n").as_bytes())
            .expect("the request, and the next one behind it");
        let mut unread = control_lanes::Unread::default();
        let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
        let mut got = String::new();
        reader.read_line(&mut got).expect("the request line");
        assert_eq!(
            reader.buffer(),
            b"version\n",
            "PRECONDITION: the reader holds the pipelined request"
        );
        let request = Request::parse(got.trim_end_matches('\n').to_string());
        assert!(
            proxy_forward_plan(&request, Scope::Owner, &store, dir).is_some(),
            "{what}: the plan names the entry's socket (the connect decides)"
        );
        let forwarded =
            try_proxy_forward(&request, Scope::Owner, &store, dir, &server, &mut reader);
        assert!(!forwarded, "{what}: a stale entry is answered here");
        assert_eq!(
            reader.buffer(),
            b"version\n",
            "{what}: the pipelined request is still the lane's"
        );
        assert_eq!(
            server.read_timeout().expect("read timeout"),
            Some(LANE_POLL),
            "{what}: the read timeout is as the lane set it"
        );
        client.set_nonblocking(true).expect("non-blocking");
        assert_eq!(
            (&client).read(&mut [0u8; 64]).map_err(|e| e.kind()),
            Err(std::io::ErrorKind::WouldBlock),
            "{what}: nothing was written to the client"
        );
    }

    // Negative control: a live host is forwarded to, on one connection.
    let live = SessionId::generate();
    let live_sock = hosting_instance(dir, 77043, &live, "cafe0123");
    let listener = aterm_uds::CtlListener::bind(&live_sock).expect("bind the host");
    let line = format!("@{} text", live.as_str());
    let host = std::thread::spawn(move || {
        // Every connection until `stop`: the verbs forwarded to it, those that
        // said nothing, and those that asked `version` alone (the relay's look
        // at a host that ended the exchange first).
        let (mut verbs, mut silent, mut looks) = (Vec::new(), 0, 0);
        loop {
            let (conn, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(&conn);
            let mut first = String::new();
            if reader.read_line(&mut first).unwrap_or(0) == 0 {
                silent += 1;
                continue;
            }
            if first == "stop\n" {
                return (verbs, silent, looks);
            }
            let _ = (&conn).write_all(b"OK version=host\n");
            let mut verb = String::new();
            if reader.read_line(&mut verb).unwrap_or(0) == 0 {
                looks += 1;
                continue;
            }
            let _ = (&conn).write_all(b"OK served\n");
            verbs.push(verb);
        }
    });
    let (client, server) = CtlStream::pair().expect("pair");
    client
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("a hang detector on the reply");
    (&client)
        .write_all(format!("{line}\n").as_bytes())
        .expect("the request");
    let server_dir = dir.to_path_buf();
    let server_store = store.clone();
    let forward = std::thread::spawn(move || {
        let mut unread = control_lanes::Unread::default();
        let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
        let mut got = String::new();
        reader.read_line(&mut got).expect("the request line");
        let request = Request::parse(got.trim_end_matches('\n').to_string());
        try_proxy_forward(
            &request,
            Scope::Owner,
            &server_store,
            &server_dir,
            &server,
            &mut reader,
        )
    });
    let mut reply = String::new();
    let _ = BufReader::new(&client).read_line(&mut reply);
    assert_eq!(
        reply, "OK served\n",
        "the live host answered through the relay"
    );
    hang_up(client);
    assert!(
        join_after_hang_up(forward, "the forward to end on its client's hang-up"),
        "the forward owned the line"
    );
    (&CtlStream::connect(&live_sock).expect("connect to stop"))
        .write_all(b"stop\n")
        .expect("stop");
    let (verbs, silent, _looks) = host.join().expect("the stand-in host");
    assert_eq!(
        verbs,
        [format!("{line}\n")],
        "one forward, the line as it came"
    );
    assert_eq!(silent, 0, "a connection said nothing: a liveness probe");
}

/// Sibling hop: an `@<sid>` not hosted locally but published in the graph by
/// another live same-uid instance forwards to that instance's socket, presenting
/// the sibling's own per-launch token and keeping the selector so the sibling
/// resolves it among its tabs. Fail-closed guards: Edge scope, locally hosted
/// sid, an out-of-dir socket, instance (`Target::Meta`) verbs. Whether anything
/// listens there and has a token is the forward's connect's to find
/// (`a_stale_graph_entry_answers_no_such_session_without_a_probe`). (The
/// self-dial guard: `sibling_forward_plan_conforms_to_proxy_forward_one_hop`.)
#[test]
#[cfg(unix)]
fn sibling_forward_plan_presents_instance_token_and_keeps_selector() {
    // A discovery state of its own: no other test's self socket is read here.
    let _guard = crate::proxy::self_sock_test_guard();
    let dir_guard = crate::test_scratch::short_dir("the sibling's socket");
    let dir = dir_guard.path().to_path_buf();
    let store = crate::session_store::new_store();
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();

    // The "sibling": a live listener at <dir>/aterm-77001.sock + its 0600 token.
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

    // Owner + live sibling + op verb → forward, selector kept, sibling token.
    let line = format!("@{} text", sid.as_str());
    let plan = plan_for(&line, Scope::Owner, &store, &dir).expect("sibling forwards");
    assert_eq!(plan, confined);
    assert_eq!(
        presented(&plan, &line),
        format!("TOKEN cafe0123 @{} text\n", sid.as_str())
    );

    // Selector-SECOND subscribe grammar: forwarded verbatim (selector kept).
    let sline = format!("subscribe @{} cells,bytes", sid.as_str());
    let splan = plan_for(&sline, Scope::Owner, &store, &dir).expect("subscribe forwards");
    assert_eq!(splan, confined);
    assert_eq!(
        presented(&splan, &sline),
        format!("TOKEN cafe0123 subscribe @{} cells,bytes\n", sid.as_str())
    );
    // A path spelling is no session id, though `graph/./<sid>` names the same
    // entry: answered here (no such session), relayed nowhere.
    for pline in [
        format!("subscribe @./{} cells", sid.as_str()),
        format!("@./{} text", sid.as_str()),
    ] {
        assert!(
            plan_for(&pline, Scope::Owner, &store, &dir).is_none(),
            "{pline:?}: only a session id is joined into a graph path"
        );
        assert!(!request_waits(&pline, Scope::Owner, &store));
    }
    // `@*` is this instance's live target set (the bridge's and fleet's
    // subscription), never one session: an entry named `*` (planted by hand,
    // since no id that is not a session id is ever written) relays it nowhere.
    std::fs::write(
        dir.join("graph").join("*"),
        format!("sock {sib_sock_str}\nnonce {}\n", nonce.to_hex()),
    )
    .expect("plant `graph/*`");
    let all = "subscribe @* events,sessions";
    assert!(
        plan_for(all, Scope::Owner, &store, &dir).is_none(),
        "`subscribe @*` stays local"
    );

    // Edge scope must NOT reach a sibling.
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    for relayed in [&line, &sline] {
        assert!(
            plan_for(relayed, edge, &store, &dir).is_none(),
            "edge scope must not forward to a sibling",
        );
    }

    // An instance verb never crosses processes: it is answered here, as the
    // sibling would answer it (`whoami` an owner verb, `who` a read).
    for meta in ["whoami", "who"] {
        let gline = format!("@{} {meta}", sid.as_str());
        assert!(plan_for(&gline, Scope::Owner, &store, &dir).is_none());
        assert!(!request_waits(&gline, Scope::Owner, &store), "{gline:?}");
    }

    // A locally hosted sid is not a hop (in-process resolution owns it).
    let local = registered_session(91, -1, b"");
    let local_line = format!("@{} text", local.sid.as_str());
    crate::proxy::write_graph_entry(&dir, &local.sid, &sib_sock_str, &local.nonce);
    store.write().unwrap().register(local.clone());
    assert!(
        plan_for(&local_line, Scope::Owner, &store, &dir).is_none(),
        "locally-hosted sid must resolve in-process, never hop",
    );

    // CONFINEMENT: an entry redirected OUTSIDE our runtime dir must never be
    // dialed.
    let outside = SessionId::generate();
    crate::proxy::write_graph_entry(&dir, &outside, "/tmp/evil-sibling.sock", &nonce);
    let out_line = format!("@{} text", outside.as_str());
    assert!(
        plan_for(&out_line, Scope::Owner, &store, &dir).is_none(),
        "an out-of-dir sibling socket must fail closed",
    );
}

/// A selector body that is no session id is never joined into a graph path: the
/// entry `graph/../x` would name is planted, live and token-bearing, and `@../x`
/// still resolves to no session, plans no forward and is no wait. So is an id
/// spelled in upper-case hex, whose `graph/` entry a case-insensitive volume
/// folds onto the planted lower-case one. Negative control: the same entries
/// under a session id's name are forwarded.
#[test]
#[cfg(unix)]
fn an_invalid_sid_is_never_joined_into_a_path() {
    use aterm_session::{LaunchNonce, SessionId};
    let _guard = crate::proxy::self_sock_test_guard();
    let dir = crate::test_scratch::short_dir("the planted host's socket");
    let dir = dir.path();
    let store = crate::session_store::new_store();
    let nonce = LaunchNonce::generate();
    let sock = dir.join("aterm-77010.sock");
    let sock_str = sock.to_string_lossy().into_owned();
    let _listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind the planted host");
    std::fs::write(dir.join("aterm-77010.token"), "cafe0123\n").expect("token");
    // `graph/../x` is `<dir>/x` once `graph/` exists: plant a live entry there,
    // by hand, since no id that is not a session id is ever written.
    crate::proxy::write_graph_entry(dir, &SessionId::generate(), &sock_str, &nonce);
    std::fs::write(
        dir.join("graph").join("../x"),
        format!("sock {sock_str}\nnonce {}\n", nonce.to_hex()),
    )
    .expect("plant `graph/../x`");
    assert!(
        dir.join("x").is_file(),
        "the planted entry sits outside graph/"
    );
    // Letters in it, so its upper-case spelling differs from it.
    let folded = SessionId::new("s-0a1b2c3d4e5f60718293");
    crate::proxy::write_graph_entry(dir, &folded, &sock_str, &nonce);

    for line in [
        "@../x text",
        "subscribe @../x screen",
        "@./../x text",
        "@s-0A1B2C3D4E5F60718293 text",
        "subscribe @s-0A1B2C3D4E5F60718293 screen",
    ] {
        assert!(
            plan_for(line, Scope::Owner, &store, dir).is_none(),
            "{line:?} must not reach the planted entry"
        );
        assert!(!request_waits(line, Scope::Owner, &store), "{line:?}");
    }
    let selector = control_request::head("@../x text")
        .selector
        .expect("a selector");
    assert_eq!(selector, Selector::Invalid("../x"));
    assert!(matches!(resolve(&store, &selector), Resolution::Missing));

    // Negative control: the same entry under a session id's name forwards.
    let sid = SessionId::generate();
    crate::proxy::write_graph_entry(dir, &sid, &sock_str, &nonce);
    for line in [
        format!("@{} text", sid.as_str()),
        format!("@{} text", folded.as_str()),
    ] {
        assert!(
            plan_for(&line, Scope::Owner, &store, dir).is_some(),
            "{line:?}: the planted host is reachable by a session id"
        );
    }
}

/// One request line is tokenized once, and every stage reads that one parse: the
/// head, the body a frame announces, the session a relay would carry it to and
/// the lane it takes agree on lines with leading separators, tabs and runs of
/// spaces, and a payload keeps its spacing. Negative control: the dispatcher's
/// old head (split at single spaces) and the old frame detector (any whitespace)
/// disagree on the first two lines, so a corpus like this one tells two
/// tokenizers apart.
#[test]
fn head_tokenizes_once_for_every_stage() {
    /// The dispatcher's request head before the one grammar (frozen copy).
    fn legacy_head_verb(line: &str) -> &str {
        let tail = match line.split_once(' ') {
            Some((first, tail)) if first.starts_with('@') => tail,
            _ => line,
        };
        tail.split_once(' ').map_or(tail, |(verb, _)| verb)
    }
    /// The frame detector's verb before the one grammar (frozen copy).
    fn legacy_frame_verb(line: &str) -> Option<&str> {
        let mut words = line.split_whitespace();
        match words.next() {
            Some(first) if first.starts_with('@') => words.next(),
            other => other,
        }
    }

    let store = session_store::new_store();
    let remote = SessionId::generate();
    let remote = remote.as_str();
    struct Case<'a> {
        line: String,
        selector: Option<Selector<'a>>,
        verb: &'a str,
        rest: &'a str,
        body: usize,
        relay: Option<&'a str>,
        wait: bool,
    }
    let remote_sel = Selector::parse(remote);
    let subscribed = format!("@{remote} screen");
    let cases = [
        Case {
            line: format!("@{remote}\tfeed-bin 5"),
            selector: Some(remote_sel),
            verb: "feed-bin",
            rest: "5",
            body: 5,
            relay: Some(remote),
            wait: true,
        },
        Case {
            line: "  feed-bin 5".to_string(),
            selector: None,
            verb: "feed-bin",
            rest: "5",
            body: 5,
            relay: None,
            wait: false,
        },
        Case {
            line: "send  hello".to_string(),
            selector: None,
            verb: "send",
            rest: " hello",
            body: 0,
            relay: None,
            wait: false,
        },
        Case {
            line: format!("subscribe\t@{remote} screen"),
            selector: None,
            verb: "subscribe",
            rest: &subscribed,
            body: 0,
            relay: Some(remote),
            wait: true,
        },
    ];
    for case in &cases {
        let request = Request::parse(case.line.clone());
        let head = request.head();
        assert_eq!(head, control_request::head(&case.line), "{:?}", case.line);
        assert_eq!(
            (head.selector, head.verb, head.rest),
            (case.selector, case.verb, case.rest),
            "{:?}",
            case.line
        );
        assert_eq!(request.body_len(), case.body, "{:?}: body", case.line);
        assert_eq!(
            announced_body_of(&case.line),
            case.body,
            "{:?}: lane",
            case.line
        );
        assert_eq!(
            relay_candidate(&request, Scope::Owner).map(|sid| sid.as_str()),
            case.relay,
            "{:?}: relay",
            case.line
        );
        assert_eq!(
            is_wait_request(&request, Scope::Owner, &store),
            case.wait,
            "{:?}: lane",
            case.line
        );
    }
    for case in &cases[..2] {
        assert_ne!(
            Some(legacy_head_verb(&case.line)),
            legacy_frame_verb(&case.line),
            "{:?}: the old stages read two verbs",
            case.line
        );
    }
}

/// Tier-1 conformance of the router to `dispatch_complete_model`'s
/// `ForwardableRemoteAlwaysForwarded`: for an Owner reaching a session another
/// instance hosts, `proxy_forward_plan` forwards a verb iff it is forwardable,
/// which is its row's target: a verb that acts on a session or an instance
/// (`Target::Session`, `Target::App`) is forwarded whatever its op class, the
/// owner verbs `topic` and `rekey` and the fine ops of `settings` and `copy`
/// included; an instance verb (`Target::Meta`) or an unknown one is answered
/// here, as the far instance would answer it. Drives the real decision over
/// every class; letting a forwardable verb fall to the local path fails it.
/// (The selector-second `subscribe` forward:
/// `a_nested_child_is_forwarded_with_its_own_token`.)
///
/// Negative control: the op-class rule this replaced (`required_op(verb)`
/// names an op) parts from the router on exactly `topic`, `rekey` (never
/// forwarded) and `who`, `hold`, `deliver` (forwarded to be refused there).
#[test]
fn proxy_forward_plan_conforms_to_dispatch_model() {
    use aterm_types::control_verbs::Target;
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    let dir = dir.path();
    let store = crate::session_store::new_store();
    let remote = SessionId::generate();
    hosting_instance(dir, 77050, &remote, "cafe0123");
    let sid = remote.as_str();

    // Representative verb lines across EVERY class: read, write, signal and
    // fine-op verbs on a session or an instance, owner verbs on a session, and
    // instance verbs.
    let cases: &[&str] = &[
        "screen",
        "text",
        "cell 0 0",
        "search x",
        "modes",
        "image read",
        "image shot.png",
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
        "signal int",
        "copy",
        "story",
        "meta set title x",
        "topic triage",
        "rekey",
        "settings get font.size",
        "window",
        "spawn",
        "grant x",
        "revoke x",
        "whoami",
        "sessions",
        "version",
        "who",
        "hold on",
        "deliver x",
        "bogus",
    ];
    let mut parted = Vec::new();
    for verb_line in cases {
        let line = format!("@{sid} {verb_line}");
        let head = control_request::head(&line);
        let forwarded = plan_for(&line, Scope::Owner, &store, dir).is_some();
        let forwardable = matches!(
            aterm_types::control_verbs::spec_for(head.verb, head.rest).map(|form| form.row.target),
            Some(Target::Session | Target::App)
        );
        assert_eq!(
            forwarded, forwardable,
            "router ≠ model for `{verb_line}`: forwarded={forwarded} forwardable={forwardable}"
        );
        if required_op(head.verb).is_some() != forwarded {
            parted.push(head.verb.to_string());
        }
    }
    assert_eq!(
        parted,
        ["topic", "rekey", "who", "hold", "deliver"],
        "the op-class rule must part from the router where the target decides"
    );
}

/// The line a planned forward to `sock` presents for `line`, `TOKEN <token>
/// <line>`, with the token beside `sock` read as the forward reads it once
/// connected ([`crate::proxy::connect_hop`]).
fn presented(sock: &str, line: &str) -> String {
    let token = crate::proxy::read_sibling_token(sock)
        .unwrap_or_else(|| panic!("{sock}: no token to present"));
    crate::proxy::forward_first_line(&token, line)
}

/// An instance hosting the session `sid`, as this process finds it: the graph
/// entry it published, naming `<dir>/aterm-<pid>.sock` (directly in `dir`, as
/// `confine_proxy_sock` requires), and `token` beside that socket. Nothing
/// listens there unless the test binds it, since a plan never connects.
/// Returns the socket's path.
fn hosting_instance(
    dir: &std::path::Path,
    pid: u32,
    sid: &SessionId,
    token: &str,
) -> std::path::PathBuf {
    let _ = std::fs::create_dir_all(dir);
    let sock = dir.join(format!("aterm-{pid}.sock"));
    std::fs::write(dir.join(format!("aterm-{pid}.token")), format!("{token}\n"))
        .expect("the host's token");
    crate::proxy::write_graph_entry(dir, sid, &sock.to_string_lossy(), &LaunchNonce::generate());
    sock
}

/// Tier-1 conformance of the forward decision to
/// `aterm_models_session::no_transitive_authority_model` (`NoTransitiveAuthority`): a
/// forward is permitted only for an Owner-class connection, so a connection that
/// holds only an edge can never start a hop, and authority does not compose
/// across instances.
///
/// For read, write, signal, owner and subscribe lines aimed at a session another
/// instance hosts, `proxy_forward_plan` is driven for three connections: the
/// instance token (Owner), the inherited bridge fd, and one that arrived with an
/// edge, whose scope is what the real connect-time auth (`first_line_scope`)
/// makes of `TOKEN <edge> <line>` at a child holding the parent edges
/// `spawn::install_parent_edges` put there. The model's `owner` guard comes from
/// what each scope is, never from `is_owner_class` (the code under test);
/// `forwarded` is the real plan. Every `Arrive`/`Forward` step must be admitted.
///
/// Negative control: `Buggy = 1` (owner guard waived) must reject the steps, since
/// the real router denies forwards the mutant permits. Removing the owner-class
/// test from `relay_candidate` fails this.
#[test]
fn proxy_forward_plan_conforms_to_no_transitive_authority() {
    use aterm_session::EdgeToken;
    use aterm_spec::verify::validate_transition_tiered;
    use std::collections::BTreeMap;
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    let dir = dir.path();
    let store = crate::session_store::new_store();
    // A child, as a session of its own holding the edges its parent minted.
    let at_child = registered_session(94, -1, b"");
    let (read, write, signal) = (
        EdgeToken::generate(),
        EdgeToken::generate(),
        EdgeToken::generate(),
    );
    let installed = crate::spawn::install_parent_edges(
        &mut at_child.ctx.edges.lock().unwrap(),
        &at_child.sid,
        &at_child.nonce,
        Some(SessionId::generate().as_str()),
        Some(&read.to_hex()),
        Some(&write.to_hex()),
        Some(&signal.to_hex()),
    );
    assert_eq!(installed, 3, "the child holds all three parent edges");
    let child_instance_token = "0f".repeat(32);
    // What each line is aimed at: a session another instance hosts.
    let remote = SessionId::generate();
    hosting_instance(dir, 77060, &remote, "cafe0123");
    let sid = remote.as_str();
    let lines = [
        (format!("@{sid} screen"), read),
        (format!("@{sid} key up"), write),
        (format!("@{sid} signal int"), signal),
        (format!("@{sid} topic triage"), write),
        (format!("subscribe @{sid} cells,bytes"), read),
    ];
    // What each scope is (see the doc): stated here, not read from the router.
    let owner_authority = |scope: Scope| match scope {
        Scope::Owner | Scope::Bridge => true,
        Scope::Edge(_) | Scope::Fleet { .. } => false,
    };
    let model = aterm_models_session::no_transitive_authority_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let init = model.init_state();
    let mut buggy_rejections = 0;
    for (line, edge) in &lines {
        let edge_line = crate::proxy::forward_first_line(&edge.to_hex(), line);
        let (arrived_scope, _, _) = first_line_scope(
            edge_line.trim_end_matches('\n'),
            &child_instance_token,
            || Some(at_child.ctx.clone()),
        )
        .unwrap_or_else(|why| panic!("`{edge_line}` does not authenticate at the child: {why}"));
        assert!(
            matches!(arrived_scope, Scope::Edge(_)),
            "`{edge_line}`: an edge-authenticated connection must arrive with an edge scope"
        );
        for (who, scope) in [
            ("owner", Scope::Owner),
            ("bridge", Scope::Bridge),
            ("arrived", arrived_scope),
            // A fleet peer is never forwarded, even past its own gate.
            (
                "fleet",
                Scope::Fleet {
                    content: true,
                    drive: false,
                },
            ),
        ] {
            let owner = owner_authority(scope);
            let forwarded = plan_for(line, scope, &store, dir).is_some();
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
                "{who} `{line}`: an Owner-class connection must reach the hosting instance"
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
        2 * lines.len(),
        "the owner-waiving mutant must part from the real router on exactly the \
         non-owner connections' lines: the arrived edge's and the fleet peer's"
    );
}

/// Tier-1 conformance of the forward to `aterm_models_gui::proxy_forward_model`
/// (`ProxyForward`). A forward keeps the `@<sid>` selector, so what ends the chain
/// is the host: the entry it followed was published by the instance at that
/// socket, so there the sid is hosted locally, or (a stale entry) names the
/// host's own socket, which `resolve_sibling`'s self-dial guard refuses. A nested
/// child is such a host too: it is reached with its own token.
///
/// The onward question is therefore asked in the host's context: the relayed line
/// past its `TOKEN <host-token>` handshake (the token read beside the socket once
/// connected, so Owner there), planned against the host's store (hosting the sid,
/// and not) with the recorded self socket set to the host's. Each hop is validated
/// as `Forward` with `active` = whether the host would forward on.
///
/// Negative control: `Buggy = 1` (chain continues) must reject every real hop.
#[test]
fn sibling_forward_plan_conforms_to_proxy_forward_one_hop() {
    use aterm_spec::verify::validate_transition_tiered;
    use std::collections::BTreeMap;
    // A discovery state of its own: no other test's self socket is read here.
    let _guard = crate::proxy::self_sock_test_guard();
    crate::proxy::clear_self_sock();
    let dir_guard = crate::test_scratch::short_dir("the host's socket");
    let dir = dir_guard.path().to_path_buf();
    // The host: a live listener, its token, and its entries (one for a session
    // it hosts, one left for a session it no longer hosts).
    let hosted = registered_session(93, -1, b"");
    let host_sock = hosting_instance(&dir, 77011, &hosted.sid, "cafe0123");
    let host_sock_str = host_sock.to_string_lossy().into_owned();
    let _listener = aterm_uds::CtlListener::bind(&host_sock).expect("bind the host");
    let stale = SessionId::generate();
    crate::proxy::write_graph_entry(&dir, &stale, &host_sock_str, &LaunchNonce::generate());
    let here = crate::session_store::new_store();
    let hosting = crate::session_store::new_store();
    hosting.write().unwrap().register(hosted.clone());
    let emptied = crate::session_store::new_store();

    let model = aterm_models_gui::proxy_forward_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let before = model.init_state();
    assert_eq!(before, BTreeMap::from([("depth", 0), ("active", 1)]));
    for (what, sid, host_store) in [
        ("hosted there", hosted.sid.clone(), &hosting),
        ("a stale entry", stale, &emptied),
    ] {
        let sid = sid.as_str().to_string();
        for line in [
            format!("@{sid} screen"),
            format!("@{sid} key up"),
            format!("@{sid} topic triage"),
            format!("subscribe @{sid} cells,bytes"),
        ] {
            let plan = plan_for(&line, Scope::Owner, &here, &dir)
                .unwrap_or_else(|| panic!("{what} `{line}`: a live host must be forwarded to"));
            let first_line = presented(&plan, &line);
            let at_host = first_line
                .trim_end_matches('\n')
                .strip_prefix("TOKEN cafe0123 ")
                .unwrap_or_else(|| panic!("`{first_line}` does not present the host's token"));
            // Become the host for the onward question.
            crate::proxy::set_self_sock(&dir, &host_sock_str);
            let onward = plan_for(at_host, Scope::Owner, host_store, &dir);
            crate::proxy::clear_self_sock();
            let after = BTreeMap::from([("depth", 1), ("active", i64::from(onward.is_some()))]);
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
                "{what} `{line}` reaches the host as `{at_host}`, which it would \
                 forward on to {onward:?}; the model admits no such hop — {why}"
            );
            assert!(
                !model.action_enabled("Forward", &after),
                "{what} `{line}`: a second hop is possible after `{at_host}`"
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
}

/// A forward planned to a host that an update's successor replaces before the
/// forward connects (token file rewritten, then the same path bound) presents
/// the token beside the socket once connected, the successor's: a token taken
/// earlier is refused `ERR auth` there, and the caller is answered `ERR
/// forward`.
#[test]
#[cfg(unix)]
fn a_sibling_replaced_after_the_plan_is_sent_its_own_token() {
    use std::io::{BufRead, BufReader, Write};
    let dir_guard = crate::test_scratch::short_dir("the replaced sibling's socket");
    let dir = dir_guard.path().to_path_buf();
    let store = crate::session_store::new_store();
    let sid = SessionId::generate();
    let sock = hosting_instance(&dir, 77021, &sid, "deadbeef");
    let token_file = dir.join("aterm-77021.token");
    let predecessor = aterm_uds::CtlListener::bind(&sock).expect("bind the sibling");
    let line = format!("@{} text", sid.as_str());
    let plan = plan_for(&line, Scope::Owner, &store, &dir).expect("a live sibling is forwarded to");

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
        let hop = crate::proxy::connect_hop(&plan).map_err(|e| format!("no hop: {e:?}"))?;
        crate::proxy::relay_hop(hop, &line, &client_relay, &[]).map_err(|e| e.to_string())
    });
    // The reply ends either way (the relayed answer, or EOF from a failed forward); the
    // client then hangs up the whole socket ([`hang_up`]). The successor is joined last,
    // since a forward that never connected leaves it parked in `accept`.
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
        "the successor was sent a token read before the connect"
    );
}

/// Tier-1 conformance of the forward path to
/// `aterm_models_session::forward_handshake_model` (`ForwardHandshake`), its liveness
/// twin: the server holds a request the host must receive while the client waits
/// for the reply, so relaying must not wait on a fresh client read (e.g.
/// `fill_buf()` on the common empty buffer would wedge every forward).
///
/// End to end: a client sends one `@<sid> screen` line and waits; the server reads
/// it as the serve loop does and calls `try_proxy_forward`; a stand-in host reports
/// the handshake it receives (`Relay`: buffered -> relayed) and answers; the client
/// reading it is `ClientRecv`. Each step is validated against the model, which must
/// end in `Done`.
///
/// Negative control: `Buggy = 1` (a fresh read before the first relay) disables
/// `Relay`; with `fill_buf()` in `LaneReader::take_buffered` the host never gets
/// the request and the test reports the parked state.
#[test]
#[cfg(unix)]
fn try_proxy_forward_conforms_to_forward_handshake() {
    use aterm_spec::verify::validate_transition_tiered;
    use std::io::{BufRead, BufReader, Write};
    let dir_guard = crate::test_scratch::short_dir("the forward's host socket");
    let dir = dir_guard.path().to_path_buf();
    let sid = SessionId::generate();
    let sock = hosting_instance(&dir, 77070, &sid, "c0ffee02");
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind the host");
    let (handshake_tx, handshake_rx) = std::sync::mpsc::channel::<String>();
    let host_side = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept the forward");
        let mut reader = BufReader::new(conn.try_clone().expect("clone"));
        // The forward's connect probe comes first and is answered; the
        // handshake the model speaks of is the probe plus the verb.
        let mut probe = String::new();
        reader.read_line(&mut probe).expect("read the handshake");
        (&conn)
            .write_all(b"OK version=host\n")
            .expect("answer the probe");
        let mut verb = String::new();
        reader.read_line(&mut verb).expect("read the verb");
        let _ = handshake_tx.send(format!("{probe}{verb}"));
        (&conn).write_all(b"OK served\n").expect("answer");
        let _ = conn.shutdown(std::net::Shutdown::Both);
    });

    let (client, server_end) = CtlStream::pair().expect("pair");
    // The client must be served: a hang detector, never a latency budget.
    client
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("client deadline");
    (&client)
        .write_all(format!("@{} screen\n", sid.as_str()).as_bytes())
        .expect("send the request");
    let store = crate::session_store::new_store();
    let server_dir = dir.clone();
    let server = std::thread::spawn(move || {
        let mut unread = control_lanes::Unread::default();
        let mut reader = control_lanes::LaneReader::new(&server_end, &mut unread);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read the request");
        let line = line.trim_end_matches('\n');
        try_proxy_forward(
            &Request::parse(line.to_string()),
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
        format!("TOKEN c0ffee02 version\n@{} screen\n", sid.as_str()),
        "the host receives its own token's handshake (with the connect probe) and \
         the line as it came"
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
    // toward the client shut nothing), so the relay's client-to-host pump
    // hits EOF and the forward returns.
    hang_up(client);
    assert!(
        join_after_hang_up(server, "the forward to end on its client's hang-up"),
        "the forward owned the line"
    );
    join_after_hang_up(host_side, "the stand-in host to finish serving");
}

/// `parse_mouse`: the additive `mods=`/`count=`/`side=`/`block=` grammar, one labelled row
/// per input line. Legacy/SGR reports carry only shift, alt/meta and ctrl, so `super`,
/// `cmd`, `command` and `hyper` are refused rather than sending an unmodified press.
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

/// A bare Owner `key` with a native view in front (Settings) reaches that view's own
/// keyboard handling, so its arrows and Tab can be driven headless. A terminal front and an
/// explicit selector keep the session/PTY key path, an Edge is denied, and an unknown key
/// answers the key usage line.
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

/// [`app_route`]: an Edge is denied every App-target verb BEFORE its selector is
/// looked up, so it learns nothing about which sessions exist, and an aimed
/// `spawn`/`tab` (`@<sid> spawn …`, `@<sid> tab close <N>`) keeps the App verdict
/// every App-target selector gets; otherwise an edge with write-input on one
/// session could mint a tab in, or close any tab of, the window hosting it. An
/// Owner's selector is looked up once and becomes the aim (`@<sid> panes`
/// describes the window showing that session); flagless and `@.` forms aim at the
/// front window with no lookup. Negative control: the same dead selector from an
/// Owner is looked up and answers `ERR no such session`.
#[test]
fn app_route_denies_an_edge_before_it_looks() {
    let looked = std::cell::Cell::new(0);
    let dead = |_: &Selector<'_>| {
        looked.set(looked.get() + 1);
        Resolution::Missing
    };
    let edge = NativeControlPrincipal::Edge;
    let owner = NativeControlPrincipal::Owner;
    // Every aimed spelling, both selector forms (local id, stable sid), with a
    // well-formed and a malformed tail, and the unaimed forms: the verdict
    // precedes the lookup and the parse.
    for line in [
        "@0 spawn",
        "@0 spawn raise=maybe",
        "@s-abc spawn cwd=/w split=v",
        "@0 tab close 0",
        "@s-abc tab move 0 1",
        "@0 panes",
        "@0 hover on",
        "spawn",
        "@. tab close",
    ] {
        let Head { selector, .. } = control_request::head(line);
        assert_eq!(
            app_route(edge, selector.as_ref(), dead).err().as_deref(),
            Some("ERR denied\n"),
            "an Edge is refused {line:?}",
        );
    }
    assert_eq!(looked.get(), 0, "an Edge is refused before any lookup");

    let Head { selector, .. } = control_request::head("@0 spawn");
    assert_eq!(
        app_route(owner, selector.as_ref(), dead).err().as_deref(),
        Some("ERR no such session\n"),
    );
    assert_eq!(looked.get(), 1, "the Owner's selector is looked up, once");

    for line in [
        "spawn",
        "spawn window=1 raise=false",
        "@. spawn",
        "tab next",
        "panes",
    ] {
        let Head { selector, .. } = control_request::head(line);
        assert!(
            matches!(app_route(owner, selector.as_ref(), dead), Ok(AppAim::Front)),
            "{line:?} aims at the front window",
        );
    }
    assert_eq!(looked.get(), 1, "an unaimed form looks nothing up");

    // A hosted session is the aim, the session `cmd_spawn`, `cmd_tab_aimed` and
    // `cmd_panes` are handed; an id two instances answer to is refused by name.
    let handle = crate::session_store::test_handle(7);
    let hosted: Target = (
        handle.term.clone(),
        handle.master,
        handle.local_id,
        handle.ctx.clone(),
    );
    let Head { selector, .. } = control_request::head("@7 panes");
    let aim = app_route(owner, selector.as_ref(), |_| {
        Resolution::Hosted(hosted.clone())
    });
    assert_eq!(aim.ok().and_then(|aim| aim.session()), Some(7));
    assert_eq!(AppAim::Front.session(), None);
    assert_eq!(
        app_route(owner, selector.as_ref(), |_| {
            Resolution::Ambiguous("ERR ambiguous\n".to_string())
        })
        .err()
        .as_deref(),
        Some("ERR ambiguous\n"),
    );
}

/// A `pipe(2)` with both ends close-on-exec as early as the platform allows; every test in
/// this file should use it. Linux uses `pipe2(O_CLOEXEC)`. Darwin has no `pipe2`, and a
/// `mkfifo` carrier reports no `POLLHUP` once the last reader closes (hanging
/// `aterm_pty::poll_writable`), so it keeps `pipe(2)` + `F_SETFD`, with a narrow window in
/// which a racing spawn can hand a child an end. No assertion rests on that window:
/// dead-peer tests use [`kill_peer_in_kernel`]. The flag is read back on both descriptors.
#[cfg(unix)]
fn cloexec_pipe() -> (i32, i32) {
    let mut fds = [-1i32; 2];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        assert_eq!(
            // SAFETY: `pipe2` fills the two-element array this frame owns,
            // and flags both descriptors as it creates them.
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

/// A live pipe-backed session: a real `SinkWriter` over a pipe's write end, its own
/// `Terminal`, and a `SessionHandle` for `local_id` sharing the registry's `Arc`s. Returns
/// the read end, which holds what reached the session's PTY.
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
/// order with name, op, framing, target, access, fleet and summary, the fields the
/// text catalog renders plus whether a fleet peer may run the verb.
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
        assert_eq!(
            row.get("fleet").and_then(|x| x.as_bool()),
            Some(spec.fleet),
            "{}",
            spec.name
        );
        assert_eq!(field("summary"), spec.summary, "{}", spec.name);
    }
    assert!(cmd_help("--nope").starts_with("ERR usage: help"));
}

/// THE FLEET GATE: a fleet peer runs only a verb the table marks `fleet`, with
/// no selector and no frame — whatever its verb, its level or how the line is
/// spelled — and every refusal is counted in the audit trail's fleet gate run;
/// every other scope passes the gate untouched (its own gates judge it). The
/// live lane (`fleet_live_headless`) proves each refusal closes the
/// connection.
#[test]
fn the_fleet_gate_admits_only_fleet_verbs_with_no_selector_or_frame() {
    use aterm_types::control_verbs::VERBS;
    let audited = || control_denials::FLEET_GATE_DENIALS.network_total();
    let (before, mut refused) = (audited(), 0);
    for content in [false, true] {
        let fleet = Scope::Fleet {
            content,
            drive: false,
        };
        for served in VERBS.iter().filter(|spec| spec.fleet) {
            assert_eq!(
                fleet_refusal(&Request::parse(served.name.to_string()), fleet),
                None,
                "{}",
                served.name
            );
        }
        // A stray tail is the verb's own usage error, not the gate's, and the
        // one request grammar reads the verb past leading separators and up to
        // an HT as every other stage does.
        for line in ["version --json", " version", "version\tx"] {
            assert_eq!(
                fleet_refusal(&Request::parse(line.to_string()), fleet),
                None,
                "{line:?}"
            );
        }
        for line in [
            "@1 version",
            "@s-0123456789abcdef version",
            "@. version",
            "@self version",
            "@s-0123456789abcdef text",
            "feed-bin 5",
            "@1 feed-bin 5",
            "paste-bin 3",
            "post to=@s-0123456789abcdef kind=note len=3",
            "spawn",
            "turn hello",
            "settings set columns 80",
            "hold on reason=x",
            "dial lab version",
            "subscribe @* events",
            "inbox get 1",
            "sessions",
            "help",
            "whoami",
            "task mark done",
            "",
            "VERSION",
        ] {
            let refusal = fleet_refusal(&Request::parse(line.to_string()), fleet)
                .unwrap_or_else(|| panic!("{line:?} passed"));
            assert!(
                refusal.starts_with("ERR denied: the fleet serves ") && refusal.ends_with('\n'),
                "{line:?}: {refusal:?}"
            );
            assert_eq!(refusal.matches('\n').count(), 1, "one line: {refusal:?}");
            refused += 1;
        }
    }
    // Others may count too (the run is the process's): at least these.
    assert!(
        audited() - before >= refused,
        "every refusal is audited: {} of {refused}",
        audited() - before
    );
    // Every other scope passes the gate.
    for scope in [
        Scope::Owner,
        Scope::Bridge,
        Scope::Edge(aterm_session::EdgeToken::generate()),
    ] {
        for line in ["@1 text", "spawn", "feed-bin 5", "subscribe @* events"] {
            assert_eq!(
                fleet_refusal(&Request::parse(line.to_string()), scope),
                None,
                "{scope:?} {line:?}"
            );
        }
    }
}

/// THE EMPTY-SELECTOR GATE: a line whose leading `@` names nothing — a bare
/// `@` (what `@$W` is when `$W` is empty), a blank after it — is refused with
/// the one shared line, whatever verb follows; a named selector, a line with
/// none, and an `@` inside a verb's own arguments pass it. Read past the gate,
/// such a token is no selector at all: the head keeps it as the verb, so it
/// can never resolve to the active session.
#[test]
fn the_empty_selector_gate_refuses_an_at_that_names_nothing() {
    let refusal = format!("{}\n", aterm_types::control_verbs::EMPTY_SELECTOR);
    for line in [
        "@ task",
        "@",
        "@  turn typed=1 hi",
        "@\tclose",
        "@ send hi",
        "@ signal term",
        "@ feed-bin 3",
        "@ spawn agent=claude len=4",
        "@ subscribe events",
        " @ status",
    ] {
        assert_eq!(
            empty_selector_refusal(&Request::parse(line.to_string())).as_deref(),
            Some(refusal.as_str()),
            "{line:?}"
        );
    }
    for line in [
        "@. task",
        "@1 text",
        "@s-0123456789abcdef turn hi",
        "@* status",
        "task",
        "send @",
        "subscribe @. events",
        "await task @s-a,@.",
        "",
    ] {
        assert_eq!(
            empty_selector_refusal(&Request::parse(line.to_string())),
            None,
            "{line:?}"
        );
    }
    // Past the gate: no selector, and the `@` is the verb no handler answers.
    let Head {
        selector,
        verb,
        rest,
    } = control_request::head("@ task");
    assert!(selector.is_none());
    assert_eq!((verb, rest), ("@", "task"));
    let Head { selector, verb, .. } = control_request::head("@. task");
    assert!(matches!(selector, Some(Selector::SelfTok)));
    assert_eq!(verb, "task");
}

/// A FLEET PEER'S SURVEY: the gate admits `survey`, and it is answered at
/// the level the peer's listener proved, never more — a peer allowed facts
/// that asks `content` reads facts. Every other scope is left to the
/// owner-only gate, and every other verb too.
#[test]
fn a_fleet_peer_reads_the_survey_at_its_level() {
    let store = crate::session_store::new_store();
    store
        .write()
        .unwrap()
        .register(crate::session_store::test_handle(1));
    let level = |reply: &str| {
        reply
            .lines()
            .nth(1)
            .and_then(|head| head.split(' ').find_map(|kv| kv.strip_prefix("level=")))
            .map(str::to_owned)
    };
    for (content, rest, want) in [
        (false, "", "facts"),
        (false, "content", "facts"),
        (true, "", "content"),
        (true, "facts", "facts"),
    ] {
        let fleet = Scope::Fleet {
            content,
            drive: false,
        };
        let line = format!("survey {rest}");
        assert_eq!(
            fleet_refusal(&Request::parse(line.trim().to_string()), fleet),
            None,
            "{line:?}"
        );
        let reply = fleet_survey("survey", rest, fleet, &store).expect("a fleet survey");
        assert_eq!(
            level(&reply).as_deref(),
            Some(want),
            "{content} {rest:?}: {reply}"
        );
        let row = reply.lines().nth(2).expect("the session row");
        assert_eq!(row.contains(" cwd="), want == "content", "{row}");
    }
    assert_eq!(
        fleet_survey(
            "sessions",
            "",
            Scope::Fleet {
                content: true,
                drive: false
            },
            &store
        ),
        None
    );
    assert_eq!(fleet_survey("survey", "", Scope::Owner, &store), None);
}

/// An empty `turn` is a usage error unless it also submits nothing (`submit=none`, a
/// settle read), so a bare `ctl turn` never presses Enter; nothing is pasted or
/// pressed on refusal.
#[test]
#[cfg(unix)]
fn a_turn_with_no_text_is_usage_unless_it_submits_nothing() {
    let store = crate::session_store::new_store();
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
    let off = control_query::cmd_offscreen(
        &target.term,
        &target.ctx.frame,
        &format!("since={ORIGIN}:4"),
    );
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

    // Resolve `@2` (the target's local id) EXACTLY as the dispatch does.
    let Resolution::Hosted((term, _, _, ctx)) = resolve(&store, &Selector::parse("2")) else {
        panic!("@2 resolves");
    };
    let ctx: &SessionCtx = &ctx;

    // `key enter` cross-session: CR reaches the TARGET sink, nothing to self.
    assert_eq!(
        control_input::input_reply_to_str(background(
            "key",
            &term,
            ctx,
            parse_key("enter").unwrap()
        )),
        "OK\n"
    );
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

    // Raw feed/send retain their direct background route but now publish an
    // accepted-input receipt on that same resolved SessionCtx.
    let unlogged_before_feed = crate::unseamed_control_inputs();
    assert_eq!(
        background(
            "feed",
            &term,
            ctx,
            InputEvent::KeySequence(feed_bytes("03").unwrap())
        ),
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

    // `send` (the other always-cross writer) writes to the resolved sink, raw (no
    // implicit CR unless a literal trailing `\n` is given).
    let unlogged_before_send = crate::unseamed_control_inputs();
    assert_eq!(
        background(
            "send",
            &term,
            ctx,
            InputEvent::KeySequence(send_bytes("ls"))
        ),
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

/// Cross-session `resize` applies to the target term and pty only, never the active window:
/// the engine grid, the PTY winsize (over a real pty: on a pipe `TIOCSWINSZ` is a no-op) and
/// the target's asciicast geometry record, as `App::apply_term_resize` does for self.
/// Out-of-range requests reuse `parse_resize`'s errors and mutate nothing.
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
    // (2) PTY winsize.
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

/// Cross-session `mouse` (`cross_mouse_apply`) against the target in both tracking states:
/// on, a report goes to the target sink (`Ok(false)`); off, a wheel scrolls the target's
/// viewport (`Ok(true)`, repaint, no pty bytes) and a plain press is a no-op.
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

    // ── Tracking on (SGR mouse, DEC 1000 + 1006) ──
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

    // ── Tracking off (DEC 1000 / 1006 reset) ──
    term_lock(term).process(b"\x1b[?1006l\x1b[?1000l");
    assert!(!term_lock(term).mouse_tracking_enabled(), "tracking reset");
    term_lock(term).scroll_to_bottom();
    assert_eq!(term_lock(term).grid().display_offset(), 0, "at live tail");

    // Wheel-up: the scroll_display fallback moves the target viewport into history, emits
    // no pty bytes, and asks for a repaint.
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

/// The decision the op-scope gate at the top of `serve_session_verb` makes, kept in lockstep
/// with its inline `match (scope, required_op(verb))` so it is testable without an
/// `EventLoopProxy`: the deny path returns before any proxy use.
fn gate_allows(scope: Scope, verb: &str, active: &SessionCtx) -> bool {
    // An Edge passes only if its token holds the verb's op on the now-active session, so it
    // cannot drive a session it was never granted on after the active handle moves.
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

/// [`route`]: every row of the table, a listed sub-form row as its verb, goes to
/// the class handler its `(access, target)` names, and a verb the table does not
/// list is refused before any handler (a retired verb by its tombstone, any other
/// by `ERR unknown verb`), so no arm can serve a verb outside the table.
#[test]
fn the_router_routes_every_row_by_its_class_and_refuses_the_rest() {
    use aterm_types::control_verbs::{VERBS, spec_for};
    let route_of = |verb: &str, rest: &str| route(verb, spec_for(verb, rest));
    for row in VERBS {
        let (verb, sub) = row.name.split_once(' ').unwrap_or((row.name, ""));
        let (routed, form) = route_of(verb, sub)
            .unwrap_or_else(|refusal| panic!("{}: refused {refusal:?}", row.name));
        assert_eq!(form.row.name, row.name, "{} keeps its own form", row.name);
        assert_eq!(
            Ok(routed),
            route_of(verb, "").map(|(base, _)| base),
            "{} routes as its verb",
            row.name
        );
    }
    for (line, want) in [
        ("version", Route::Meta),
        ("privacy --json", Route::Meta),
        ("deliver s-a off=1", Route::Bridge),
        ("outbox sent s-a 1 off=9", Route::Bridge),
        ("hold s-a on", Route::Owner),
        ("appnotice update hi", Route::Owner),
        ("story approved", Route::Owner),
        ("flows", Route::Owner),
        ("window", Route::App),
        ("hwkey a", Route::App),
        ("theme set Nord", Route::App),
        ("panes", Route::App),
        ("text", Route::Session),
        ("inbox seen 3", Route::Session),
        ("subscribe screen", Route::Session),
    ] {
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        assert_eq!(route_of(verb, rest).map(|(r, _)| r), Ok(want), "{line:?}");
    }
    for verb in ["bogus", "Text", "theme-set", ""] {
        assert_eq!(
            route_of(verb, "").err().as_deref(),
            Some("ERR unknown verb (try: help)\n"),
            "{verb:?}"
        );
    }
    assert_eq!(
        route_of("dial", "").err().as_deref(),
        retired_verb_error("dial").as_deref(),
        "a retired verb answers its tombstone"
    );
}

/// [`owner_aim`] over every owner-only row: only the session-authority verbs
/// (`grant`, `revoke`, `whoami`) and a bare `topic` or `story` ask for the
/// connection's own session, so every other owner verb, a new row included,
/// answers with no terminal or a native tab in front. A selector naming another
/// session is the verb's own aim; `@.` is the connection's.
#[test]
fn owner_verbs_need_no_terminal_except_the_session_authority_ones() {
    use aterm_types::control_verbs::{Access, VERBS};
    let own_session = ["grant", "revoke", "whoami", "topic", "story"];
    for row in VERBS
        .iter()
        .filter(|row| matches!(row.access, Access::OwnerOnly))
    {
        let verb = row.name.split_once(' ').map_or(row.name, |(verb, _)| verb);
        let want = if own_session.contains(&verb) {
            OwnerAim::OwnSession
        } else {
            OwnerAim::Instance
        };
        assert_eq!(owner_aim(verb, None), want, "{}", row.name);
    }
    for (verb, selector, want) in [
        ("topic", "s-a1", OwnerAim::Instance),
        ("topic", "s-x", OwnerAim::Instance),
        ("story", "0", OwnerAim::Instance),
        ("story", ".", OwnerAim::OwnSession),
        ("topic", ".", OwnerAim::OwnSession),
        ("flows", ".", OwnerAim::Instance),
    ] {
        assert_eq!(
            owner_aim(verb, Some(&Selector::parse(selector))),
            want,
            "@{selector} {verb}"
        );
    }
}

/// [`dispatch_authorized`] for `verb rest` over its table form, as the router
/// hands it to the gate.
fn line_authorized(scope: Scope, verb: &str, rest: &str, ctx: &SessionCtx) -> bool {
    let form = aterm_types::control_verbs::spec_for(verb, rest).expect("a table verb");
    dispatch_authorized(scope, &form, verb, rest, ctx)
}

/// What `verb rest` needs at the dispatch gate: [`need_of`] over its table form.
fn request_need(verb: &str, rest: &str) -> Option<Need> {
    aterm_types::control_verbs::spec_for(verb, rest).and_then(|form| need_of(&form, verb, rest))
}

/// What `verb rest` needs at the dispatch gate BEYOND its verb's base op
/// ([`required_op`]): [`request_need`] when the two differ, `None` when the base op
/// is the whole requirement.
fn escalation(verb: &str, rest: &str) -> Option<Need> {
    let need = request_need(verb, rest);
    need.filter(|_| need != required_op(verb).map(Need::Op))
}

/// The relay's and the cross-session gate's verb-only projection
/// ([`required_op`]) agrees with the dispatch gate ([`need_of`]) on every bare
/// row of the table.
#[test]
fn the_verb_only_projection_agrees_with_the_dispatch_gate() {
    use aterm_types::control_verbs::VERBS;
    for row in VERBS {
        assert_eq!(
            request_need(row.name, ""),
            required_op(row.name).map(Need::Op),
            "{}",
            row.name
        );
    }
}

/// `invoke <clipboard/config action>` and `open prefs|settings` escalate their op on
/// the argument, so a plain WriteInput edge cannot reach the fenced fine ops
/// through them: WriteInput is denied, Owner and a granted
/// ClipboardWrite/ConfigWrite edge are allowed. For `update`, `status` is an
/// AnyScopeMeta read while `check` (stages) and `apply` (re-execs) are owner-only,
/// like `invoke ApplyUpdate`/`open update`, so a ReadScreen edge cannot re-exec.
#[test]
fn update_check_and_apply_are_owner_only_subcommands() {
    // What `cmd_update` asks: the table's `update` form for the request.
    let owner_only = |rest: &str| {
        aterm_types::control_verbs::spec_for("update", rest).is_some_and(|form| form.owner_only)
    };
    assert!(owner_only("check"));
    assert!(owner_only("apply"));
    assert!(owner_only("  apply  "));
    // The sub-command word decides, whatever follows it: a malformed `check` is
    // refused to an edge before it can read as a usage error.
    assert!(owner_only("check extra"));
    // Reads stay any-scope so an in-session observer can SEE update state.
    assert!(!owner_only(""));
    assert!(!owner_only("status"));
    assert!(!owner_only("bogus"));
}

/// The argument-dependent fences ([`escalation`]: what [`need_of`] asks beyond
/// the verb's base op). `invoke`/`open`/`act` escalate to the fine op their
/// target needs; owner-only surfaces (the palette gateway, update re-exec,
/// standing connection authority, `family <sid>`, `pointer click`, `brief all`)
/// to `OwnerOnly`. `find` escalates for nothing: escalation replaces the base op,
/// and every `find` form answers with a match position, so it would give a
/// keystroke-only edge a screen read (the dispatch requires write on top instead).
/// The spawn fence tokenizes as `parse_spawn_args` does, so quoting or position
/// cannot walk a `connected=` past it.
#[test]
fn escalated_op_fences_argument_dependent_seams() {
    use Need::{Op as EOp, OwnerOnly};
    const SETTINGS_ACT: &str =
        "app/v1 view 42 settings/control/allow_osc52_query settings/set/allow_osc52_query true";
    let clipboard = Some(EOp(Op::ClipboardWrite));
    let config = Some(EOp(Op::ConfigWrite));
    let write = Some(EOp(Op::WriteInput));
    let owner = Some(OwnerOnly);
    let rows: &[(&str, &str, Option<Need>)] = &[
        ("invoke", "Copy", clipboard),
        ("invoke", "Paste", clipboard),
        ("invoke", "SelectAll", clipboard),
        ("invoke", "ToggleSettings", config),
        ("invoke", "Preferences", config),
        ("invoke", "OpenPalette", owner),
        ("invoke", "SoftwareUpdate", owner),
        ("invoke", "ApplyUpdate", owner),
        // The Overview (and the map's old name, its alias) lists every
        // session: Owner-only, the `open overview` arm's class.
        ("invoke", "Overview", owner),
        ("invoke", "ShowConnectionMap", owner),
        // The retired connection ids and presets name no action: nothing to
        // classify (`invoke` answers them `ERR retired`, below).
        ("invoke", "NewControlledWindow", None),
        ("invoke", "NewControlledTab", None),
        ("invoke", "NewControllerWindow", None),
        ("invoke", "NewControllerTab", None),
        ("invoke", "ConnectToSession", None),
        ("invoke", "ConfigureConnection", None),
        ("invoke", "DisconnectSession", None),
        // The person's own acts: Owner-only at the gate (and refused to the
        // Owner token before the hop, below).
        ("invoke", "TakeOver", owner),
        ("invoke", "HandBack", owner),
        ("invoke", "HoldSession", owner),
        ("invoke", "LiftHold", owner),
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
        // A worker (`agent=`, its `model=`, its `from=` and its task frame's
        // `len=`) starts an agent in a folder trusted for it: Owner-only in
        // every spelling.
        ("spawn", "agent=claude len=5", owner),
        ("spawn", "cwd=/w agent=claude model=opus len=5", owner),
        ("spawn", "\"agent=claude\" len=5", owner),
        ("spawn", "agent=claude from=s-ab12 len=5", owner),
        ("spawn", "model=opus", owner),
        ("spawn", "from=s-ab12", owner),
        ("spawn", "len=5", owner),
        ("spawn", "\"connected=controller\" of=s-abc", owner),
        (
            "spawn",
            "of=s-abc \"connected=controlled\" place=tab",
            owner,
        ),
        ("spawn", "\"connected=controller", owner),
        // The Overview carries the `flows` Owner gate (its open, its readout,
        // its capture and the `close` dismiss) under its keyword and the
        // map's old `connections` alike.
        ("open", "overview", owner),
        ("controls", "overview", owner),
        ("window", "overview", owner),
        ("open", "overview close", owner),
        ("open", "connections", owner),
        ("controls", "connections", owner),
        ("window", "connections", owner),
        ("open", "connections close", owner),
        ("controls", "front", None),
        ("window", "", None),
        ("window", "front shot.png", None),
        ("meta", "set title build agent", write),
        ("meta", "unset icon", write),
        ("meta", "set role reviewer", write),
        ("meta", "unset attention", write),
        ("meta", "", None),
        ("timeline", "10 since=3", None),
        // `log` is a plain read whatever it asks for (`tier=content` included:
        // the OSC title `text` and `timeline` already show at ReadScreen);
        // `brief` reads its target, and `brief all` the whole roster, Owner-only,
        // wherever `all` stands among its arguments (its parser takes it there).
        ("log", "tier=content --json", None),
        // `log n` is the INSTANCE's node log (session log 5J-3): Owner-only like
        // `brief all`, wherever the bare `n` stands; `n=<count>` is a count.
        ("log", "n", owner),
        ("log", "since=1 ep=2 n --json", owner),
        ("log", "n=5", None),
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
        // A wheel scrolls (or, under DEC mouse tracking, reports) as `move` does.
        ("pointer", "wheel dy=3 key=settings/search", None),
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
        assert_eq!(escalation(verb, rest), want, "{verb} {rest:?}");
    }
    // Before any hop, `invoke` answers a retired name `ERR retired` and the
    // person's own acts `ERR denied`, to the Owner token too. CONTROL: an
    // Owner-only action that is not the person's goes on to the main thread.
    for retired in [
        "NewControlledWindow",
        "NewControllerTab",
        "ConnectToSession",
        "ConfigureConnection",
        "DisconnectSession",
    ] {
        assert!(
            super::control_media::invoke_refusal(retired)
                .is_some_and(|r| r.starts_with("ERR retired: ")),
            "{retired}"
        );
    }
    for name in [
        "TakeOver",
        "HandBack",
        "HoldSession",
        "LiftHold",
        "ToggleStatsLens",
    ] {
        let refusal = super::control_media::invoke_refusal(name).expect("refused");
        assert!(refusal.starts_with("ERR denied"), "{name}: {refusal}");
    }
    // The stats lens is the person's close-up (design §12.2): its refusal
    // names where the person opens it.
    let lens = super::control_media::invoke_refusal("ToggleStatsLens").expect("refused");
    assert!(lens.contains("\u{2318}I"), "{lens}");
    assert_eq!(super::control_media::invoke_refusal("OpenPalette"), None);
    assert_eq!(
        super::control_media::invoke_refusal("TotallyBogusAction"),
        None
    );
}

/// `theme` reads at its base class; `preview` and `revert` need `WriteInput`, and every
/// other sub-form word fails closed to `ConfigWrite`, the class of the forms that rewrite
/// `aterm.toml`. Negative control: the read forms add nothing.
#[test]
fn control_theme_sub_forms_escalate_to_the_class_they_need() {
    use Need::Op as EOp;
    for rest in [
        "",
        "status",
        "list",
        "palette",
        "palette session",
        "session",
        "session status",
    ] {
        assert_eq!(escalation("theme", rest), None, "{rest:?} is a read");
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
            escalation("theme", rest),
            Some(EOp(Op::WriteInput)),
            "{rest:?}"
        );
    }
    for rest in ["set Nord", "commit", "bogus", "SET Nord"] {
        assert_eq!(
            escalation("theme", rest),
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
        ("write", write, "pointer", "wheel dy=3", true),
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
            line_authorized(scope, verb, rest, &ctx),
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
    // URLs; the Overview raises and stops workers), so input verbs escalate
    // identically.
    for kind in [OverlayKind::Palette, OverlayKind::Overview] {
        for verb in [
            "key", "ctrl", "mouse", "paste", "focus", "send", "feed", "turn",
        ] {
            assert_eq!(
                front_drive_escalation(scoped, input_of(verb), FrontControlSurface::Overlay(kind)),
                Some(Need::OwnerOnly),
                "{verb} must be owner-only under {kind:?}"
            );
        }
    }

    // OWNER is ALWAYS exempt — the owner's established control path is untouched.
    for kind in [OverlayKind::Palette, OverlayKind::Overview] {
        assert_eq!(
            front_drive_escalation(owner, input_of("key"), FrontControlSurface::Overlay(kind)),
            None,
            "Owner may drive its own {kind:?} overlay"
        );
    }

    // Native Settings is not an Overlay at all, but it still consumes the
    // same key/mouse vocabulary and can emit ConfigPatch effects.
    let escalation =
        front_drive_escalation(scoped, input_of("key"), FrontControlSurface::NativeSettings);
    assert_eq!(escalation, Some(Need::Op(Op::ConfigWrite)));
    let ctx = test_ctx();
    assert!(
        !scope_holds_need(
            edge_granted(Op::WriteInput, &ctx),
            escalation.unwrap(),
            &ctx
        ),
        "a WriteInput edge cannot drive native Settings"
    );
    assert!(scope_holds_need(
        edge_granted(Op::ConfigWrite, &ctx),
        escalation.unwrap(),
        &ctx,
    ));

    // No privileged front consumer => preserve the verb's ordinary authority.
    assert_eq!(
        front_drive_escalation(scoped, input_of("key"), FrontControlSurface::None),
        None
    );
    assert_eq!(
        front_drive_escalation(scoped, input_of("key"), FrontControlSurface::OtherNative),
        None
    );
    // A non-input verb is never dynamically escalated.
    assert_eq!(
        front_drive_escalation(
            scoped,
            input_of("text"),
            FrontControlSurface::NativeSettings
        ),
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
    let escalation =
        front_drive_escalation(write_edge, input_of("key"), app.front_control_surface())
            .expect("native Settings replaces the base input authority");
    assert!(!scope_holds_need(write_edge, escalation, &ctx));
}

/// Settings ▸ Harness's session presses are Owner-only, like `meta set autopilot`: a press
/// writes a session's Autopilot word (Off to Cruise puts the harness's hands back on a
/// session its owner took them off), which the Settings fine op must not reach.
///
/// END TO END NO EDGE REACHES THE PAGE TODAY, whatever it holds: `act` (like
/// `settings`) is an App-target verb, which `dispatch_app` answers `ERR denied`
/// for every Edge (`app_route`, by `native_control_decision`'s App arm) before
/// `need_of` is asked; and an Edge's flagless `key`/`mouse`/`paste` are pinned to the session
/// its token was checked against (`front_routed`), which `input_to_session` hands to
/// that session and never to `native_input_event`. That gate is asserted first. The
/// two fences below are DEFENCE IN DEPTH, read at their own predicates (the main-
/// thread hop and the dispatcher need an event loop): on the semantic `act` path and
/// on input keyed at the front Settings surface while it shows the page, a ConfigWrite
/// edge is denied a session's press — and the `Every session` row's, which writes
/// every session's word (design §11.5.2) — and Owner passes. NEGATIVE CONTROLS: at
/// the same predicates, that edge's Refresh and other Settings controls, an `all`
/// key spelled outside the session prefix, and its input on any other route, are
/// the ordinary ConfigWrite fence; and Owner's App verdict is the sessionless lane.
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
        "an Edge's `act` is ERR denied before need_of"
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
    // The `Every session` row's keys (design §11.5.2), as the page mints them:
    // one press writes every session's word, so they stay behind the same fence.
    let every: Vec<String> = [
        crate::harness_screen::EveryPress::Mode(aterm_agent::supervise::Autopilot::Cruise),
        crate::harness_screen::EveryPress::Mode(aterm_agent::supervise::Autopilot::Land),
        crate::harness_screen::EveryPress::Timer(crate::autopilot_timers::TimerChoice::In(120)),
        crate::harness_screen::EveryPress::Timer(crate::autopilot_timers::TimerChoice::At),
        crate::harness_screen::EveryPress::Follow,
    ]
    .into_iter()
    .map(crate::harness_screen::every_action)
    .chain([format!(
        "{}{}/timer",
        crate::harness_screen::SESSION_ACTION_PREFIX,
        crate::harness_screen::EVERY_ID
    )])
    .collect();
    for action in [
        "harness/session/7/autopilot/cruise",
        "harness/session/7/land-in/120",
        "harness/session/7/land-at",
        "harness/session/7/land-cancel",
        "harness/session/7/show",
    ]
    .into_iter()
    .chain(every.iter().map(String::as_str))
    {
        let press = format!("app/v1 view 42 {action} {action}");
        assert_eq!(escalation("act", &press), Some(Need::OwnerOnly), "{action}");
        assert!(
            !line_authorized(config_edge, "act", &press, &ctx),
            "a ConfigWrite edge is DENIED a session's {action}"
        );
        assert!(
            line_authorized(owner, "act", &press, &ctx),
            "Owner presses {action}"
        );
    }
    // CONTROL: an `all` key spelled outside the prefix would escape the Owner
    // fence to the ordinary ConfigWrite one — the prefix is what holds it.
    for rest in [
        "app/v1 view 42 harness/refresh harness/refresh",
        "app/v1 view 42 settings/control/harness.enabled settings/set/harness.enabled false",
        "app/v1 view 42 harness/all/autopilot/land harness/all/autopilot/land",
    ] {
        assert_eq!(
            escalation("act", rest),
            Some(Need::Op(Op::ConfigWrite)),
            "{rest}"
        );
        assert!(line_authorized(config_edge, "act", rest, &ctx), "{rest}");
    }

    // The front path: keys and clicks at the Settings tab while it shows the page.
    let mut app = crate::App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Harness));
    assert_eq!(
        app.front_control_surface(),
        FrontControlSurface::HarnessSettings
    );
    for verb in ["key", "mouse", "paste"] {
        let need = front_drive_escalation(config_edge, input_of(verb), app.front_control_surface())
            .expect("the page replaces the base input authority");
        assert_eq!(need, Need::OwnerOnly, "{verb}");
        assert!(
            !scope_holds_need(config_edge, need, &ctx),
            "a ConfigWrite edge is DENIED {verb} on the Harness page"
        );
        assert!(scope_holds_need(owner, need, &ctx));
        assert_eq!(
            front_drive_escalation(owner, input_of(verb), app.front_control_surface()),
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
    let need = front_drive_escalation(config_edge, input_of("key"), app.front_control_surface())
        .expect("native Settings");
    assert!(scope_holds_need(config_edge, need, &ctx));
}

/// Copy-on-select exfil fence (the decision): a scoped-edge `mouse` gesture has its
/// copy-on-select clipboard effect suppressed, while Owner and a real human gesture do not,
/// mirroring `front_drive_escalation`. The seam honouring it is pinned by
/// `copy_on_select_fires_only_on_completed_drag_when_enabled`.
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
    // A granted ReadScreen token reports its real effective op.
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

    // Owner mints a ReadScreen edge from some source session into this session.
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

    // Mint a full `Both` connection (three rows) through the one mint path
    // (`grant_connection`): the graph the sweep must dissolve whole.
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
    // The hex-free obligation: no captured line anywhere carries a token.
    for hex in [hex1, hex2] {
        let leaked = crate::test_log::containing(hex);
        assert!(
            leaked.is_empty(),
            "a token hex leaked into a log line: {leaked:?}"
        );
    }
}

/// `resize`, the cell form `resize <r> <c>` and the window form `resize px <w> <h>`, one
/// labelled row per input line, through `parse_resize`/`parse_resize_px` (the verb itself
/// hops to the main thread). The cell form is bounded to 1..=MAX_GRID_ROWS/COLS. The `px`
/// token, not the magnitudes, decides the form: `100 40` is also a valid cell geometry, and
/// `px` exists to reach the live-drag path. One parser keeps the px form's three routers
/// (the plain verb, the front-routed cross-session twin, the native-front window lane) equal.
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
        // A pixel pair that is also in cell range is still not a cell geometry.
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
    let reply = cmd_cast(&ctx.cast);
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
    let reply2 = cmd_cast(&ctx.cast);
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
    let reg = Arc::<crate::session_watch::Watchers>::default();
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
    let reg = Arc::<crate::session_watch::Watchers>::default();
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
    let reg = Arc::<crate::session_watch::Watchers>::default();
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

    // A new command is now executing: the stale completion above must not be returned,
    // so with nothing completing in time `wait` times out.
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
    // The token is the multi-codepoint cluster, not a bare codepoint.
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
    // The absolute row resolves (via abs_row_text_within, the `line`/`text`
    // space) to a row whose text contains the faithful cluster.
    let row = term_lock(&term).abs_row_text_within(abs_row, usize::MAX);
    let row_text = match row {
        aterm_core::terminal::AbsRowText::Text(t) => t,
        aterm_core::terminal::AbsRowText::Evicted => {
            panic!("search row {abs_row} unexpectedly evicted")
        }
        aterm_core::terminal::AbsRowText::OutOfRange => panic!("search row {abs_row} out of range"),
        aterm_core::terminal::AbsRowText::Unreadable => panic!("search row {abs_row} unreadable"),
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

// ---- cross-session @selector addressing ----------------------------------------

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
    let observed = crate::session_watch::Observed::default();
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
        turns: observed.turns,
        meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
        timeline: observed.timeline,
        fabric: std::sync::Arc::default(),
        idem: Default::default(),
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        licence: Default::default(),
        launch: Default::default(),
        reset_lane: Default::default(),
        crash_journal: Default::default(),
        typed: Default::default(),
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
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
    }
}

/// (a) `@.` names the connection's own session: the resolver answers `SelfTok`
/// without a lookup, even with nothing registered, so the dispatch serves it from
/// the very tuple a flagless line gets. A bare `@` (an empty body, what `@$W` is
/// when `$W` is empty) or a blank one names nothing, so it is never self.
#[test]
fn at_dot_selector_resolves_self_verbatim() {
    let empty = session_store::new_store();
    let sel = Selector::parse(".");
    assert!(sel.is_self(), "@. names self");
    assert!(
        matches!(resolve(&empty, &sel), Resolution::SelfTok),
        "@. is self whatever the registry holds"
    );
    for nothing in ["", " ", "\t"] {
        assert!(
            !Selector::parse(nothing).is_self(),
            "@{nothing:?} names nothing"
        );
    }
    // Negative control: a number, even the self session's own, is a lookup.
    assert!(matches!(
        resolve(&empty, &Selector::parse("0")),
        Resolution::Missing
    ));
}

/// A second registered session is readable via `@<local>` and `@<sid>` by an Owner
/// connection, and returns its own state (its text), not self's.
#[test]
fn owner_reads_a_sibling_via_local_and_sid_selectors() {
    let store = session_store::new_store();
    let self_h = registered_session(0, -1, b"I-am-self\r\n");
    let peer_h = registered_session(7, -1, b"I-am-peer\r\n");
    store.write().unwrap().register(self_h.clone());
    store.write().unwrap().register(peer_h.clone());
    let hosted = |sel: &Selector<'_>| match resolve(&store, sel) {
        Resolution::Hosted(target) => Some(target),
        Resolution::SelfTok | Resolution::Missing | Resolution::Ambiguous(_) => None,
    };

    let self_text = cmd_text(&self_h.term);
    let peer_text = cmd_text(&peer_h.term);
    assert_ne!(self_text, peer_text, "the two sessions read distinctly");

    // By process-local id.
    let by_local = hosted(&Selector::parse("7")).expect("by local");
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
    let by_sid = hosted(&Selector::parse(peer_h.sid.as_str())).expect("by sid");
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

    // An unknown selector fails closed (no such session), and so does a
    // spelling that is no session id.
    for body in ["999", "s-0ff", "s-nope", "S-7"] {
        assert!(
            matches!(resolve(&store, &Selector::parse(body)), Resolution::Missing),
            "@{body}"
        );
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

    // Idle: no driver, no watcher.
    let out = cmd_who(&store);
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
    let _watch = crate::session_watch::Subscription::single(&root.ctx.watchers);
    let out = cmd_who(&store);
    let line = out.lines().nth(1).expect("one session line");
    assert!(
        line.contains("driving=9 watchers=1"),
        "driver + watcher surfaced: {line}"
    );

    // A cooperative drive lease surfaces as `driving=lease:<holder>`, so a raw driver's
    // hold is visible in the same presence readout as a turn.
    *root.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
        holder: "agent-a".to_string(),
        expires_us: crate::metrics::now_us() + 60_000_000,
        conn: None,
        hard: false,
        here: false,
        holder_pid: None,
    });
    let line = cmd_who(&store)
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
    let two = cmd_sessions_store(&store, None);
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
    // Re-setting the same value is a no-change (no repaint/notify fan-out).
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
        cmd_sessions_store(&store, None)
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
        cmd_sessions_store(&store, None)
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
        cmd_sessions_store(&store, None)
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

/// `meta`'s `worker=` IS THE AGENT A WORKER'S SPAWN STARTED, its task
/// record's (`TaskRecord::agent`), after `trusted_cwd=`: what the window
/// harness's trust rules read (a Gemini CLI worker's folder-trust dialog is
/// never pressed). NEGATIVE CONTROLS: a session with no task, and one whose
/// record a `task mark` made, print `worker=-`.
#[test]
fn meta_names_the_agent_a_workers_spawn_started() {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let read = || cmd_meta(&h.term, &store, 0, &h.ctx, "").0;
    assert!(
        read().contains(" trusted_cwd=- worker=- autopilot="),
        "{}",
        read()
    );
    mark_session_task(&h.ctx, 0, &TaskMark::new(TaskState::Working)).expect("unfenced");
    assert!(read().contains(" worker=- "), "a mark's record: {}", read());
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .set_task_agent(aterm_phase::Program::Muse);
    assert!(
        read().contains(" trusted_cwd=- worker=muse autopilot="),
        "{}",
        read()
    );
}

/// The Autopilot word on the wire: the supervisor's three modes and the owner's words for
/// two of them, stored as the mode's word and read back by key in every bare `meta`, just
/// before `land_at=`. Unset it reads `autopilot=-`, since the supervisor's loop reads the
/// word only off a reply that names the field. It counts toward `ls meta=1`, and `unset`
/// returns it to `[harness] autopilot`. Negative controls: anything else, an on/off word
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
        read0.ends_with(" autopilot=- land_at=- autopilot_by=- autopilot_since=-\n"),
        "always named, and no setter for a word never set: {read0}"
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
            read.contains(&format!(
                " autopilot={stored} land_at=- autopilot_by=ctl autopilot_since="
            )),
            "{read}"
        );
    }
    let (_, again) = cmd_meta(&h.term, &store, 0, &h.ctx, "set autopilot wrap");
    assert!(!again, "the owner's word for land is the same value");
    assert!(
        cmd_sessions_store(&store, None)
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
    // Who handed it back to the default stays beside the unset word.
    assert!(
        read.contains(" autopilot=- land_at=- autopilot_by=ctl autopilot_since="),
        "{read}"
    );
    // `land_at=` follows it (the Autopilot design, §9.3.5): a timed Cruise's
    // instant as stored, in unix seconds — printed only, no field of its own.
    h.ctx.meta.lock().unwrap().autopilot = Some("cruise".into());
    h.ctx.meta.lock().unwrap().land_at = Some(1_792_249_200_000);
    let (read, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(
        read.contains(" autopilot=cruise land_at=1792249200 "),
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

/// Writing `autopilot` is Owner-only, as `questions` is: an edge, even one granted write on
/// the session, must not put the harness's hands back on a session its owner took them
/// off. Other forms, a bare read included, fall through.
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
    // A same-state re-mark records nothing (change-gated).
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

// ── subscribe wiring ─────────────────────────────────────────────────────

/// How many of `store`'s sessions a subscription is registered on, the
/// instance list counting as one more: what a refused subscribe, or a push
/// loop that returned, must leave at zero.
fn watched_sessions(store: &Store) -> usize {
    let g = store.read().unwrap();
    let sessions = g
        .snapshot()
        .iter()
        .filter(|h| h.ctx.watchers.count() > 0)
        .count();
    sessions + usize::from(g.instance_watchers().count() > 0)
}

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
        let mut unread = control_lanes::Unread::default();
        let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
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

/// Accumulate pushed bytes from a `CtlStream` until `pred` matches the accumulated text or
/// the deadline passes; frames may arrive split or coalesced.
fn read_until(s: &CtlStream, mut acc: String, pred: impl Fn(&str) -> bool) -> String {
    use std::io::Read;
    let mut s = s.try_clone().expect("clone client end");
    s.set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    // A hang detector: the loop returns as soon as `pred` matches, and must clear the
    // slowest push path (the `sessions` stream's 250 ms tick) on a saturated machine.
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

// A test's client hangs up the whole socket ([`hang_up`]), so the production peer probe
// ends its push loop on the next tick whatever copies of the client end a fork holds.
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
    let (client, server) = CtlStream::pair().unwrap();
    // Every read below is bounded by a hang detector, set here on the fresh socket: macOS
    // refuses `SO_RCVTIMEO` (EINVAL) once both halves are shut.
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .expect("read timeout");
    let forked_copy = server
        .try_clone()
        .expect("a surviving copy of the server end");
    let (store_t, active_t) = (store.clone(), active.clone());
    let join = std::thread::spawn(move || {
        let mut w = server;
        run_subscribe_socket(
            control_request::head("subscribe @. screen"),
            &active_t,
            &store_t,
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

/// `topic` is Owner-class and selector-taking, and its refusal is audited. It takes
/// `@<sid>` so a manager can arm a worker's topics from its own window; it is Owner-class
/// because a topic decides what lands in an agent's inbox, so an edge gets the audited
/// refusal, not the generic gate's silent `ERR denied`.
#[test]
fn topic_is_owner_class_and_names_its_session() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    let sid = h.sid.clone();
    let h_ctx = h.ctx.clone();
    store.write().unwrap().register(h);

    // An edge token is refused, whatever it is a token for.
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    assert_eq!(
        dispatch_topic_verb(
            "ls",
            Some(&Selector::parse(sid.as_str())),
            edge,
            &store,
            None
        ),
        "ERR denied\n"
    );

    // Owner, naming the session: the empty set, which is the default.
    let owner_ls = |rest: &str| {
        dispatch_topic_verb(
            rest,
            Some(&Selector::parse(sid.as_str())),
            Scope::Owner,
            &store,
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
        .map(|e| e.payload.to_string())
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
        "add a since=head b",
        "drop A",
        "drop a b",
        "wat",
        "ls extra",
    ] {
        assert!(
            owner_ls(bad).starts_with("ERR usage: topic"),
            "`{bad}` must be a usage error: {}",
            owner_ls(bad)
        );
    }
    // A session nobody hosts, and a spelling that is no session id.
    for nobody in [aterm_session::SessionId::generate().as_str(), "s-nobody"] {
        assert_eq!(
            dispatch_topic_verb(
                "ls",
                Some(&Selector::parse(nobody)),
                Scope::Owner,
                &store,
                None,
            ),
            "ERR no such session\n",
            "{nobody}"
        );
    }
}

/// One session opts into at most `MAX_TOPICS` topics: every opt-in rides the session's
/// handoff record, which the writer may not shed, so unbounded topics would grow the
/// manifest past the cap a successor reads it under. An `add` past the bound is refused
/// and pushes nothing; a repeated add and a drop still answer. A set carried across an
/// update is taken whole, past the bound included, and adds again only below it.
#[test]
fn a_session_opts_into_at_most_the_topic_bound() {
    use crate::fabric::MAX_TOPICS;
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    let sid = h.sid.clone();
    let h_ctx = h.ctx.clone();
    store.write().unwrap().register(h);
    let owner = |rest: &str| {
        dispatch_topic_verb(
            rest,
            Some(&Selector::parse(sid.as_str())),
            Scope::Owner,
            &store,
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
            let sid = h.sid.as_str().to_string();

            let (client, server) = CtlStream::pair().unwrap();
            let (store_t, active_t) = (store.clone(), active.clone());
            let line = format!("subscribe @. {streams}");
            let join = std::thread::spawn(move || {
                let mut w = server;
                run_subscribe_socket(
                    control_request::head(&line),
                    &active_t,
                    &store_t,
                    Scope::Owner,
                    &mut w,
                );
            });
            let acc = read_until(&client, String::new(), |s| s.contains("OK subscribe 1\n"));
            assert!(acc.contains("OK subscribe 1\n"), "ack: {acc:?}");

            // The ack is the barrier: the push loop
            // (`subscribe::push_loop_with_peer_probe`) seeds every watch's cursor and
            // registers before it writes the ack, so a row delivered after the ack is new.
            assert_eq!(
                watched_sessions(&store),
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
            // Each delivery's `inbox` record woke the watch: no tick is waited for.
            // Wait for what this subscription will send, not one line per delivery: a
            // filtered watch drops rows on purpose.
            let acc = read_until(&client, acc, |s| s.matches("MAIL 0 ").count() >= want);
            // A quiet mail-only subscription writes nothing, so its end is the peer
            // probe's to find: the hang-up, read on the loop's next tick.
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
/// write, or each crash holds its subscription lane for good and the cap
/// (`control_subscriptions_cap`) runs out. Run sequential crash cycles, each
/// client crashing with every frame read and nothing left to write; every
/// loop must return and deregister on its own. The request is selectorless
/// (`subscribe screen`), which names the connection's own session like every
/// selectorless verb.
#[test]
fn quiet_subscription_crashes_are_each_reaped() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);

    for cycle in 0..6 {
        let (client, server) = CtlStream::pair().expect("subscription socket pair");
        let (store_t, active_t) = (store.clone(), active.clone());
        let join = std::thread::spawn(move || {
            let mut server = server;
            run_subscribe_socket(
                control_request::head("subscribe screen"),
                &active_t,
                &store_t,
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

        // The immediate catch-up is read whole (the `screen` frame's header and every row
        // it announces), so no write is pending and the server can learn of the hangup
        // below only from the socket's read side.
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
        h.ctx.watchers.ring_watchers();
        let delta = read_until(&client, String::new(), |text| {
            text.contains(&marker) && screen_frame_read_whole(text)
        });
        assert!(
            delta.contains(&marker) && screen_frame_read_whole(&delta),
            "cycle {cycle} keeps a fully-open quiet peer subscribed: {delta:?}"
        );

        // No output or wake follows. Socket EOF is the only fact capable of
        // releasing this worker.
        hang_up(client);
        join_after_hang_up(join, &format!("quiet dead subscriber {cycle} to be reaped"));
        assert_eq!(
            watched_sessions(&store),
            0,
            "cycle {cycle} left its session's watchers"
        );
    }
}

/// AN EVENTS-ONLY SUBSCRIPTION PARKS WITH NO TIMER, AND ITS CLIENT'S HANG-UP
/// STILL ENDS IT, end to end through the production socket path. With no
/// content stream and the peer watch running (unix), a wait the loop entered
/// before the hang-up has no bound, so only the peer watch's ring wakes it;
/// a hang-up that lands before the loop's next wait is read off the verdict,
/// which bounds that wait. The quiet open peer is kept first, at a fact: an
/// event its session records is pushed to it. Where no peer watch runs, the
/// loop's timed probe ends it the same way. Which of the two ends it here is
/// a race, so each link is pinned on its own: the ring by
/// `peer_watch::tests`, the unbounded wait by
/// `an_evented_subscription_parks_without_a_timeout`, the
/// loop's parking on its peer's list by
/// `an_events_only_loop_parked_without_a_bound_wakes_on_its_peers_ring`, and
/// the list a watched stream names by
/// `a_watched_peer_names_its_list_only_while_its_stream_is_watched`.
#[test]
fn an_events_only_subscription_ends_on_its_clients_hang_up() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);
    let (client, server) = CtlStream::pair().expect("subscription socket pair");
    let (store_t, active_t) = (store.clone(), active.clone());
    let join = std::thread::spawn(move || {
        let mut server = server;
        run_subscribe_socket(
            control_request::head("subscribe @. events"),
            &active_t,
            &store_t,
            Scope::Owner,
            &mut server,
        );
    });
    let ack = read_until(&client, String::new(), |text| {
        text.contains("OK subscribe 1\n")
    });
    assert!(
        ack.contains("OK subscribe 1\n"),
        "entered push mode: {ack:?}"
    );

    crate::term_lock(&h.term).process(b"\x1b]2;kept\x07");
    h.ctx.watchers.ring_watchers();
    let title = format!("EVENT {} title kept\n", h.local_id);
    let pushed = read_until(&client, ack, |text| text.contains(&title));
    assert!(
        pushed.contains(&title),
        "a quiet open peer stays subscribed: {pushed:?}"
    );

    hang_up(client);
    join_after_hang_up(
        join,
        "the parked events loop to end on its client's hang-up",
    );
    assert_eq!(watched_sessions(&store), 0, "deregistered on return");
}

/// A WATCHED PEER NAMES ITS LIST ONLY WHILE THE WATCH HOLDS ITS STREAM.
/// While the client is `Watching`, `rings()` is the guard's list, so the push
/// loop parks on it with no timer; once the verdict settles it is `None`, and
/// the loop's wait is bounded again. `gone()` is the verdict once the hang-up
/// rang, and once a stopped watch handed the stream back (`Unwatched`) it is
/// the stream's own read-side probe: a quiet open client is kept, and its
/// hang-up, which no watch polls any more, is read off the stream.
#[cfg(unix)]
#[test]
fn a_watched_peer_names_its_list_only_while_its_stream_is_watched() {
    use crate::peer_watch::{Peer, PeerWatch};
    use crate::session_watch::Subscription;
    use crate::subscribe::PeerLiveness as _;

    let watch = PeerWatch::start().expect("the peer watch thread starts");
    let (left_server, left_client) = CtlStream::pair().expect("pair");
    let (kept_server, kept_client) = CtlStream::pair().expect("pair");
    let mut left = WatchedPeer {
        guard: watch.watch(&left_server).expect("watched"),
        stream: &left_server,
    };
    let mut kept = WatchedPeer {
        guard: watch.watch(&kept_server).expect("watched"),
        stream: &kept_server,
    };
    let names_its_list = |peer: &WatchedPeer<'_>| {
        peer.rings()
            .is_some_and(|wakes| Arc::ptr_eq(wakes, peer.guard.wakes()))
    };
    assert!(
        names_its_list(&left) && !left.gone(),
        "watching: the guard's list, and there"
    );

    let left_rang = Subscription::single(left.guard.wakes());
    hang_up(left_client);
    assert!(
        left_rang.wait(aterm_spec::wait::MUST_HAPPEN),
        "the hang-up rang"
    );
    assert_eq!(left.guard.peer(), Peer::Gone);
    assert!(left.rings().is_none(), "gone: no list");
    assert!(left.gone(), "gone: the verdict");

    assert!(names_its_list(&kept), "the quiet client is still watched");
    let kept_rang = Subscription::single(kept.guard.wakes());
    watch.fail_for_test();
    assert!(
        kept_rang.wait(aterm_spec::wait::MUST_HAPPEN),
        "the stop rang"
    );
    assert_eq!(kept.guard.peer(), Peer::Unwatched);
    assert!(
        kept.rings().is_none(),
        "unwatched: no list, so the loop keeps a timer"
    );
    assert!(
        !kept.gone(),
        "a quiet open client, asked of its stream, is kept"
    );
    hang_up(kept_client);
    assert!(kept.gone(), "and its hang-up is read off the stream");
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
/// timeout.
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
/// A wait added BESIDE the probe's one `recv` — a bounded `poll(fd, POLLIN,
/// 1)` before the peek, a 1 ms receive timeout and a `read` next to it —
/// passes the quiet-peer hang detector above, because the wait is bounded.
/// Every way a thread waits is a kernel entry (a `poll`, a timed `read`,
/// `nanosleep`, a contended lock's `ulock`/`psynch` wait, a Mach semaphore), so
/// this counts the probe's: the kernel's own per-task count of BSD syscalls
/// plus Mach traps (`proc_pidinfo(PROC_PIDTASKINFO)`) must grow by one per
/// probe of a quiet open peer. With the quiet-peer test above — that one
/// `recv` answers on a socket with nothing to bound a park — no part of the
/// probe can wait (short of a spin on the clock, which no kernel entry
/// reveals).
///
/// The count is per TASK, so the test runs alone in its own process
/// ([`crate::test_alone`]), where the only other thread is libtest's, parked
/// on its result channel. It counts once and never times: a scheduler under
/// load preempts the loop but adds no syscall to it. What else lands in the
/// count — the count's own reads, an entry of that other thread — is a few
/// entries that do not grow with `PROBES`, while a wait in the probe adds one
/// per probe, so the count is held under `PROBES + PROBES / 2`: one entry per
/// probe passes, two cannot.
///
/// NEGATIVE CONTROL, run here: the same count over the probe with a 1 ms
/// readiness `poll` in front of it reaches two per probe — the wait the hang
/// detector cannot see.
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

    let (_client, server) = CtlStream::pair().expect("probe socket pair");
    // The kernel entries over `PROBES` probes, the count's own read included.
    let over_probes = |probe: &dyn Fn() -> bool| -> u64 {
        // The first probe pays its first touches once.
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
        after - before
    };

    let probed = over_probes(&|| subscription_peer_gone(&server));
    assert!(
        (PROBES..PROBES + PROBES / 2).contains(&probed),
        "the push probe enters the kernel once, for its recv: {probed} entries over {PROBES} \
         probes"
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
    assert!(
        polled >= 2 * PROBES,
        "negative control: a readiness wait in front of the recv is a second kernel entry \
         per probe: {polled} entries over {PROBES} probes"
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
        let scope = if owner {
            Scope::Owner
        } else {
            Scope::Edge(EdgeToken::generate())
        };
        let mut out: Vec<u8> = Vec::new();
        run_subscribe(line, &active, &store, scope, &mut out);
        let s = String::from_utf8(out).unwrap();
        match want {
            Want::Exact(reply) => assert_eq!(s, reply, "{label}"),
            Want::Names(needle) => assert!(
                s.starts_with("ERR ") && s.contains(needle),
                "{label}: {s:?}"
            ),
        }
        assert_eq!(
            watched_sessions(&store),
            0,
            "{label}: a refused subscribe registers nothing"
        );
    }
    // The comma form of the space-separated request is the grammar: it parses.
    assert!(subscribe::Requested::parse("screen,cursor").is_some());
}

/// `@<sid> subscribe …` is answered through dispatch on a connection that stays
/// open, so its usage is one `ERR` line: a second line would be read as the next
/// request's reply. The serve loop's refusal, which closes the connection, adds
/// the streams after that same line.
#[test]
fn the_dispatched_subscribe_usage_is_one_line() {
    assert!(SUBSCRIBE_USAGE.starts_with("ERR usage: subscribe "));
    assert_eq!(SUBSCRIBE_USAGE.matches('\n').count(), 1);
    assert!(SUBSCRIBE_USAGE.ends_with('\n'));
    let closing = String::from_utf8(subscribe_usage_bytes()).expect("utf-8");
    let rest = closing
        .strip_prefix(SUBSCRIBE_USAGE)
        .expect("the closing refusal starts with the one-line usage");
    assert!(rest.starts_with("  <streams>: "), "{rest:?}");
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

    // A ReadScreen edge granted on this very session: the per-target gate passes.
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
        scope,
        &mut out,
    );
    assert_eq!(
        String::from_utf8_lossy(&out),
        "ERR denied\n",
        "the instance-wide sessions stream needs Owner, not a per-target grant",
    );
    assert_eq!(watched_sessions(&store), 0, "no registration on denial");
}

/// A self (`@.`) subscribe re-verifies the edge against the session active now: the
/// global ActiveHandle retargets `@.` to the frontmost tab on every switch
/// (`sync_active_session`), so a `ReadScreen` edge granted on session B must not
/// read session A after A comes to the front.
#[test]
fn subscribe_self_edge_denied_after_active_session_swings() {
    let store = session_store::new_store();
    let a = registered_session(0, -1, b"secret-of-A"); // frontmost; what `@.` resolves to
    let b = registered_session(2, -1, b""); // a different session, where the edge was granted
    store.write().unwrap().register(a.clone());
    let active = active_for(&a);

    // A ReadScreen edge granted on B's table (op matches subscribe), but no grant against
    // the now-active A.
    let tok = {
        let mut edges = b.ctx.edges.lock().unwrap();
        edges.grant(a.sid.clone(), b.sid.clone(), Op::ReadScreen, b.nonce)
    };
    let scope = Scope::Edge(tok);
    // Positive control: the edge legitimately authorizes subscribe on its own B.
    assert!(
        cross_session_authorized(scope, "subscribe", &b.ctx),
        "B's edge authorizes subscribe against its own session",
    );

    // A self subscribe while A is frontmost is denied: no partial push, no entry.
    let mut out: Vec<u8> = Vec::new();
    run_subscribe("subscribe @. screen", &active, &store, scope, &mut out);
    assert_eq!(
        String::from_utf8_lossy(&out),
        "ERR denied\n",
        "B's edge must NOT subscribe to the swung-to active session A",
    );
    assert_eq!(watched_sessions(&store), 0, "no registration on denial");
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
    let (store_t, active_t) = (store.clone(), active.clone());
    let join = std::thread::spawn(move || {
        let mut w = server;
        // `@0` is self (active), `@2` a sibling; the edge is granted on both.
        run_subscribe_socket(
            control_request::head("subscribe @0,@2 screen"),
            &active_t,
            &store_t,
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
    a.ctx.watchers.ring_watchers();
    b.ctx.watchers.ring_watchers();
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

/// The subscribe handler serves the line its lane, relay and push flip read:
/// every spelling the request grammar classes a `subscribe` (a leading
/// separator, a tab or a run of separators between its words) is acked, never
/// flipped to push and then answered with usage. Served as production serves
/// it, from the head of the request its lane parsed.
///
/// NEGATIVE CONTROL: the handler that split the line at its first space itself
/// (`split_once(' ')` had to give `("subscribe", …)`) answered usage to the
/// first two spellings, which the push flip had already taken.
#[test]
fn subscribe_reads_its_line_by_the_request_grammar() {
    for line in [
        "subscribe\t@. screen",
        "  subscribe @. screen",
        "subscribe  @.\t screen",
    ] {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let active = active_for(&h);
        let request = Request::parse(line.to_string());
        let head = request.head();
        assert!(
            head.verb == "subscribe" && head.selector.is_none(),
            "{line:?}: the push flip takes it"
        );
        let (client, server) = CtlStream::pair().unwrap();
        let (store_t, active_t) = (store.clone(), active.clone());
        let join = std::thread::spawn(move || {
            let mut w = server;
            run_subscribe_socket(request.head(), &active_t, &store_t, Scope::Owner, &mut w);
        });
        let want = format!("OK subscribe 1\nsub 0 {}\n", h.sid.as_str());
        let ack = read_until(&client, String::new(), |s| s.contains(&want));
        assert!(
            ack.starts_with(&want),
            "{line:?} subscribes its own session: {ack:?}"
        );
        hang_up(client);
        join_after_hang_up(join, "the push loop to end on its client's hang-up");
    }
}

// ── --json read mode + edges/family/feed-bin/ready verbs ─────────────────

/// A `--json` body is one well-formed JSON value, as the workspace's JSON
/// reader parses it.
fn assert_well_formed_json(s: &str) {
    if let Err(e) = aterm_json::from_str::<aterm_json::Value>(s) {
        panic!("malformed JSON ({e}) in {s:?}");
    }
}

/// The `--json` gate ([`json_ask`]), read off the table's `json` column.
///
/// * On a JSON-emitting row the `--json`/`json` flag is taken off `rest`, so the
///   emitter's own positional parse runs unchanged (`blocks 1 --json`), even
///   mid-tail.
/// * An explicit flag on a row with no JSON form (`offscreen`, `modes`, …) is an
///   ERR, never the text reply with the flag dropped.
/// * A payload verb's `json` is data: the gate never looks at it, so it reaches
///   the handler verbatim. Negative control: the same token on a JSON-emitting
///   verb is the flag.
#[test]
fn json_gate_strips_the_flag_refuses_it_and_never_scans_a_payload_verb() {
    // The router hands each class handler the request's form: unknown verbs are
    // refused before any handler asks.
    let json_ask = |verb: &str, rest: &str| {
        let form = aterm_types::control_verbs::spec_for(verb, rest).expect("a table verb");
        json_ask(&form, verb, rest)
    };
    for (verb, rest, body) in [
        ("blocks", "1 --json", "1"),
        ("blocks", "json 1", "1"),
        ("text", "--json", ""),
        ("log", "since=4 --json tier=content", "since=4 tier=content"),
        ("metrics", "percentiles json", "percentiles"),
        ("privacy", "--json", ""),
    ] {
        assert_eq!(
            json_ask(verb, rest),
            JsonAsk::Emit(body.to_string()),
            "{verb} {rest:?}"
        );
    }
    for verb in [
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
            matches!(
                json_ask(verb, "since=4 --json"),
                JsonAsk::Refuse(refusal) if refusal.starts_with("ERR ")
            ),
            "{verb} refuses an explicit json request"
        );
        assert_eq!(json_ask(verb, "since=4"), JsonAsk::No, "{verb} bare");
    }
    for (verb, rest) in [
        ("search", "json"),
        ("meta", "set title json"),
        ("send", "--json"),
        ("paste", "json"),
        ("feed", "--json"),
        ("turn", "echo json"),
        ("await", "match json"),
        ("inbox", "get json"),
    ] {
        assert_eq!(json_ask(verb, rest), JsonAsk::No, "{verb} {rest:?}");
    }
    assert_eq!(
        json_ask("text", "json"),
        JsonAsk::Emit(String::new()),
        "the token the payload verbs keep is the flag on a JSON-emitting verb"
    );
}

/// `cursor --json` / `dims --json` / `blocks --json` round-trip the same data as
/// their text forms in a well-formed JSON body; `text --json` carries the cursor.
#[test]
fn cursor_dims_blocks_json_schemas() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let reg = Arc::<crate::session_watch::Watchers>::default();
    let host = GuiHost::new(0, &term, None, &reg);
    // cursor: row/col/visible/style.
    let cj = cmd_cursor_json(&term);
    let cbody = cj.strip_prefix("OK 1\n").unwrap().trim_end();
    assert_well_formed_json(cbody);
    assert!(
        cbody.contains("\"row\":0") && cbody.contains("\"style\":\"blinking_block\""),
        "{cbody}"
    );
    let tj = cmd_text_json_opt(&term, TextArgs::default());
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
    // The dst column is always this session.
    assert!(l1.contains(ctx.self_id.as_str()), "dst is self: {l1}");
    // The secret token never appears in the listing.
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

    // No-arg form from the root's ctx (Owner): self=root, parent=-, two children.
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

// ---- session connections: the wire verbs ------------------------------------

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
    // The delivered write token is a real edge on the destination.
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

    // Re-kind pull→push: one atomic transition; `edges` shows exactly push.
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

    // Wire-granted rows with no record: the op-filtered sweep fallback.
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

    // An edge-scoped caller gets no connection rows, and the count agrees.
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

/// `feed-bin` framing: the request line's parse yields the optional selector, and
/// its row's counted body the declared length; malformed or oversize lines are
/// told apart, and the verb is recognized with or without an `@<sel>` prefix.
#[test]
fn feed_bin_framing_parse() {
    let read = |line: &str| {
        let request = Request::parse(line.to_string());
        let selector = request.head().selector.map(|s| format!("{s:?}"));
        let frame = request.frame().map(|frame| format!("{frame:?}"));
        (selector, frame, request.body_len())
    };
    let ok = |n: usize| Some(format!("{:?}", FeedBinFrame::Ok(n, None)));
    let malformed = Some(format!("{:?}", FeedBinFrame::Malformed));
    // Bare form.
    assert_eq!(read("feed-bin 4"), (None, ok(4), 4));
    // Cross-session form.
    assert_eq!(
        read("@7 feed-bin 10"),
        (Some(format!("{:?}", Selector::Local(7))), ok(10), 10)
    );
    // Not a counted frame: no header, no body.
    assert_eq!(read("feed 0a").1, None);
    assert_eq!(read("@7 feed 0a").1, None);
    // Malformed (missing / non-numeric length) -> Malformed: no payload was
    // announced, so the caller keeps the connection.
    assert_eq!(read("feed-bin"), (None, malformed.clone(), 0));
    assert_eq!(read("feed-bin xx"), (None, malformed.clone(), 0));
    // A valid but oversize length is TooLarge, not Malformed: the client pipelined
    // N bytes we refuse to read, so the caller must close rather than desync.
    assert_eq!(
        read(&format!("feed-bin {}", MAX_FEED_BIN + 1)).1,
        Some(format!("{:?}", FeedBinFrame::TooLarge))
    );
    // Exactly the cap is allowed.
    assert_eq!(
        read(&format!("feed-bin {MAX_FEED_BIN}")).1,
        ok(MAX_FEED_BIN)
    );
    // Trailing tokens after the length are Malformed: a canonical frame is exactly
    // `[@sel] feed-bin <n> [id=<key>]`, and reading `feed-bin 1 junk` as a 1-byte
    // frame would consume a byte of the next pipelined line. Trailing whitespace is
    // fine; only `feed-bin` takes the key.
    assert_eq!(read("feed-bin 1 junk").1, malformed);
    assert_eq!(read("@7 feed-bin 10 extra").1, malformed);
    assert_eq!(read("feed-bin 4   ").1, ok(4));
    assert_eq!(
        read("feed-bin 4 id=e:p:1").1,
        Some(format!("{:?}", FeedBinFrame::Ok(4, Some("e:p:1"))))
    );
    assert_eq!(read("paste-bin 4 id=e:p:1").1, malformed);
}

/// A bounded binary header is the authoritative input boundary: the target's
/// licence fence is recorded before the server can block on a slow payload, and
/// rejected or unauthorized headers never record one.
#[test]
#[cfg(unix)]
fn binary_input_header_fences_before_payload_and_only_after_authorization() {
    use crate::licence_fence::LicenceFence;

    struct FenceCheckedReader {
        inner: std::io::Cursor<Vec<u8>>,
        licence: Arc<LicenceFence>,
        before: crate::licence_fence::LicenceEpoch,
    }

    impl std::io::Read for FenceCheckedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            assert_ne!(
                self.licence.read(),
                self.before,
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
    let licence = front.ctx.licence.clone();
    let mut reader = FenceCheckedReader {
        inner: std::io::Cursor::new(b"ABCafter\n".to_vec()),
        licence: licence.clone(),
        before: licence.read(),
    };
    let mut dispatch = |_event, _session| Ok(InputOutcome::Ok);
    let mut out = Vec::new();
    assert!(feed_bin_routed(
        "@1 feed-bin 3",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
        },
        &mut dispatch,
        &mut out,
    ));
    assert_eq!(String::from_utf8_lossy(&out), "OK 3 bytes\n");
    assert_eq!(read_request_line(&mut reader).as_deref(), Some("after"));
    assert!(drain_pipe(&front_rx).is_empty());

    // A rejected frame still consumes its bounded payload, but an unauthorized
    // header cannot use the licence fence as a UI side channel.
    let before = licence.read();
    let mut reader = std::io::Cursor::new(b"XYZafter\n".to_vec());
    let mut denied = Vec::new();
    assert!(feed_bin_routed(
        "@1 feed-bin 3",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Edge(EdgeToken::generate()),
        },
        &mut dispatch,
        &mut denied,
    ));
    assert_eq!(licence.read(), before, "denied header must not touch UI");
    assert_eq!(String::from_utf8_lossy(&denied), "ERR denied\n");
    assert_eq!(read_request_line(&mut reader).as_deref(), Some("after"));

    // A targetable authorized attempt supersedes even with a malformed or
    // over-cap length, like ordinary verbs' pre-argument-parse boundary; an
    // unknown target stays side-effect free.
    let mut empty = std::io::Cursor::new(Vec::<u8>::new());
    let mut malformed = Vec::new();
    assert!(feed_bin_routed(
        "@1 feed-bin nope",
        &mut empty,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
        },
        &mut dispatch,
        &mut malformed,
    ));
    let after_malformed = licence.read();
    assert_ne!(after_malformed, before);
    let mut oversize = Vec::new();
    assert!(!feed_bin_routed(
        &format!("@1 feed-bin {}", MAX_FEED_BIN + 1),
        &mut empty,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
        },
        &mut dispatch,
        &mut oversize,
    ));
    let after_oversize = licence.read();
    assert_ne!(after_oversize, after_malformed);
    let mut missing = Vec::new();
    assert!(feed_bin_routed(
        "@999999 feed-bin nope",
        &mut empty,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
        },
        &mut dispatch,
        &mut missing,
    ));
    assert_eq!(
        licence.read(),
        after_oversize,
        "unknown target cannot fence a window"
    );
}

/// S9's typed-by stamp at a typing verb's reply: an `OK` credits the
/// connection's caller on the target, its lane's memo where one is lent (a
/// connection is read once) and a fresh read off a lane; the harness's token
/// is `aterm`; an edge is who presented its token on the session it was
/// minted for, never that session on its own word; a refusal, the halt's
/// included, credits nobody, and neither does an `id=` replay.
#[test]
#[cfg(unix)]
fn a_typing_verb_credits_its_caller_only_when_it_wrote() {
    use crate::typed_by::{Caller, Who};
    use aterm_session::Op;
    let store = session_store::new_store();
    let (front, front_rx) = crate::control_input_ack::testing::owned_handle(1);
    store.write().unwrap().register(front.clone());
    let active = active_for_handle(&front);
    let feed_bytes = |scope: Scope, payload: &[u8]| {
        let mut out = Vec::new();
        let mut dispatch =
            |event,
             target: Option<u64>,
             completion: Option<crate::control_input_ack::InputCompletion>| {
                let receipt = crate::app_input::tracked_egress(
                    &front.term,
                    &front.ctx.modes,
                    &front.ctx.sink,
                    &front.ctx.output_echo,
                    &event,
                    crate::input::EgressMode::Backpressured,
                );
                if let Some(completion) =
                    completion.filter(|c| c.matches(target.unwrap_or(1), &front.ctx))
                {
                    completion.finish(receipt);
                }
                Ok(crate::app_input::receipt_outcome(receipt))
            };
        let request = Request::parse(format!("@1 feed-bin {}", payload.len()));
        let frame = request.frame().expect("a counted frame's header");
        assert!(run_feed_bin_routed_observed(
            &request,
            frame,
            &mut std::io::Cursor::new(payload.to_vec()),
            FeedBinRoute {
                active: &active,
                store: &store,
                scope,
            },
            &mut dispatch,
            &mut out,
        ));
        String::from_utf8_lossy(&out).into_owned()
    };
    let feed = |scope| feed_bytes(scope, b"X");
    let by = || front.ctx.typed.last().map(|t| t.by.words());

    assert_eq!(feed(Scope::Edge(EdgeToken::generate())), "ERR denied\n");
    assert_eq!(by(), None, "a refusal credits nobody");

    assert!(crate::fabric::apply_hold_for_test(
        &front.ctx,
        Some(crate::fabric::Hold {
            reason: "demo".to_string(),
            origin: "local".to_string(),
        })
    ));
    assert!(feed(Scope::Owner).starts_with("ERR halted"));
    assert_eq!(by(), None, "nor does the halt");
    assert!(crate::fabric::apply_hold_for_test(&front.ctx, None));

    assert_eq!(feed_bytes(Scope::Owner, b""), "OK 0 bytes\n");
    assert_eq!(by(), None, "an ordinary zero-byte OK never credits typing");

    // Off a lane there is no peer pid to read, and nothing to keep.
    assert_eq!(feed(Scope::Owner), "OK 1 bytes\n");
    assert_eq!(by().as_deref(), Some("a script"));

    // A lane lends the connection's memo: read once, kept for the next verb.
    let mut memo = Some(Caller::new(Who::Word("lab-sweep".to_string())));
    {
        let _lent = ServingCaller::enter(&mut memo);
        assert_eq!(feed(Scope::Owner), "OK 1 bytes\n");
    }
    assert_eq!(by().as_deref(), Some("lab-sweep"));
    assert!(memo.is_some_and(|m| m.who == Who::Word("lab-sweep".to_string())));
    let mut fresh = None;
    {
        let _lent = ServingCaller::enter(&mut fresh);
        assert_eq!(feed(Scope::Owner), "OK 1 bytes\n");
    }
    assert!(
        fresh.is_some_and(|m| m.who == Who::Word("a script".to_string())),
        "the first typing verb read the connection and the lane kept it"
    );

    // The harness's token is aterm's own.
    {
        let _harness = ServingPrincipal::enter(Principal::Harness);
        assert_eq!(feed(Scope::Owner), "OK 1 bytes\n");
    }
    assert_eq!(by().as_deref(), Some("aterm"));

    // An edge minted for M: whoever presents the token, on M's edge.
    let manager = SessionId::new("s-0123456789abcdef");
    let token = front.ctx.edges.lock().unwrap().grant(
        manager.clone(),
        front.sid.clone(),
        Op::WriteInput,
        front.nonce,
    );
    assert_eq!(feed(Scope::Edge(token)), "OK 1 bytes\n");
    let credited = front.ctx.typed.last().expect("stamped").by;
    assert_eq!(
        credited.who,
        Who::Edge {
            src: manager,
            client: Box::new(Who::Word("a script".to_string())),
        },
        "the presenter, read as an owner client is (no peer pid off a lane)"
    );
    assert_eq!(credited.words(), "a script on s-0123's edge");

    assert_eq!(feed(Scope::Bridge), "OK 1 bytes\n");
    assert_eq!(by().as_deref(), Some("the message bus"));
    assert_eq!(
        drain_pipe(&front_rx),
        b"XXXXXX",
        "only nonempty actual events wrote"
    );

    // What a reply says it typed: a skip typed nothing, unless its turn had.
    assert!(typed_into("OK 3 bytes seq=9\n"));
    assert!(typed_into("OK 2\nrow one\nrow two\n"));
    assert!(!typed_into("OK skipped reason=changed\n"));
    assert!(!typed_into(
        "OK 0 turn skipped reason=changed submitted=0 seq=4 id=7\n"
    ));
    assert!(typed_into(
        "OK 0 turn skipped reason=guard submitted=0 seq=4 id=7\n"
    ));
    assert!(typed_into(
        "OK 0 turn skipped reason=person typed=5 pasted=0 submitted=0 seq=4 id=7\n"
    ));
    assert!(!typed_into(
        "OK 0 turn skipped reason=deadline typed=0 pasted=0 submitted=0 seq=4 id=7\n"
    ));
    assert!(!typed_into("ERR busy turn\n"));
    // An `id=` already written answers `dup=1` and writes nothing now.
    assert!(!typed_into("OK dup=1\n"));
    assert!(!typed_into("OK 0 dup=1\n"));
    assert!(!typed_into("OK dup=1 seq=10\n"));
    assert!(
        typed_into("OK seq=10\n"),
        "CONTROL: the same reply that typed"
    );
}

/// THE EDIT HOLD REACHES THE WIRE (amacs design §6.7): a binary frame
/// (`feed-bin`, `paste-bin`) meets the hold at the input admission (`admit`),
/// as a text verb does. While an edit ticket holds the target, both frames answer
/// `ERR busy editing ticket=<t> view=<v>`, nothing reaches the input
/// dispatch or the PTY and nobody is credited, and the payload is consumed
/// so the stream stays framed. NEGATIVE CONTROL: the same frame types once
/// the ticket's hold clears.
#[test]
#[cfg(unix)]
fn a_binary_frame_into_a_session_an_edit_holds_is_refused_busy_editing() {
    use amacs::protocol::edit::Ticket;

    use crate::control_edit::hold;

    // A session no other test mints: the hold map is process-wide.
    const SID: u64 = 1 << 47;
    let store = session_store::new_store();
    let (front, front_rx) = crate::control_input_ack::testing::owned_handle(SID);
    store.write().unwrap().register(front.clone());
    let active = active_for_handle(&front);
    let dispatched = std::cell::Cell::new(0);
    let frame = |verb: &str| {
        let mut out = Vec::new();
        let mut dispatch =
            |event,
             _target: Option<u64>,
             _completion: Option<crate::control_input_ack::InputCompletion>| {
                dispatched.set(dispatched.get() + 1);
                let receipt = crate::app_input::tracked_egress(
                    &front.term,
                    &front.ctx.modes,
                    &front.ctx.sink,
                    &front.ctx.output_echo,
                    &event,
                    crate::input::EgressMode::Backpressured,
                );
                Ok(crate::app_input::receipt_outcome(receipt))
            };
        let request = Request::parse(format!("@{SID} {verb} 1"));
        let header = request.frame().expect("a counted frame's header");
        let mut payload = std::io::Cursor::new(b"X".to_vec());
        assert!(run_feed_bin_routed_observed(
            &request,
            header,
            &mut payload,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
            &mut out,
        ));
        assert_eq!(payload.position(), 1, "{verb}: the payload is consumed");
        String::from_utf8_lossy(&out).into_owned()
    };

    hold::set(
        SID,
        hold::Hold {
            ticket: Ticket(91),
            view: 4,
        },
    );
    for verb in ["feed-bin", "paste-bin"] {
        assert_eq!(frame(verb), "ERR busy editing ticket=91 view=4\n", "{verb}");
    }
    assert_eq!(dispatched.get(), 0, "nothing reached the input dispatch");
    assert!(drain_pipe(&front_rx).is_empty(), "nothing reached the PTY");
    assert!(front.ctx.typed.last().is_none(), "nobody is credited");

    hold::clear(SID, Ticket(91));
    assert_eq!(
        frame("feed-bin"),
        "OK 1 bytes\n",
        "typed once the hold clears"
    );
    assert_eq!(dispatched.get(), 1);
    assert_eq!(drain_pipe(&front_rx), b"X");
}

/// The header fence is an early liveness boundary, not an authority cache: target
/// and capability scope are sampled again after the payload, so a stalled `@.`
/// frame follows the new active session and a revoked edge can no longer write.
/// Each session's licence counts the fences it took.
#[test]
#[cfg(unix)]
fn binary_input_revalidates_target_and_authority_after_payload() {
    // A licence that took `n` fences, none keeping the hand: what a frame's
    // fences leave on a session that took no other.
    fn fenced(n: usize) -> crate::licence_fence::LicenceEpoch {
        let licence = crate::licence_fence::LicenceFence::default();
        (0..n).for_each(|_| licence.fence(false));
        licence.read()
    }

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
    let routed = std::cell::RefCell::new(None);
    let mut dispatch = |event, session| {
        *routed.borrow_mut() = Some((event, session));
        Ok(InputOutcome::Ok)
    };
    let mut out = Vec::new();
    assert!(feed_bin_routed(
        "@. feed-bin 1",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
        },
        &mut dispatch,
        &mut out,
    ));
    // The header fenced the session active then, the effect boundary the one
    // active now.
    assert_eq!(first.ctx.licence.read(), fenced(1));
    assert_eq!(second.ctx.licence.read(), fenced(1));
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
    let mut dispatch = |_event, _session| -> Result<InputOutcome, String> {
        panic!("revoked payload reached the App input seam")
    };
    let mut denied = Vec::new();
    assert!(feed_bin_routed(
        "@3 feed-bin 1",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope,
        },
        &mut dispatch,
        &mut denied,
    ));
    assert_eq!(
        guarded.ctx.licence.read(),
        fenced(1),
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
    let mut dispatch = |_event, _session| -> Result<InputOutcome, String> {
        panic!("background explicit feed unexpectedly entered App input")
    };
    let mut accepted = Vec::new();
    assert!(feed_bin_routed(
        "@4 feed-bin 1",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: late_scope,
        },
        &mut dispatch,
        &mut accepted,
    ));
    assert_eq!(granted_late.ctx.licence.read(), fenced(1));
    assert_eq!(String::from_utf8_lossy(&accepted), "OK 1 bytes\n");
    assert_eq!(drain_pipe(&granted_late_rx), b"W");
}

/// A refused frame still consumes its body, whichever admission gate refuses it:
/// a held session's frame answers the halt although its lease would refuse it
/// too, the lease once the hold lifts, and the frame writes once neither stands.
/// Each time the next line is read as a request, never as payload, and a refused
/// body reaches no PTY. (The lease before the unread-input gate is
/// `feed_bin_gets_the_s4_ctrl_c_exemption`'s.)
#[test]
#[cfg(unix)]
fn a_refused_frame_still_consumes_its_body() {
    let store = session_store::new_store();
    let (front, front_rx) = pipe_session(1);
    let (target, target_rx) = pipe_session(2);
    store.write().unwrap().register(front.clone());
    store.write().unwrap().register(target.clone());
    let active = active_for_handle(&front);
    let mut dispatch = |_event, _session| -> Result<InputOutcome, String> {
        panic!("a frame into a background session never takes the seam")
    };
    let mut frame = |want: &str| {
        let mut reader = std::io::Cursor::new(b"ABCafter\n".to_vec());
        let mut out = Vec::new();
        assert!(feed_bin_routed(
            "@2 feed-bin 3",
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
            &mut out,
        ));
        assert_eq!(String::from_utf8_lossy(&out), want);
        assert_eq!(read_request_line(&mut reader).as_deref(), Some("after"));
    };
    assert!(crate::fabric::apply_hold_for_test(
        &target.ctx,
        Some(crate::fabric::Hold {
            reason: "demo".to_string(),
            origin: "local".to_string(),
        })
    ));
    *target.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
        id: 7,
        driver: None,
        typing: false,
    });
    frame("ERR halted reason=demo origin=local\n");
    assert!(crate::fabric::apply_hold_for_test(&target.ctx, None));
    frame("ERR busy turn=7\n");
    assert!(
        drain_pipe(&target_rx).is_empty(),
        "no refused body was written"
    );
    *target.ctx.turn_lease.lock().unwrap() = None;
    frame("OK 3 bytes\n");
    assert_eq!(drain_pipe(&target_rx), b"ABC");
    assert!(drain_pipe(&front_rx).is_empty());
}

/// `feed-bin` meets the unread-input gate with its own body, so it gets the S4
/// exemption `feed 03` has: a lone `^C` frame into a cbreak program (ISIG on) whose
/// stall is published is a signal, not input behind the unread `q`, and is written
/// (the line discipline flushes the `q`). Before, the frame asked the gate with no
/// body and was refused. Negative control: a frame of one byte that is not `^C` is
/// refused `ERR busy input-unread`, its body consumed and nothing written behind the
/// `q`.
///
/// A frame meets the admission in the text verbs' order: with a turn's lease on
/// the same stalled session, the `x` frame answers the lease (`ERR busy turn=`),
/// which comes before the unread-input gate. The frame path's own order (halt,
/// gate, lease, floor) answered `ERR busy input-unread` here.
#[test]
#[cfg(target_os = "macos")]
fn feed_bin_gets_the_s4_ctrl_c_exemption() {
    use crate::input_stall::tests::{publish_stall, raw_pty_pair, slave_read_all};
    let (master, slave) = raw_pty_pair();
    // SAFETY: all-zeros is a valid termios, overwritten by `tcgetattr`.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `slave` is this test's live pty slave; `t` is an out-param.
    assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
    t.c_lflag |= libc::ISIG;
    // SAFETY: `slave` is live; `t` is derived from its own termios.
    assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
    let target = registered_session(9, master, b"");
    target.ctx.sink.note_master_nonblocking(true);
    target
        .ctx
        .sink
        .write_frame_nonparking(b"q")
        .expect("the person's key");
    publish_stall(&target.ctx);
    let store = session_store::new_store();
    let (front, _front_rx) = pipe_session(1);
    store.write().unwrap().register(front.clone());
    store.write().unwrap().register(target.clone());
    let active = active_for_handle(&front);
    let mut dispatch = |_event, _session| -> Result<InputOutcome, String> {
        panic!("a frame into a background session never takes the seam")
    };
    let mut frame = |body: &[u8]| {
        let mut input = body.to_vec();
        input.extend_from_slice(b"after\n");
        let mut reader = std::io::Cursor::new(input);
        let mut out = Vec::new();
        assert!(feed_bin_routed(
            "@9 feed-bin 1",
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
            &mut out,
        ));
        assert_eq!(read_request_line(&mut reader).as_deref(), Some("after"));
        String::from_utf8_lossy(&out).into_owned()
    };

    let refused = frame(b"x");
    assert!(refused.starts_with("ERR busy input-unread"), "{refused}");
    *target.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
        id: 7,
        driver: None,
        typing: false,
    });
    assert_eq!(
        frame(b"x"),
        "ERR busy turn=7\n",
        "the lease before the gate"
    );
    *target.ctx.turn_lease.lock().unwrap() = None;
    assert_eq!(slave_read_all(slave), b"q", "nothing queued behind the `q`");
    target
        .ctx
        .sink
        .write_frame_nonparking(b"q")
        .expect("the person's key again");
    publish_stall(&target.ctx);
    assert_eq!(frame(b"\x03"), "OK 1 bytes\n");
    assert_eq!(
        target.ctx.sink.input_backlog().map(|b| b.queued),
        Some(0),
        "the ^C was a signal and flushed the `q`: nothing was read behind it"
    );
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);
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
            line: None,
            frame: Frame::Unchecked,
            keep: true,
            reply: Reply::Prefix("ERR usage"),
            pty: None,
            next: Some("following"),
        },
        // A frame's header takes a length and, on `feed-bin`, a key; `unread=ok` is
        // the text verbs' leading option, so here it is a malformed header and no
        // payload is read.
        Row {
            label: "a frame takes no unread=ok",
            input: b"feed-bin 3 unread=ok\nfollowing\n".to_vec(),
            line: None,
            frame: Frame::Unchecked,
            keep: true,
            reply: Reply::Prefix("ERR usage"),
            pty: Some(b""),
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
        let request = Request::parse(line.clone());
        match row.frame {
            Frame::Any => assert!(request.frame().is_some(), "{}", row.label),
            Frame::Is(verb) => {
                assert!(request.frame().is_some(), "{}", row.label);
                assert_eq!(request.head().verb, verb, "{}", row.label);
            }
            Frame::Unchecked => {}
        }
        let keep = run_feed_bin(&line, &mut reader, &active, &store, Scope::Owner, &mut out);
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
    assert!(feed_bin_routed(
        &line,
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
        },
        &mut dispatch,
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
    assert!(feed_bin_routed(
        &line,
        &mut reader,
        FeedBinRoute {
            active: &no_terminal,
            store: &session_store::new_store(),
            scope: Scope::Owner,
        },
        &mut dispatch,
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
    // The background peer must stay gone even if a concurrent test's child inherited the
    // read end ([`cloexec_pipe`]), so it is killed in the kernel.
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
        assert!(feed_bin_routed(
            &line,
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
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
    // Dropping this process's reader is not enough ([`kill_peer_in_kernel`]).
    drop(rx);
    kill_peer_in_kernel(h.master, "a cross-session send into a dead peer");
    assert_eq!(
        background(
            "send",
            &h.term,
            &h.ctx,
            InputEvent::KeySequence(send_bytes("ls"))
        ),
        Ok(InputOutcome::WriteFailed),
        "raw send into a dead peer is a failed write"
    );
    assert_eq!(
        background("key", &h.term, &h.ctx, parse_key("enter").unwrap()),
        Ok(InputOutcome::WriteFailed),
        "a key into a dead peer is a failed write, not OK"
    );
}

/// BACKGROUND INPUT ON THE ORDERED WRITER'S ROUTE ANSWERS FROM THE WRITE, NOT ITS
/// ADMISSION (ATERM-KI-1014-6): on the route every Windows key, report and paste
/// takes (`force_conpty_route_for_test`), a background `send`, `key`, `paste` and
/// mouse report into a session whose peer is gone answer `ERR write failed`.
/// Before, they were answered on the writer's acceptance: `OK`. CONTROL: into a
/// live reader the same input answers `OK` and arrives. The lanes wait for the
/// write's answer as long as a hang detector (`queued_input_waits_for_test`), not
/// the 100 ms a request lane gives it, so a slow writer on a loaded host cannot
/// pass as "queued, accepted".
#[test]
#[cfg(unix)]
fn queued_background_input_into_a_dead_peer_is_a_failed_write() {
    let _route = crate::app_input::paste_order::force_conpty_route_for_test();
    let _wait = super::queued_input_waits_for_test(aterm_spec::wait::MUST_HAPPEN);
    let (live, live_rx) = pipe_session(75);
    assert_eq!(
        background(
            "send",
            &live.term,
            &live.ctx,
            InputEvent::KeySequence(send_bytes("ls"))
        ),
        Ok(InputOutcome::Ok),
        "CONTROL: a send into a live reader"
    );
    assert_eq!(
        background("key", &live.term, &live.ctx, parse_key("enter").unwrap()),
        Ok(InputOutcome::Ok)
    );
    assert_eq!(
        background_input(
            &live.term,
            &live.ctx,
            &InputEvent::Paste("pasted".into(), PasteFraming::AtDrain),
            aterm_spec::wait::MUST_HAPPEN,
        ),
        Ok(InputOutcome::Ok)
    );
    let mut arrived = Vec::new();
    let all = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(1),
        || {
            arrived.extend(drain_pipe(&live_rx));
            arrived.ends_with(b"pasted").then_some(())
        },
    );
    assert!(all.is_some(), "the input arrives: {arrived:?}");
    assert!(arrived.starts_with(b"ls\r"), "{arrived:?}");

    let (dead, dead_rx) = pipe_session(76);
    term_lock(&dead.term).process(b"\x1b[?1000h\x1b[?1006h");
    drop(dead_rx);
    kill_peer_in_kernel(dead.master, "a queued background write into a dead peer");
    assert_eq!(
        background(
            "send",
            &dead.term,
            &dead.ctx,
            InputEvent::KeySequence(send_bytes("ls"))
        ),
        Ok(InputOutcome::WriteFailed),
        "a queued send into a dead peer"
    );
    assert_eq!(
        background("key", &dead.term, &dead.ctx, parse_key("enter").unwrap()),
        Ok(InputOutcome::WriteFailed),
        "a queued key into a dead peer"
    );
    assert_eq!(
        background_input(
            &dead.term,
            &dead.ctx,
            &InputEvent::Paste("pasted".into(), PasteFraming::AtDrain),
            aterm_spec::wait::MUST_HAPPEN,
        ),
        Ok(InputOutcome::WriteFailed),
        "a queued paste into a dead peer"
    );
    assert_eq!(
        cross_mouse_apply(&dead.term, &dead.ctx, "press left 2 4"),
        Err("ERR write failed\n".to_string()),
        "a queued mouse report into a dead peer"
    );
}

/// A program on a real ConPTY, registered as a background tab's session, its
/// output read off on a thread of its own as the App's reader reads it.
#[cfg(windows)]
struct ConptyTab {
    session: SessionHandle,
    pid: i32,
    master: i32,
    output: std::sync::mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
}

#[cfg(windows)]
impl ConptyTab {
    fn spawn(local_id: u64, argv: &[&str]) -> Self {
        // SAFETY: a test process; the trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let exec: Vec<std::ffi::OsString> = argv.iter().map(|a| (*a).into()).collect();
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&exec),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("a ConPTY spawn");
        let master = shell.master;
        let (tx, output) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n = aterm_pty::read(master, &mut buf);
                let Some(chunk) = usize::try_from(n).ok().filter(|n| *n > 0) else {
                    return;
                };
                if tx.send(buf[..chunk].to_vec()).is_err() {
                    return;
                }
            }
        });
        Self {
            session: registered_session(local_id, master, b""),
            pid: shell.pid,
            master,
            output,
            seen: Vec::new(),
        }
    }

    /// The text conhost drew so far, its CSI and OSC framing left out.
    fn text(&self) -> String {
        let (mut text, mut i, s) = (Vec::new(), 0, &self.seen);
        while i < s.len() {
            match (s[i], s.get(i + 1)) {
                (0x1b, Some(b'[')) => {
                    i += 2;
                    while i < s.len() && !(0x40..=0x7e).contains(&s[i]) {
                        i += 1;
                    }
                }
                (0x1b, Some(b']')) => {
                    i += 2;
                    while i < s.len() && !matches!(s[i], 0x07 | 0x1b) {
                        i += 1;
                    }
                    if s.get(i) == Some(&0x1b) {
                        i += 1;
                    }
                }
                (b, _) => text.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&text).into_owned()
    }

    /// Read until the drawn text holds `marker` (a hang detector, never a budget).
    fn await_text(&mut self, marker: &str) {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !self.text().contains(marker) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.output.recv_timeout(left) {
                Ok(chunk) => self.seen.extend_from_slice(&chunk),
                Err(e) => panic!("no {marker:?} ({e}); drawn: {:?}", self.text()),
            }
        }
    }

    /// Read to the end of the program's output: its console host is gone.
    fn await_end(&mut self) {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.output.recv_timeout(left) {
                Ok(chunk) => self.seen.extend_from_slice(&chunk),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Err(e) => panic!("the output did not end ({e}): {:?}", self.text()),
            }
        }
    }
}

#[cfg(windows)]
impl Drop for ConptyTab {
    fn drop(&mut self) {
        aterm_pty::hangup(self.pid);
        aterm_pty::reap(self.pid);
        aterm_pty::close_master(self.master);
    }
}

/// [`queued_background_input_into_a_dead_peer_is_a_failed_write`] on a real ConPTY
/// (ATERM-KI-1014-6), where every background input takes the ordered writer's route
/// and the write is ConPTY's `WriteFile`: a background `send`, `key`, `paste` and
/// mouse report into a tab whose program and console host are gone answer
/// `ERR write failed`. CONTROL: into a live cmd.exe the same verbs answer `OK` and the
/// program runs what they typed, printing results neither its banner nor its prompt
/// holds (the banner's `10.0.26300.9457` holds `45`).
#[test]
#[cfg(windows)]
fn queued_background_input_into_a_dead_conpty_peer_is_a_failed_write() {
    let _wait = super::queued_input_waits_for_test(aterm_spec::wait::MUST_HAPPEN);
    let mut live = ConptyTab::spawn(77, &["cmd.exe", "/d"]);
    live.await_text(">");
    let (term, ctx) = (&live.session.term, &live.session.ctx);
    let typed = InputEvent::KeySequence(send_bytes("set /a 6007*7"));
    assert_eq!(background("send", term, ctx, typed), Ok(InputOutcome::Ok));
    let enter = || parse_key("enter").unwrap();
    assert_eq!(background("key", term, ctx, enter()), Ok(InputOutcome::Ok));
    live.await_text("42049");
    let (term, ctx) = (&live.session.term, &live.session.ctx);
    assert_eq!(
        background_input(
            term,
            ctx,
            &InputEvent::Paste("set /a 5009*9".into(), PasteFraming::AtDrain),
            aterm_spec::wait::MUST_HAPPEN,
        ),
        Ok(InputOutcome::Ok),
        "CONTROL: a paste into a live program"
    );
    assert_eq!(background("key", term, ctx, enter()), Ok(InputOutcome::Ok));
    live.await_text("45081");

    let mut dead = ConptyTab::spawn(78, &["cmd.exe", "/d", "/c", "exit 0"]);
    dead.await_end();
    let probe = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(10),
        || aterm_pty::write_some(dead.master, b"x").err(),
    );
    assert!(
        probe.is_some(),
        "the console host's input pipe must be observably gone before any verdict"
    );
    let (term, ctx) = (&dead.session.term, &dead.session.ctx);
    term_lock(term).process(b"\x1b[?1000h\x1b[?1006h");
    let typed = InputEvent::KeySequence(send_bytes("set /a 6007*7"));
    assert_eq!(
        background("send", term, ctx, typed),
        Ok(InputOutcome::WriteFailed),
        "a send into a dead ConPTY peer ({probe:?})"
    );
    assert_eq!(
        background("key", term, ctx, enter()),
        Ok(InputOutcome::WriteFailed),
        "a key into a dead ConPTY peer"
    );
    assert_eq!(
        background_input(
            term,
            ctx,
            &InputEvent::Paste("pasted".into(), PasteFraming::AtDrain),
            aterm_spec::wait::MUST_HAPPEN,
        ),
        Ok(InputOutcome::WriteFailed),
        "a paste into a dead ConPTY peer"
    );
    assert_eq!(
        cross_mouse_apply(term, ctx, "press left 2 4"),
        Err("ERR write failed\n".to_string()),
        "a mouse report into a dead ConPTY peer"
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
    let reply = control_input::input_reply_to_str(background(
        "paste",
        &handle.term,
        &handle.ctx,
        InputEvent::Paste("newer".into(), PasteFraming::AtDrain),
    ));
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
/// older spilled frame, the background route keeps the sink's sticky failure and returns
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

/// Control input into a background session whose program reads nothing never parks the lane
/// that serves it, or `CONTROL_WORKERS` pastes into one stuck tab would take every request
/// lane and leave every other client, `@<sid> signal` (the remedy) included, answered busy.
/// With the pipe full: `send` and `key` are queued and answered at once; a paste waits for
/// its bytes to reach the program only up to its bound, then answers that they are queued
/// unread; at the session's input cap a write is refused with nothing written, and the reply
/// starts `ERR busy`.
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
    let sent = answered_without_parking("send", move || {
        background("send", &t, &c, InputEvent::KeySequence(send_bytes("ls")))
    });
    assert_eq!(sent, Ok(InputOutcome::Ok), "accepted for delivery");
    assert!(
        ctx.sink.spilled_bytes() >= 2,
        "the bytes wait in the session's spill, for the program to read"
    );
    let (t, c) = (Arc::clone(&term), Arc::clone(&ctx));
    let key = answered_without_parking("key", move || {
        control_input::input_reply_to_str(background("key", &t, &c, parse_key("enter").unwrap()))
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
        control_input::input_reply_to_str(background("key", &t, &c, parse_key("enter").unwrap()))
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

/// A background paste's `OK` is a kernel receipt, so one whose queued bytes a discard drops
/// while it waits (`signal`'s, `reset flush`) answers `ERR write failed` (in doubt), never
/// `OK`, though the drain fence reads the emptied spill as drained. On a raw pty nobody reads
/// (a discard measures the tty's queue: macOS only), whose two ends the test leaks, as
/// [`pipe_session`] leaks its write end.
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

/// ONE ANSWER FOR A FULL INPUT QUEUE: a key into the front tab whose session is
/// at its input cap is refused `ERR busy input-queue-full`, nothing
/// written, the reply a background tab's lane gives for the same refusal. It
/// was `ERR write failed` in front (in doubt, so a driver could not retry) and
/// busy behind (retryable as is). Driven through the real App input seam
/// ([`crate::App::input`]) on the headless App's session 0, and the background
/// route ([`InputRoute::Background`]) on the same session.
#[test]
#[cfg(unix)]
fn a_full_input_queue_answers_busy_in_the_front_tab_as_in_a_background_one() {
    let (rd, wr) = cloexec_pipe();
    // SAFETY: `rd` is a fresh pipe end this test owns.
    let reader = unsafe { std::fs::File::from_raw_fd(rd) };
    aterm_pty::set_nonblocking(wr, true).expect("nonblocking test master");
    let mut app = crate::App::headless_for_test_with_sink(Arc::new(SinkWriter::new(wr)));
    let (term, ctx) = {
        let session = &app.pool.get(0).expect("session 0");
        (Arc::clone(&session.term), Arc::clone(&session.ctx))
    };
    ctx.sink.note_master_nonblocking(true);
    loop {
        match aterm_pty::write_some(wr, &[b'.'; 4096]) {
            Ok(n) if n > 0 => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected pipe fill result: {other:?}"),
        }
    }
    let key = || parse_key("enter").expect("enter parses");
    assert_eq!(
        app.input(crate::WindowId(0), key(), crate::input::Source::Controller),
        InputOutcome::Ok,
        "CONTROL: below the cap the front tab's key is accepted"
    );
    let below = SinkWriter::SPILL_CAP - ctx.sink.spilled_bytes();
    let accepted = ctx
        .sink
        .write_frame_interactive_with_receipt(&vec![b'x'; below])
        .expect("the frame that reaches the cap is accepted")
        .accepted();
    assert_eq!(accepted, below);
    let spilled = ctx.sink.spilled_bytes();

    let front = app.input(crate::WindowId(0), key(), crate::input::Source::Controller);
    assert_eq!(front, InputOutcome::QueueFull);
    assert_eq!(
        control_input::input_reply_to_str(Ok(front)),
        INPUT_QUEUE_FULL
    );
    assert_eq!(
        control_input::input_reply_to_str(background("key", &term, &ctx, key())),
        INPUT_QUEUE_FULL,
        "the background lane's answer for the same refusal"
    );
    assert_eq!(
        ctx.sink.spilled_bytes(),
        spilled,
        "a refused write accepts nothing"
    );
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
        stamp_input_seq(input_of("send"), "OK\n".to_string(), &term),
        format!("OK seq={seq}\n")
    );
    assert_eq!(
        stamp_input_seq(input_of("feed"), "OK 3 bytes\n".to_string(), &term),
        format!("OK 3 bytes seq={seq}\n")
    );
    // A non-input verb is not stamped.
    assert_eq!(
        stamp_input_seq(input_of("text"), "OK\n".to_string(), &term),
        "OK\n".to_string()
    );
    // An ERR reply is not stamped (no baseline to correlate).
    assert_eq!(
        stamp_input_seq(input_of("send"), "ERR no window\n".to_string(), &term),
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
    // Session A is frontmost: what `@.`/self resolves to via the active handle.
    let (h_a, a_rx) = pipe_session(1);
    // Session B, a different session, is where the edge was legitimately granted.
    let (h_b, _b_rx) = pipe_session(2);
    store.write().unwrap().register(h_a.clone());
    let active = active_for_handle(&h_a);

    // A WriteInput edge granted on B's table: the op matches feed-bin, but it holds no
    // grant against the now-active A. (Scope is Copy, reused for both asserts.)
    let edge_b = edge_granted(Op::WriteInput, &h_b.ctx);
    // Positive control: the same edge legitimately drives its own session B.
    assert!(
        cross_session_authorized(edge_b, "feed", &h_b.ctx),
        "B's edge authorizes feed-bin against its own session",
    );

    let input: Vec<u8> = b"feed-bin 3\nXYZafter\n".to_vec();
    let mut reader = BufReader::new(std::io::Cursor::new(input));
    let mut out: Vec<u8> = Vec::new();
    let line = read_request_line(&mut reader).unwrap();
    let keep = run_feed_bin(&line, &mut reader, &active, &store, edge_b, &mut out);

    assert!(keep, "denied frame keeps the connection");
    assert_eq!(
        String::from_utf8_lossy(&out),
        "ERR denied\n",
        "B's edge must NOT feed-bin the swung-to active session A",
    );
    assert!(drain_pipe(&a_rx).is_empty(), "nothing reached A's pty");
    // The denial still consumed the N payload bytes, so the stream stays framed.
    let next = read_request_line(&mut reader).unwrap();
    assert_eq!(
        next, "after",
        "denial still consumes the payload (stream framed)"
    );
}

/// A line one byte past [`MAX_REQUEST_LINE`] is answered why before the drop
/// (`None`), so the peer reads a reason, not a bare EOF; a line exactly at the
/// cap is read, and owes no answer.
#[test]
fn an_over_long_request_line_is_answered_before_the_drop() {
    let at_cap = "a".repeat(MAX_REQUEST_LINE);
    let mut input = format!("{at_cap}\n").into_bytes();
    input.extend(std::iter::repeat_n(b'b', MAX_REQUEST_LINE + 1));
    input.extend_from_slice(b"\nversion\n");
    let mut reader = BufReader::new(std::io::Cursor::new(input));
    let mut refusal = Vec::new();
    let line = read_request_line_with_policy(&mut reader, &mut refusal, false, || Ok(()));
    assert_eq!(line.as_deref(), Some(at_cap.as_str()));
    assert!(refusal.is_empty(), "a line at the cap is no refusal");
    let line = read_request_line_with_policy(&mut reader, &mut refusal, false, || Ok(()));
    assert_eq!(line, None, "the over-long line closes the connection");
    assert_eq!(
        String::from_utf8(refusal).unwrap(),
        "ERR request line too long (max 65536 bytes)\n"
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
    let line = read_request_line_with_policy(&mut reader, &mut std::io::sink(), false, || {
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

/// AN EXIT ALONE ENDS A PARKED `await`: the shell's `set_state(Exited)` records
/// the transition, the record rings the session's watchers, and the await
/// reads `Exited` and answers. The exit path sends no wake of its own, so this
/// is the only road. The wait is a hang detector: the await asks ten minutes.
#[test]
fn exit_wakes_a_parked_await() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let waiter = {
        let (store, h) = (store.clone(), h.clone());
        std::thread::spawn(move || {
            cmd_await(
                &h.term,
                &store,
                0,
                &h.ctx,
                "match never-on-this-screen-xyzzy timeout=600000",
            )
        })
    };
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the await parks", || {
        (h.ctx.watchers.count() == 1).then_some(())
    });
    store
        .write()
        .unwrap()
        .set_state(0, session_store::SessionState::Exited);
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the await answers", || {
        waiter.is_finished().then_some(())
    });
    assert_eq!(waiter.join().unwrap(), "ERR exited\n");
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
        cmd_ready(term, &store, 0, &h.ctx, "2000"),
        "OK ready prompt\n",
        "fresh prompt is ready"
    );

    // An executing command -> not ready -> a short timeout returns `OK timeout`.
    term.lock()
        .unwrap()
        .process(b"\x1b]133;B\x07sleep\n\x1b]133;C\x07");
    assert_eq!(
        cmd_ready(term, &store, 0, &h.ctx, "0"),
        "OK timeout\n",
        "executing is not ready"
    );

    // The command completes -> Complete -> ready again (prompt-end).
    term.lock().unwrap().process(b"\x1b]133;D;0\x07");
    assert_eq!(
        cmd_ready(term, &store, 0, &h.ctx, "2000"),
        "OK ready prompt\n",
        "completed is ready"
    );

    // A session marked Exited never becomes ready -> ERR exited (fail closed).
    store
        .write()
        .unwrap()
        .set_state(0, session_store::SessionState::Exited);
    assert_eq!(
        cmd_ready(term, &store, 0, &h.ctx, "2000"),
        "ERR exited\n",
        "exited fails closed"
    );
}

/// The wait family reads `<ms>` one way ([`aterm_control::wire::wait_ms`]): a whole number
/// of milliseconds, or the verb's usage line. Covers `ready`, `await` and `await momentum`.
#[test]
fn the_wait_family_refuses_an_ms_that_is_not_one() {
    use std::time::Instant;

    use control_session::cmd_await_momentum;

    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    // A fresh prompt: `ready` and `await gone` both answer at once when asked well.
    crate::term_lock(&h.term).process(b"\x1b]133;A\x07$ ");
    let ready = |tail: &str| cmd_ready(&h.term, &store, 0, &h.ctx, tail);
    let await_ = |tail: &str| cmd_await(&h.term, &store, 0, &h.ctx, tail);
    let dark = || Ok((0.0, Instant::now()));
    let momentum = |tail: &str| cmd_await_momentum(&h.term, &store, 0, &h.ctx, tail, &dark);

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
        cmd_ready(&h.term, &store, 0, &h.ctx, "2000",),
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
    // Parity: the client hashes what `text --json` served.
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
    // The spinner row repaints: outside the range.
    term.lock().unwrap().process(b"\x1b[1;1Htick 2\x1b[3;3H");
    // Negative control: the whole-screen fence refuses that repaint.
    let out = cmd_turn(
        term,
        &store,
        0,
        &format!("if-gen={judged} idle=50 timeout=5000 hello peer"),
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
    // A change inside the range (a keystroke in the box): refused.
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
            &h.ctx,
            &io,
        );
        assert!(out.starts_with("ERR usage: turn"), "{bad}: {out}");
    }
    assert_eq!(pasted.borrow().len(), 1, "a bad fence types nothing");
}

/// A headless window wearing the rainbow kitty, ticked once so its engine is live. Its
/// session writes into a real pipe (keep the returned read end): a key whose write fails is
/// revoked at the seam, so only keys that reached the wire lay ribbon.
//
// Unix only: on Windows `SinkWriter::new`'s `i32` is a ConPTY registry key, and the
// `try_write_frame_immediate` family is fail-closed there (`ImmediateWrite::BusyZero`).
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
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                key: &key,
                momentum: &momentum,
                driver: None,
                by: crate::turn_ledger::TurnBy::Ctl,
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
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            key: &key,
            momentum: &momentum,
            driver: None,
            by: crate::turn_ledger::TurnBy::Ctl,
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
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                key: &key,
                momentum: &momentum,
                driver: None,
                by: crate::turn_ledger::TurnBy::Ctl,
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
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            key: &key,
            momentum: &momentum,
            driver: None,
            by: crate::turn_ledger::TurnBy::Ctl,
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
/// `❯ <draft>` wrapping every `wrap` graphemes, bottom rule, footer and rows under it, drawn
/// from row `at` as one finished DEC-2026 frame per key. On a `full` screen a new draft row
/// moves the top rule up. `lag` delays the next echo, `hold` holds it until the following
/// key is out, `later` writes bytes after a delay on a thread, and `fence` names other rows
/// than the composer's three.
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
/// A turn returns at its verdict, so for a verdict that must come (every key typed,
/// `submitted=1`, a stop at a fact) the `timeout=` is a hang detector (60000 ms). A short
/// timeout appears only where `reason=deadline` is the subject.
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
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                key: &key,
                momentum: &momentum,
                driver: None,
                by: crate::turn_ledger::TurnBy::Ctl,
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

/// A box that takes the composer's place mid-notice stops typing before its digits (a
/// subagent's box can mount without moving the person count or epoch, and the digits would
/// answer it): a turn fenced `if-rows=` on the composer stops at `reason=changed typed=28`,
/// no digit and no Enter, for a labelled and an all-`─` top rule. Negative control: an
/// AskUserQuestion box between all-`─` borders matches both rules, and its caret-less row
/// stops it.
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

/// The caret anchor follows everything a composer's screen does while typed into (footer
/// edits, rows above and below, wrapping both ways, a survey drawn over the top rule, a full
/// screen), in one turn that types every key and submits.
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

/// A torn frame is never judged: while the program's DEC-2026 frame is open with writes in
/// it the cursor is its write position, not its caret, so the key waits for the frame to
/// finish, and a frame never finished stops the typing at the deadline (`reason=deadline
/// typed=0`, no Enter). The frame is torn before the first key, so no thread stall can change
/// the verdict. Negative controls: the same write in a closed bracket is judged
/// (`reason=changed typed=3`); a torn frame the program finishes 100 ms later is judged then
/// and the typing goes on at its cadence (the waiting key anchors the schedule, so no burst
/// follows); and a cursor off the frame when the fence is read is judged at the first key
/// (`reason=changed typed=0`).
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

/// A digit waits for the previous key to echo (a box's first frame reaches the grid a
/// render after it mounts), and digits whose predecessors echo at once are not slowed.
/// Negative control: a letter after a held echo goes out on schedule.
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

/// A digit waits for the program to read every key before it: the caret-moved check alone
/// can be satisfied by an older key's echo while later keys sit unread, keys a box mounting
/// then would take. On a real pty a line written to the master stands unread until read
/// ([`aterm_session::sink::SinkWriter::input_backlog`]): left unread after key 8, the text
/// stops at its deadline with no digit and no Enter, `typed=8`; read meanwhile, the digit goes
/// out, the text is submitted, and no digit ever goes out over an unread queue. Negative
/// control: a text of letters types every key past the same unread line.
///
/// The program is played at the hold's wait ([`super::control_session::set_hold_wait_for_test`]):
/// the deadline comes there for the line never read, and the line is read there otherwise.
///
/// macOS only: elsewhere a tty cannot count its unread input (`aterm_pty::input_queue_len`
/// answers `None`), and the digit keeps the caret check alone (`control_session::keys_unread`).
#[cfg(target_os = "macos")]
#[test]
fn a_digit_waits_for_the_program_to_read_every_key_before_it() {
    use super::control_session::set_hold_wait_for_test;
    use aterm_session::sink::Discard;
    use std::time::Instant;
    /// The thread's holds wait on the clock again however the test ends.
    struct Clock;
    impl Drop for Clock {
        fn drop(&mut self) {
            set_hold_wait_for_test(None);
        }
    }
    let _clock = Clock;
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty pair");
    // The turn's budget, a hang detector for the turns that must finish.
    let budget = aterm_spec::wait::MUST_HAPPEN;
    let opts = format!("timeout={}", budget.as_millis());
    let digits_over_unread = std::cell::Cell::new(0usize);
    let waits = std::rc::Rc::new(std::cell::Cell::new(0usize));
    // At key `at` the program is sent a line; at each wait of the hold it `reads` it, or the
    // wait finds the turn's budget spent.
    let unread_after = |at: usize, reads: bool| {
        let digits_over_unread = &digits_over_unread;
        let waits = std::rc::Rc::clone(&waits);
        move |k: usize, c: &Composer, h: &SessionHandle| {
            if k == at {
                aterm_pty::write_all(master, b"x\n");
                let waits = std::rc::Rc::clone(&waits);
                let sink = h.ctx.sink.clone();
                set_hold_wait_for_test(Some(Box::new(move || {
                    waits.set(waits.get() + 1);
                    if reads {
                        let _ = sink.discard_unread_input(Discard::Flush);
                        Instant::now()
                    } else {
                        Instant::now() + budget
                    }
                })));
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
    // Never read: the digit's wait finds the turn's budget spent.
    let t = composer_turn_on(master, &rule, 76, &opts, text, &unread_after(8, false));
    set_hold_wait_for_test(None);
    assert!(
        t.verdict.contains("skipped reason=deadline typed=8 "),
        "{}",
        t.verdict
    );
    assert_eq!(
        t.keys.concat(),
        "version ",
        "no digit while the line stands unread"
    );
    assert_eq!(t.enters, 0);
    assert_eq!(
        waits.get(),
        1,
        "the digit waited, and its deadline came there"
    );
    // Negative control: letters do not wait on it.
    waits.set(0);
    let letters = "version is in";
    let t = composer_turn_on(master, &rule, 76, &opts, letters, &unread_after(3, false));
    set_hold_wait_for_test(None);
    assert!(t.verdict.contains(" submitted=1 "), "{}", t.verdict);
    assert_eq!(t.keys.concat(), letters);
    assert_eq!(waits.get(), 0, "no key of letters waited");
    // Read meanwhile: the digit goes out, the text is whole — and no
    // digit went out while the line was still unread.
    let t = composer_turn_on(master, &rule, 76, &opts, text, &unread_after(8, true));
    set_hold_wait_for_test(None);
    assert!(t.verdict.contains(" submitted=1 "), "{}", t.verdict);
    assert_eq!(t.keys.concat(), text);
    assert!(waits.get() >= 1, "the digit waited for the line to be read");
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
    // The schedule fits (11 ms), the route does not: 60 ms a key, so eleven keys cannot
    // fit 300 ms. Where the cut falls is the clock's; the subject is what follows it: no
    // Enter, and the verdict names the keys typed.
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
/// at 0.31 and a floor of 0.30 the release law puts the crossing at
/// `2·ln(0.31/0.30)` = 66 ms, and the verb takes exactly two readings (the one it
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
        Ok((if reads.get() == 1 { 0.31 } else { 0.10 }, Instant::now()))
    };
    let t0 = Instant::now();
    let out = cmd_await_momentum(
        &h.term,
        &store,
        0,
        &h.ctx,
        "momentum 0.30 timeout 60000",
        &read,
    );
    let waited = t0.elapsed();
    assert_eq!(out, "OK momentum 0.10\n");
    assert_eq!(reads.get(), 2, "solve, then confirm — no read in between");
    let crossing = Duration::from_secs_f32(2.0 * (0.31f32 / 0.30).ln());
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
            1 => 0.31,
            2 => 0.305,
            _ => 0.05,
        };
        Ok((v, Instant::now()))
    };
    let t0 = Instant::now();
    let out = cmd_await_momentum(
        &h.term,
        &store,
        0,
        &h.ctx,
        "momentum 0.30 timeout=60000",
        &read,
    );
    let waited = t0.elapsed();
    assert_eq!(out, "OK momentum 0.05\n");
    assert_eq!(reads.get(), 3);
    let second = Duration::from_secs_f32(2.0 * (0.305f32 / 0.30).ln());
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
        &h.ctx,
        "momentum 0.30 timeout 120",
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
        Ok((if reads.get() == 1 { 0.31 } else { 0.0 }, Instant::now()))
    };
    let t0 = Instant::now();
    let out = cmd_turn(
        &h.term,
        &store,
        0,
        "yield=0.30 submit=none idle=20 timeout=60000 after you",
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            key: &key,
            momentum: &momentum,
            driver: None,
            by: crate::turn_ledger::TurnBy::Ctl,
            actor: aterm_slog::Actor::Ctl,
            cause: None,
            mark: None,
        },
    );
    let verdict = out.lines().next().unwrap_or("");
    let crossing = Duration::from_secs_f32(2.0 * (0.31f32 / 0.30).ln());
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
        &h.ctx,
        &TurnIo {
            paste: &untyped,
            press: &press,
            key: &key,
            momentum: &never,
            driver: None,
            by: crate::turn_ledger::TurnBy::Ctl,
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

    // Already clear => latched at arm, answered as `gone` with the latch seq.
    let out = cmd_await(&h.term, &store, 0, &h.ctx, "gone interrupt timeout=2000");
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
    let out = cmd_await(&h.term, &store, 0, &h.ctx, "gone interrupt timeout=150");
    assert_eq!(
        out, "OK timeout\n",
        "a visible match holds `gone` pending to the deadline"
    );

    // It latches on the clearing frame, woken by the ring the PTY reader makes
    // after a burst. A lost wake would park to the two-minute timeout, where the
    // re-poll still latches, so the bound sits a minute clear of both.
    let (term_t, watchers) = (h.term.clone(), h.ctx.watchers.clone());
    let painter = std::thread::spawn(move || {
        // Painted once the await is parked (its subscriber registered), so the notify
        // path, not the arm-time latch, is exercised.
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the `gone` await parks on the session",
            || (watchers.count() == 1).then_some(()),
        );
        crate::term_lock(&term_t).process(b"\x1b[H\x1b[2Jresponse-line\r\n\xe2\x9d\xaf ");
        watchers.ring_watchers();
    });
    let t0 = std::time::Instant::now();
    let out = cmd_await(&h.term, &store, 0, &h.ctx, "gone interrupt timeout=120000");
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
    assert_eq!(
        cmd_await(&h.term, &store, 0, &h.ctx, "gone (unclosed timeout=100",),
        "ERR badregex\n"
    );
    // A 24-row grid: rows 40..=50 meet nothing, rows 5..=2 is inverted.
    for args in [
        "gone interrupt rows 40 50",
        "gone interrupt rows 5 2",
        "match interrupt rows 40 50",
    ] {
        let out = cmd_await(&h.term, &store, 0, &h.ctx, &format!("{args} timeout=100"));
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
        let term = &h.term;
        let painter: Cell<Option<std::thread::JoinHandle<()>>> = Cell::new(None);
        let paste = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            term.lock().unwrap().process(row.press_paint);
            let (term_t, watchers, script) = (term.clone(), h.ctx.watchers.clone(), row.script);
            painter.set(Some(std::thread::spawn(move || {
                for &(ms, bytes) in script {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    crate::term_lock(&term_t).process(bytes);
                    watchers.ring_watchers();
                }
            })));
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            row.options,
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

/// `turn settle=submitted` ANSWERS AT THE VERIFIED SUBMIT on a screen that
/// never goes quiet: once the submit lands, a painter recolours one cell
/// every few milliseconds until the turn has answered (Claude Code 2.1.301's
/// animated logo at an idle prompt). The turn answers `submitted=1
/// status=submitted` with the screen, its ledger record says `pressed` (no
/// settle was awaited), and a done task opened again at the submit as ever.
/// Its `idle=` is past any wait here, so the answer can only be the submit's.
/// NEGATIVE CONTROL: the default settle on the same screen and options, its
/// timeout short, does not answer at the submit (`status=timeout`) — neither
/// outcome hangs on how often the painter gets the CPU. An unverified submit
/// is `status=timeout` under `settle=submitted` too.
#[test]
fn turn_settle_submitted_answers_at_the_verified_submit_on_a_screen_that_never_rests() {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering};
    let run = |options: &str, lands: bool| {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        crate::task_record::mark_session_task(
            &h.ctx,
            0,
            &crate::task_record::TaskMark::new(crate::task_record::TaskState::Done),
        )
        .expect("unfenced");
        let term = &h.term;
        let stop = Arc::new(AtomicBool::new(false));
        let painter: Cell<Option<std::thread::JoinHandle<()>>> = Cell::new(None);
        let paste = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            if !lands {
                return true;
            }
            term.lock().unwrap().process(b"\r\nworking\r\n");
            let (term_t, watchers_t, stop_t) =
                (term.clone(), h.ctx.watchers.clone(), Arc::clone(&stop));
            painter.set(Some(std::thread::spawn(move || {
                let mut red = false;
                while !stop_t.load(Ordering::SeqCst) {
                    red = !red;
                    let frame: &[u8] = if red {
                        b"\x1b7\x1b[1;1H\x1b[31m*\x1b[0m\x1b8"
                    } else {
                        b"\x1b7\x1b[1;1H\x1b[32m*\x1b[0m\x1b8"
                    };
                    crate::term_lock(&term_t).process(frame);
                    watchers_t.ring_watchers();
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            })));
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            options,
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        );
        stop.store(true, Ordering::SeqCst);
        if let Some(j) = painter.take() {
            j.join().unwrap();
        }
        let ledger = h.ctx.turns.lock().unwrap();
        let status = ledger.since(None).last().map(|r| r.status);
        drop(ledger);
        let task = h.ctx.timeline.lock().unwrap().task().map(|r| r.state);
        (out, status, task)
    };
    let (out, status, task) = run("settle=submitted idle=60000 timeout=60000 carry on", true);
    let verdict = out.lines().next().unwrap_or("");
    assert!(
        verdict.starts_with("OK 24 turn submitted=1 status=submitted seq="),
        "{verdict}"
    );
    assert!(out.contains("working"), "the screen rides the reply: {out}");
    assert_eq!(status, Some(crate::turn_ledger::STATUS_PRESSED));
    assert_eq!(task, Some(crate::task_record::TaskState::Working));
    // NEGATIVE CONTROL: the default settle on the same screen.
    let (out, status, _) = run("idle=60000 timeout=1000 carry on", true);
    let verdict = out.lines().next().unwrap_or("");
    assert!(
        verdict.starts_with("OK 24 turn submitted=1 status=timeout seq="),
        "{verdict}"
    );
    assert_eq!(status, Some("timeout"));
    // An unverified submit.
    let (out, status, task) = run(
        "settle=submitted presses=1 submit_window=50 timeout=60000 carry on",
        false,
    );
    let verdict = out.lines().next().unwrap_or("");
    assert!(
        verdict.starts_with("OK 24 turn submitted=0 status=timeout seq="),
        "{verdict}"
    );
    assert_eq!(status, Some("timeout"));
    assert_eq!(
        task,
        Some(crate::task_record::TaskState::Done),
        "not opened"
    );
}

/// A Claude Code screen at 24x80: the logo row, the composer's caret row
/// between its two rules, the mode row under them.
fn claude_composer_screen() -> Vec<u8> {
    let rule = "\u{2500}".repeat(80);
    format!(
        "\x1b[1;1H\u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\u{259c}\u{258c}   Claude Code v2.1.301\
         \x1b[20;1H{rule}\x1b[21;1H\u{276f} \x1b[22;1H{rule}\
         \x1b[23;1H  \u{23f5}\u{23f5} bypass permissions on (shift+tab to cycle)\x1b[21;3H"
    )
    .into_bytes()
}

/// THE COMPOSER VERIFY (P2 of the rl review): on an identified Claude Code
/// session whose composer shows the typed text, `turn` verifies its submit
/// by that text LEAVING the composer, which a repaint elsewhere never does.
/// Once the Enter is written a painter recolours the logo every few
/// milliseconds (2.1.301's animation at an idle prompt). A program that
/// ignores the Enter keeps the text in its composer: `submitted=0`, though
/// the screen moved all through the window. One that takes it empties its
/// composer and echoes the line above: `submitted=1`, each the negative
/// control of the other. A box drawn over the composer (a slash command's
/// menu) is a landed press too, judged at the window's close and never
/// pressed again. Claude Code's dim placeholder back in the composer is a
/// line taken, though its words spell the typed one (`Press up to edit
/// queued messages` after `edit`). NEGATIVE CONTROL: the same ignoring
/// screen, the session not read as Claude Code, keeps AUTO's `seq` — the
/// animation verifies it, as every other program's turn is verified.
#[test]
fn turn_into_claude_code_verifies_its_submit_by_the_composer_not_a_repaint() {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[derive(Clone, Copy)]
    enum App {
        Ignores,
        Takes,
        Box,
        Placeholder,
    }
    let run = |app: App, claude: bool, options: &str| {
        let store = session_store::new_store();
        let h = registered_session(0, -1, &claude_composer_screen());
        store.write().unwrap().register(h.clone());
        if claude {
            h.ctx.timeline.lock().unwrap().publish_agent(
                "idle",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        }
        let term = &h.term;
        let stop = Arc::new(AtomicBool::new(false));
        let painter: Cell<Option<std::thread::JoinHandle<()>>> = Cell::new(None);
        let presses = Cell::new(0u32);
        // Claude Code echoes a typed line into its composer.
        let paste = |text: &str| {
            term.lock()
                .unwrap()
                .process(format!("\x1b[21;1H\x1b[2K\u{276f} {text}").as_bytes());
            true
        };
        let press = |_: &str| {
            presses.set(presses.get() + 1);
            match app {
                App::Ignores => {}
                App::Takes => term.lock().unwrap().process(
                    "\x1b[18;1H\u{276f} carry on\x1b[21;1H\x1b[2K\u{276f} \x1b[21;3H".as_bytes(),
                ),
                App::Box => term.lock().unwrap().process(
                    "\x1b[20;1H\x1b[J Select model\r\n \u{276f} 1. Default\r\n   2. Opus"
                        .as_bytes(),
                ),
                App::Placeholder => term.lock().unwrap().process(
                    "\x1b[18;1H\u{276f} edit\x1b[21;1H\x1b[2K\u{276f} Press up to edit queued \
                     messages\x1b[21;3H"
                        .as_bytes(),
                ),
            }
            if presses.get() == 1 {
                let (term_t, watchers_t, stop_t) =
                    (term.clone(), h.ctx.watchers.clone(), Arc::clone(&stop));
                painter.set(Some(std::thread::spawn(move || {
                    let mut red = false;
                    while !stop_t.load(Ordering::SeqCst) {
                        red = !red;
                        let frame: &[u8] = if red {
                            b"\x1b7\x1b[1;1H\x1b[31m*\x1b[0m\x1b8"
                        } else {
                            b"\x1b7\x1b[1;1H\x1b[32m*\x1b[0m\x1b8"
                        };
                        crate::term_lock(&term_t).process(frame);
                        watchers_t.ring_watchers();
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                })));
            }
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            options,
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        );
        stop.store(true, Ordering::SeqCst);
        if let Some(j) = painter.take() {
            j.join().unwrap();
        }
        (out.lines().next().unwrap_or("").to_string(), presses.get())
    };
    // The window is a hang bound where the answer must come; short where it
    // must close with nothing verified.
    let (verdict, _) = run(
        App::Takes,
        true,
        "settle=submitted presses=1 submit_window=60000 timeout=60000 carry on",
    );
    assert!(
        verdict.contains("submitted=1 status=submitted"),
        "{verdict}"
    );
    let (verdict, presses) = run(
        App::Ignores,
        true,
        "settle=submitted presses=2 submit_window=200 timeout=60000 carry on",
    );
    assert!(verdict.contains("submitted=0 status=timeout"), "{verdict}");
    assert_eq!(presses, 2, "the text still in the composer: pressed again");
    let (verdict, presses) = run(
        App::Box,
        true,
        "settle=submitted presses=3 submit_window=200 timeout=60000 /model",
    );
    assert!(
        verdict.contains("submitted=1 status=submitted"),
        "{verdict}"
    );
    assert_eq!(presses, 1, "never an Enter into the box");
    let (verdict, presses) = run(
        App::Placeholder,
        true,
        "settle=submitted presses=2 submit_window=60000 timeout=60000 edit",
    );
    assert!(
        verdict.contains("submitted=1 status=submitted"),
        "{verdict}"
    );
    assert_eq!(
        presses, 1,
        "the placeholder is no line left in the composer"
    );
    // NEGATIVE CONTROL: no Claude Code read, AUTO keeps `seq`.
    let (verdict, _) = run(
        App::Ignores,
        false,
        "settle=submitted presses=1 submit_window=60000 timeout=60000 carry on",
    );
    assert!(
        verdict.contains("submitted=1 status=submitted"),
        "{verdict}"
    );
}

/// A WORKER'S TURN NAMES THE TASK REV ITS ENTER FOUND (`task_rev=`, P1 of
/// the rl review): the follow-up into a done worker answers the done rev,
/// which `await task @<sid>:<rev>` waits past — the reopened `working` is no
/// news there and the follow-up's own done is, even one marked before the
/// wait is armed. NEGATIVE CONTROL: a session with no task names none.
#[test]
fn a_turn_into_a_worker_names_the_task_rev_its_enter_found() {
    use crate::task_record::{Combine, TaskMark, TaskState, mark_session_task};
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let paste = |text: &str| {
        term.lock().unwrap().process(text.as_bytes());
        true
    };
    let press = |_: &str| {
        term.lock().unwrap().process(b"\r\nreply\r\n");
        true
    };
    let turn = || {
        let out = cmd_turn(
            term,
            &store,
            0,
            "settle=submitted timeout=60000 also the README",
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        );
        out.lines().next().unwrap_or("").to_string()
    };
    let verdict = turn();
    assert!(verdict.contains("submitted=1"), "{verdict}");
    assert!(!verdict.contains("task_rev="), "no task: {verdict}");
    let done = TaskMark {
        why: Some("marker"),
        ..TaskMark::new(TaskState::Done)
    };
    let (found, _) = mark_session_task(&h.ctx, 0, &done).expect("unfenced");
    let verdict = turn();
    let named: u64 = verdict
        .split_whitespace()
        .find_map(|w| w.strip_prefix("task_rev="))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no task_rev=: {verdict}"));
    assert_eq!(named, found.rev);
    let wait = |timeout_ms| {
        let target = [super::control_session::TaskTarget {
            local_id: 0,
            ctx: &h.ctx,
            since: Some(named),
        }];
        super::control_session::await_task(&store, &target, Combine::Any, timeout_ms)
    };
    // The reopened `working` is no news past the found rev.
    assert!(wait(0).starts_with("OK 1 status=timeout\n"), "{}", wait(0));
    // The follow-up's own done, marked before the wait is armed, answers it.
    mark_session_task(&h.ctx, 0, &done).expect("unfenced");
    let rows = wait(60_000);
    assert!(rows.starts_with("OK 1\n"), "{rows}");
    assert!(rows.contains("state=done"), "{rows}");
}

/// A TURN INTO AN AGENT NAMES THE VERDICT REV ITS ENTER FOUND (`agent_rev=`):
/// a `settle=submitted` turn answers as its submit lands, before the status
/// sweep republishes the verdict, so the `idle` from before the submit still
/// stands — and `await agent idle,… since=<agent_rev>` is no answer to it:
/// it answers on the `idle` published after the turn's `busy`, by that
/// verdict's rev. NEGATIVE CONTROLS: the latched wait with no `since=`
/// answers the stale `idle` at once; a session no agent is in names no
/// `agent_rev=`; a `since=` that is no rev is the usage line.
#[test]
fn a_turn_into_an_agent_names_the_verdict_rev_its_enter_found() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let paste = |text: &str| {
        term.lock().unwrap().process(text.as_bytes());
        true
    };
    let press = |_: &str| {
        term.lock().unwrap().process(b"\r\nreply\r\n");
        true
    };
    let turn = || {
        let out = cmd_turn(
            term,
            &store,
            0,
            "settle=submitted submit_verify=seq timeout=60000 carry on",
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        );
        out.lines().next().unwrap_or("").to_string()
    };
    let verdict = turn();
    assert!(verdict.contains("submitted=1"), "{verdict}");
    assert!(!verdict.contains("agent_rev="), "no agent: {verdict}");
    let publish = |word: &'static str, n: u64| {
        h.ctx.timeline.lock().unwrap().publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: n },
                fp: n,
            },
        )
    };
    assert!(publish("idle", 1));
    let verdict = turn();
    let found: u64 = verdict
        .split_whitespace()
        .find_map(|w| w.strip_prefix("agent_rev="))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no agent_rev=: {verdict}"));
    assert_eq!(found, h.ctx.timeline.lock().unwrap().agent().rev);
    let wait = |tail: &str| cmd_await(term, &store, 0, &h.ctx, tail);
    let past = format!("agent idle,prompt,question,wall since={found}");
    // NEGATIVE CONTROL: latched, the idle from before the submit answers.
    assert_eq!(
        wait("agent idle,prompt,question,wall timeout=0"),
        format!("OK agent idle rev={found}\n")
    );
    // Past the Enter's rev, the stale idle is no answer, nor the busy.
    assert_eq!(wait(&format!("{past} timeout=0")), "OK timeout\n");
    assert!(publish("busy", 2));
    assert_eq!(wait(&format!("{past} timeout=0")), "OK timeout\n");
    assert!(publish("idle", 3));
    assert_eq!(
        wait(&format!("{past} timeout=60000")),
        format!("OK agent idle rev={}\n", found + 2)
    );
    // NEGATIVE CONTROL: a since= that is no rev.
    assert!(
        wait("agent idle since=soon timeout=0").starts_with("ERR usage"),
        "{}",
        wait("agent idle since=soon timeout=0")
    );
}

/// `settle=submitted` is one word of `settle=`'s, named by the usage line;
/// it needs a submit to answer at (`submit=none` with it is the usage line,
/// nothing typed), and a later `settle=` takes its place. A word that is not
/// it is the usage line.
#[test]
fn turn_settle_submitted_parses_and_needs_a_submit() {
    use std::cell::Cell;
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let pasted = Cell::new(0u32);
    let paste = |text: &str| {
        pasted.set(pasted.get() + 1);
        term.lock().unwrap().process(text.as_bytes());
        true
    };
    let press = |_: &str| {
        term.lock().unwrap().process(b"\r\nreply\r\n");
        true
    };
    let turn = |options: &str| {
        cmd_turn(
            term,
            &store,
            0,
            options,
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        )
    };
    for bad in [
        "settle=submitted submit=none hello",
        "submit=none settle=submitted hello",
        "settle=submit hello",
        "settle=submitted: hello",
    ] {
        let out = turn(bad);
        assert!(out.starts_with("ERR usage: turn"), "{bad}: {out}");
        assert!(
            out.contains("[settle=match:<re>|gone:<re>|submitted]"),
            "{bad}: {out}"
        );
    }
    assert_eq!(pasted.get(), 0, "nothing typed");
    // A later `settle=` takes its place: the idle settle settles.
    let out = turn("settle=submitted settle=match:reply idle=50 timeout=60000 hello");
    assert!(
        out.lines()
            .next()
            .unwrap_or("")
            .contains("submitted=1 status=settled"),
        "{out}"
    );
}

/// `turn`'s submit verification, one row per case. Each row builds a fresh session,
/// optionally seeds OSC 133 state, pastes with echo, and scripts what each Enter
/// paints (by press number; the last entry repeats, and an empty paint is a
/// swallowed press). A press paints synchronously, so a row whose first press
/// does not land waits a short `submit_window`, not the 2 s default, before it
/// presses again.
///
/// The rows about the started-command counter carry a NEGATIVE CONTROL read
/// before and after the first press's paint, shown to give the wrong answer on
/// that row's fixture: the count of blocks that reached C (Executing or
/// Complete), the signal `turn` once verified by, or `content_seq`, whose
/// advance is all that wakes the block check inside the window. Each such row
/// exercises a defect the counter, read at every wake and once more as the
/// window closes, does not have.
#[test]
fn turn_submit_verification_rows() {
    use aterm_core::terminal::{BlockState, Terminal};
    use std::cell::Cell;
    #[derive(Clone, Copy)]
    struct Probe {
        started_blocks: usize,
        content_seq: u64,
    }
    /// The probe before and after the first press, and why the signal it
    /// reads misjudges the press.
    type Control = (fn(Probe, Probe) -> bool, &'static str);
    struct Row {
        label: &'static str,
        seed: Vec<u8>,
        paints: &'static [&'static [u8]],
        options: &'static str,
        verdict: &'static str,
        presses: u32,
        why: &'static str,
        control: Option<Control>,
    }
    const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";
    const COMMAND: &[u8] =
        b"\x1b]133;A\x07$ \x1b]133;B\x07echo\x1b]133;C\x07\r\nout\r\n\x1b]133;D;0\x07";
    let probe = |t: &Terminal| Probe {
        started_blocks: t
            .all_blocks()
            .filter(|b| matches!(b.state, BlockState::Executing | BlockState::Complete))
            .count(),
        content_seq: t.content_seq(),
    };
    let rows = [
        // Verified-submit retry: an Enter swallowed mid-paste-ingestion does not
        // advance `content_seq`, so the verb re-presses and the second lands. The
        // press count shows verification, not a timer, drove the retry.
        Row {
            label: "swallowed first Enter",
            seed: Vec::new(),
            paints: &[b"", b"\r\nlanded\r\n"],
            options: "idle=50 timeout=60000 submit_window=150 msg",
            verdict: "submitted=1 status=settled",
            presses: 2,
            why: "exactly one re-press after the swallow",
            control: None,
        },
        // At a shell prompt (133;A opens the block, 133;B marks input-ready) `turn`
        // auto-verifies against the OSC-133 command start with no `submit_verify=`,
        // and the command start (133;C -> Executing) attributes the press at once.
        Row {
            label: "auto block-verifies at a shell prompt",
            seed: PROMPT.to_vec(),
            paints: &[b"echo\x1b]133;C\x07\r\nrunning"],
            options: "idle=50 timeout=60000 build",
            verdict: "submitted=1",
            presses: 1,
            why: "one press started the command — no re-press",
            control: None,
        },
        // A C that moves no grid content — its command prints nothing in the
        // window (`sleep 3`, `ssh host`) — wakes no content latch: the
        // window's close reads the counter, so no Enter goes into the command.
        Row {
            label: "a C that moves no content verifies at the window's close",
            seed: PROMPT.to_vec(),
            paints: &[b"\x1b]133;C\x07"],
            options: "idle=50 timeout=60000 submit_window=120 presses=2 sleep",
            verdict: "submitted=1",
            presses: 1,
            why: "the started command verified the first press",
            control: Some((
                |before, after| after.content_seq == before.content_seq,
                "the C moved no content: a check run only on a latch never reads it",
            )),
        },
        // A full block ring (1000 archived): the press's command starts, ends and
        // the next prompt archives it in one paint, evicting the oldest block.
        Row {
            label: "a command archived out of a full ring still verifies",
            seed: [COMMAND.repeat(1001).as_slice(), PROMPT].concat(),
            paints: &[b"\x1b]133;C\x07\r\nout\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07"],
            options: "idle=50 timeout=60000 run",
            verdict: "submitted=1",
            presses: 1,
            why: "the started command verified the first press",
            control: Some((
                |before, after| after.started_blocks == before.started_blocks,
                "one block archived, one evicted: the count stays flat",
            )),
        },
        // `clear`: its command starts, then its ED 3 drops every finished block
        // (the engine keeps the running one, so its exit status lands).
        Row {
            label: "a command that clears the scrollback still verifies",
            seed: [COMMAND, PROMPT].concat(),
            paints: &[b"\x1b]133;C\x07\x1b[H\x1b[2J\x1b[3Jafter-clear\r\n"],
            options: "idle=50 timeout=60000 clear",
            verdict: "submitted=1",
            presses: 1,
            why: "no extra Enter after a `clear` that ran",
            control: None,
        },
        // Auto degrade: the target looks like a shell prompt but the 133 stream is
        // desynced, so no press ever produces a command start (e.g. vte.sh
        // double-sourced clobbering PS0's `133;C`), while the echo advances
        // `content_seq`. Auto must not blindly re-press (each Enter is real input)
        // or report a false `submitted=0 status=timeout`; it degrades to the seq
        // verdict. The small submit_window makes the degrade arrive fast.
        Row {
            label: "auto degrades when the prompt block is stale",
            seed: PROMPT.to_vec(),
            paints: &[b"\r\nran-anyway\r\n$ "],
            options: "idle=50 timeout=60000 submit_window=120 run",
            verdict: "submitted=1 status=settled",
            presses: 1,
            why: "a press whose echo moved the screen was consumed — re-pressing would double-type",
            control: None,
        },
        // The degrade must not fire on a proven-healthy stream: once this session
        // started a command (133;C seen), no command plus ambient movement looks
        // like a swallowed Enter beside background output. The strict lane
        // re-presses and reports `submitted=0` when nothing starts.
        Row {
            label: "auto stays strict once the stream has started blocks",
            seed: [COMMAND, PROMPT].concat(),
            paints: &[b"\r\n[bg] tick"],
            options: "idle=50 timeout=60000 submit_window=120 presses=2 run",
            verdict: "submitted=0",
            presses: 2,
            why: "strictness holds — the press budget retries instead of overclaiming",
            control: None,
        },
        // A healthy stream whose blocks a reset took (RIS; a clear keeps a
        // finished block) is still a healthy stream: the C it accepted before
        // the reset keeps AUTO strict.
        Row {
            label: "auto stays strict after a reset took the started blocks",
            seed: [COMMAND, b"\x1bc", PROMPT].concat(),
            paints: &[b"\r\n[bg] tick"],
            options: "idle=50 timeout=60000 submit_window=120 presses=2 run",
            verdict: "submitted=0",
            presses: 2,
            why: "strictness holds — the reset stream is not a desynced one",
            control: Some((
                |before, _| before.started_blocks == 0,
                "no started block left at the press: AUTO would degrade to submitted=1",
            )),
        },
        // `submit_verify=block` verifies against the OSC-133 command start, not a
        // bare `content_seq` advance, so an ambient repaint (a spinner frame) cannot
        // false-verify; `seq` mode would on the first press.
        Row {
            label: "submit_verify=block ignores an ambient repaint",
            seed: PROMPT.to_vec(),
            paints: &[b"\x1b[2K spinner-tick ", b"echo\x1b]133;C\x07\r\ndone"],
            options: "idle=50 timeout=60000 submit_window=150 submit_verify=block run",
            verdict: "submitted=1",
            presses: 2,
            why: "the ambient repaint did NOT verify; a second press started the block",
            control: None,
        },
    ];
    // Each row is a session of its own, judged by its own presses and windows: side by side.
    let run = |row: &Row| {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;
        if !row.seed.is_empty() {
            term.lock().unwrap().process(&row.seed);
        }
        let presses = Cell::new(0u32);
        let first_press = Cell::new(None);
        let paste = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            presses.set(presses.get() + 1);
            let at = (presses.get() as usize - 1).min(row.paints.len() - 1);
            let paint = row.paints[at];
            let mut t = term.lock().unwrap();
            let before = probe(&t);
            if !paint.is_empty() {
                t.process(paint);
            }
            if presses.get() == 1 {
                first_press.set(Some((before, probe(&t))));
            }
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            row.options,
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        );
        (out, presses.get(), first_press.get())
    };
    let ran: Vec<_> = std::thread::scope(|scope| {
        let runs: Vec<_> = rows
            .iter()
            .map(|row| scope.spawn(move || run(row)))
            .collect();
        runs.into_iter()
            .map(|r| r.join().expect("a row's turn"))
            .collect()
    });
    for (row, (out, presses, first_press)) in rows.iter().zip(ran) {
        assert!(
            out.contains(row.verdict),
            "{}: {}",
            row.label,
            out.lines().next().unwrap_or("")
        );
        assert_eq!(presses, row.presses, "{}: {}", row.label, row.why);
        if let Some((misjudges, what)) = row.control {
            let (before, after) = first_press.expect("the turn pressed");
            assert!(
                misjudges(before, after),
                "{}: control: {what} (started blocks {} -> {}, content_seq {} -> {})",
                row.label,
                before.started_blocks,
                after.started_blocks,
                before.content_seq,
                after.content_seq
            );
        }
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
    let turn = || cmd_turn(term, &store, 0, "timeout=300 idle=1 hi", &h.ctx, &io);
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
        here: false,
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

/// `here` KEEPS A LEASE TO THE CONNECTION THAT ASKS: every lane of one
/// process takes its hand under one holder name, and a plain acquire under
/// it moves another lane's live hand to the asker, a plain release frees it
/// from anywhere. With `here` an acquire of the same holder's live lease
/// held over another connection is refused as a different holder's is (the
/// hand stays where it is), one over the asker's own is renewed, and a
/// release frees only the asker's own. NEGATIVE CONTROLS: without `here`
/// the same acquire moves the hand and the same release frees it; a lapsed
/// lease is taken and released by anyone.
#[test]
fn a_lease_kept_here_is_never_moved_or_freed_from_another_connection() {
    let h = registered_session(0, -1, b"");
    let on = |conn: u64| SERVING_CONNECTION.with(|s| s.set(Some(conn)));
    let held_over = || match h.ctx.turn_lease.lock().unwrap().as_ref() {
        Some(crate::Lease::Drive { conn, .. }) => *conn,
        other => panic!("a drive lease: {other:?}"),
    };
    on(7);
    assert!(
        cmd_lease(&h.ctx, "acquire holder=aterm-harness@1 ttl=60000 hard")
            .starts_with("OK lease acquired")
    );
    on(8);
    let refused = cmd_lease(&h.ctx, "acquire holder=aterm-harness@1 ttl=60000 hard here");
    assert!(
        refused.starts_with("ERR lease held holder=aterm-harness@1 "),
        "{refused}"
    );
    assert_eq!(held_over(), Some(7), "not moved");
    assert_eq!(
        cmd_lease(&h.ctx, "release holder=aterm-harness@1 here"),
        "ERR lease held by aterm-harness@1 over another connection\n"
    );
    assert_eq!(held_over(), Some(7), "not freed");
    on(7);
    assert!(
        cmd_lease(&h.ctx, "acquire holder=aterm-harness@1 ttl=60000 hard here")
            .starts_with("OK lease acquired"),
        "its own connection's is renewed"
    );
    assert_eq!(held_over(), Some(7));
    assert_eq!(
        cmd_lease(&h.ctx, "release holder=aterm-harness@1 here"),
        "OK lease released\n"
    );
    assert_eq!(cmd_lease(&h.ctx, "status"), "OK lease none\n");

    // NEGATIVE CONTROLS: without `here`, moved and freed from anywhere.
    assert!(
        cmd_lease(&h.ctx, "acquire holder=aterm-harness@1 ttl=60000 hard")
            .starts_with("OK lease acquired")
    );
    on(8);
    assert!(
        cmd_lease(&h.ctx, "acquire holder=aterm-harness@1 ttl=60000 hard")
            .starts_with("OK lease acquired")
    );
    assert_eq!(held_over(), Some(8), "moved");
    on(7);
    assert_eq!(
        cmd_lease(&h.ctx, "release holder=aterm-harness@1"),
        "OK lease released\n"
    );
    // A lapsed one is anyone's, `here` or not.
    *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
        holder: "aterm-harness@1".to_string(),
        expires_us: crate::metrics::now_us().saturating_sub(1),
        conn: Some(8),
        hard: true,
        here: false,
        holder_pid: None,
    });
    assert!(
        cmd_lease(&h.ctx, "acquire holder=aterm-harness@1 ttl=60000 hard here")
            .starts_with("OK lease acquired")
    );
    assert_eq!(held_over(), Some(7));
    *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
        holder: "aterm-harness@1".to_string(),
        expires_us: crate::metrics::now_us().saturating_sub(1),
        conn: Some(8),
        hard: true,
        here: false,
        holder_pid: None,
    });
    assert_eq!(
        cmd_lease(&h.ctx, "release holder=aterm-harness@1 here"),
        "OK lease released\n"
    );
    SERVING_CONNECTION.with(|s| s.set(None));
}

/// A LEASE TAKEN `here` ENDS WITH ITS CONNECTION: the acquire marks the
/// connection for the sweep its end runs (as a bound supervisor claim
/// does), and that sweep frees the hand at once — the turn-end loop's
/// hand when a stop hangs its connection up, which no request from another
/// connection may free. Another connection's end frees nothing. NEGATIVE
/// CONTROLS: the same hand taken without `here` marks nothing and outlives
/// its connection (the upgrade's and relaunch's hand rides a reconnect); a
/// `here` hand a plain renewal moved to another connection is that one's,
/// unbound, and outlives both.
#[test]
fn a_lease_taken_here_ends_with_its_connection() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let on = |conn: u64| SERVING_CONNECTION.with(|s| s.set(Some(conn)));
    let marked = || CONNECTION_CLAIMED.with(|c| c.replace(false));
    let acquire = |rest: &str| {
        let reply = cmd_lease(
            &h.ctx,
            &format!("acquire holder=aterm-harness@1 ttl=60000 {rest}"),
        );
        assert!(reply.starts_with("OK lease acquired"), "{rest}: {reply}");
    };
    let held = || cmd_lease(&h.ctx, "status").starts_with("OK lease holder=aterm-harness@1 ");
    on(7);
    marked();
    acquire("hard here");
    assert!(marked(), "its connection's end sweeps it");
    release_connection_claims(&store, None, 8);
    assert!(held(), "another connection's end frees nothing");
    release_connection_claims(&store, None, 7);
    assert_eq!(cmd_lease(&h.ctx, "status"), "OK lease none\n", "freed");

    // NEGATIVE CONTROLS.
    acquire("hard");
    assert!(!marked(), "nothing to sweep");
    release_connection_claims(&store, None, 7);
    assert!(held(), "a plain hand outlives its connection");
    assert_eq!(
        cmd_lease(&h.ctx, "release holder=aterm-harness@1"),
        "OK lease released\n"
    );
    acquire("hard here");
    on(8);
    acquire("hard");
    release_connection_claims(&store, None, 7);
    release_connection_claims(&store, None, 8);
    assert!(held(), "moved by a plain renewal, unbound");
    SERVING_CONNECTION.with(|s| s.set(None));
    CONNECTION_CLAIMED.with(|c| c.set(false));
}

/// THE CONNECTION'S END FREES A `here` HAND AS A RELEASE DOES: the text the
/// harness typed under it and never submitted is dropped — a later Enter
/// from the same connection, under the hand taken again, books no turn —
/// and the presence rim is woken for the lease that moved. NEGATIVE
/// CONTROL first: under the hand still held, that Enter books the text
/// (`status=pressed`), and another connection's end wakes nothing.
#[test]
fn a_connection_end_drops_the_hands_unsubmitted_text_and_wakes_presence() {
    use aterm_agent::harness::upgrade_drive::hand_take_here_request;
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let on = |conn: u64| SERVING_CONNECTION.with(|s| s.set(Some(conn)));
    let take = || {
        let request = hand_take_here_request();
        let reply = cmd_lease(&h.ctx, request.strip_prefix("lease ").unwrap_or(&request));
        assert!(reply.starts_with("OK lease acquired"), "{reply}");
    };
    let input = |verb: &str, rest: &str| {
        control_session::ledger_typed_input(&h.term, &h.ctx, Scope::Owner, verb, rest, "OK\n");
    };
    let turns = || control_session::cmd_history(&h.ctx, "").lines().count() - 1;
    on(7);

    take();
    input("send", "keep going");
    input("key", "enter");
    assert_eq!(turns(), 1, "under the hand, the Enter books the text");

    input("send", "keep going");
    let _ = crate::presence::take_lease_wakes();
    release_connection_claims(&store, None, 8);
    assert!(
        crate::presence::take_lease_wakes().is_empty(),
        "another connection's end moved nothing"
    );
    release_connection_claims(&store, None, 7);
    assert_eq!(cmd_lease(&h.ctx, "status"), "OK lease none\n", "freed");
    assert_eq!(
        crate::presence::take_lease_wakes(),
        std::slice::from_ref(&h.ctx.self_id),
        "the level follows the lease"
    );
    take();
    input("key", "enter");
    assert_eq!(turns(), 1, "the connection's end dropped the text: no turn");
    SERVING_CONNECTION.with(|s| s.set(None));
    CONNECTION_CLAIMED.with(|c| c.set(false));
}

/// WHO TYPED EACH TURN, on the `history` wire: `by=` names the caller the
/// dispatch resolved ([`TurnIo::by`]), and a turn the WINDOW HARNESS types
/// reads `by=harness` — one typed under the harness's hand, the hard drive
/// lease its supervisor loops take a tab by, held over the turn's own
/// connection: the wire the live upgrade's notice and release and a
/// relaunched agent's continuation go out on, replayed here as the
/// harness's own builders spell it (`aterm-agent`'s
/// `upgrade_drive::hand_take_request`, a `turn`, `hand_give_back_request`).
/// `by=` sits before `arch=`, and `text=` stays the tail every reader cuts
/// at. NEGATIVE CONTROLS: the same connection's turn once the hand is given
/// back is `ctl` again; under another instance's harness hand it is `ctl`;
/// an edge caller and the bridge print their own words.
#[test]
fn history_names_who_typed_each_turn_and_the_harness_hand_reads_harness() {
    use crate::turn_ledger::TurnBy;
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let paste = |_: &str| true;
    // Each press moves the screen, so each turn is verified at its press, not its window's close.
    let press = |_: &str| {
        h.term.lock().unwrap().process(b"\r\nran\r\n");
        true
    };
    let turn = |by: TurnBy, text: &str| {
        let io = TurnIo {
            paste: &paste,
            press: &press,
            by,
            ..TurnIo::paste_only()
        };
        let reply = cmd_turn(
            &h.term,
            &store,
            0,
            &format!("timeout=60000 idle=1 {text}"),
            &h.ctx,
            &io,
        );
        assert!(
            reply.starts_with("OK ") && reply.contains(" submitted=1 "),
            "{text}: {reply}"
        );
    };
    let lease = |request: &str| {
        let rest = request.strip_prefix("lease ").unwrap_or(request);
        let reply = cmd_lease(&h.ctx, rest);
        assert!(reply.starts_with("OK"), "{request}: {reply}");
    };
    SERVING_CONNECTION.with(|s| s.set(Some(7)));
    turn(TurnBy::Ctl, "from ctl");
    lease(&aterm_agent::harness::upgrade_drive::hand_take_request());
    turn(TurnBy::Ctl, "keep going");
    lease(&aterm_agent::harness::upgrade_drive::hand_give_back_request());
    turn(TurnBy::Ctl, "after the hand");
    lease("acquire ttl=60000 holder=aterm-harness@1 hard");
    turn(TurnBy::Ctl, "another harness");
    lease("release holder=aterm-harness@1");
    let peer = aterm_session::SessionId::generate();
    turn(TurnBy::Sid(peer.as_str().to_string()), "from a peer");
    turn(TurnBy::Bridge, "from the bridge");
    SERVING_CONNECTION.with(|s| s.set(None));

    let history = control_session::cmd_history(&h.ctx, "");
    let rows: Vec<(String, String)> = history
        .lines()
        .skip(1)
        .map(|row| {
            let (meta, text) = row.split_once(" text=").expect("a text= tail");
            let by_at = meta.find(" by=").expect("a by= field");
            assert!(by_at < meta.find(" arch=").expect("arch="), "{row}");
            let by = meta[by_at + 4..].split(' ').next().unwrap_or("");
            (by.to_string(), text.to_string())
        })
        .collect();
    let want: Vec<(String, String)> = [
        ("ctl", "from%20ctl"),
        ("harness", "keep%20going"),
        ("ctl", "after%20the%20hand"),
        ("ctl", "another%20harness"),
        (peer.as_str(), "from%20a%20peer"),
        ("bridge", "from%20the%20bridge"),
    ]
    .iter()
    .map(|(by, text)| ((*by).to_string(), (*text).to_string()))
    .collect();
    assert_eq!(rows, want, "{history}");
}

/// THE HARNESS'S `send` AND ITS `key enter` ARE ITS TURN: the turn-end
/// loop's fenced continuation, replayed as the loop sends it under the
/// harness's hand (`hand_take_here_request`, the text, the Enter,
/// `hand_give_back_here_request`) and booked as the dispatch books every input
/// verb's reply ([`control_session::ledger_typed_input`]), reads `history …
/// submitted=1 status=pressed … by=harness … text=keep%20going`, beside a
/// manager's `turn`, `by=ctl`. A write another connection was refused
/// between the two (the hand refuses them) does not part them. NEGATIVE
/// CONTROLS, none of which records anything: the same send and Enter under
/// no hand, under another instance's harness's hand, or from an edge; an
/// Enter that is not the next write behind the text (another key between),
/// one the screen skipped, one after the hand was given back and taken
/// again, and one from another connection; a send that wrote nothing.
#[test]
fn the_harness_send_and_its_enter_are_its_turn_and_nobody_elses_are() {
    use aterm_agent::harness::upgrade_drive::{
        hand_give_back_here_request, hand_take_here_request,
    };
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let on = |conn: u64| SERVING_CONNECTION.with(|s| s.set(Some(conn)));
    let lease = |request: &str| {
        let reply = cmd_lease(&h.ctx, request.strip_prefix("lease ").unwrap_or(request));
        assert!(reply.starts_with("OK"), "{request}: {reply}");
    };
    let input = |scope: Scope, verb: &str, rest: &str, reply: &str| {
        control_session::ledger_typed_input(&h.term, &h.ctx, scope, verb, rest, reply);
    };
    let typed = |scope: Scope, enter_on: u64, between: &[(&str, &str, &str)], reply: &str| {
        input(scope, "send", "keep going", "OK\n");
        for (verb, rest, reply) in between {
            input(Scope::Owner, verb, rest, reply);
        }
        on(enter_on);
        input(scope, "key", "enter", reply);
        on(7);
    };
    let rows = || {
        control_session::cmd_history(&h.ctx, "")
            .lines()
            .skip(1)
            .map(str::to_string)
            .collect::<Vec<String>>()
    };
    on(7);

    // The harness's continuation, a refused write between its two halves.
    lease(&hand_take_here_request());
    on(8);
    input(
        Scope::Owner,
        "send",
        "x",
        "ERR busy lease=aterm-harness@1\n",
    );
    on(7);
    typed(Scope::Owner, 7, &[], "OK\n");
    lease(&hand_give_back_here_request());
    let rows_now = rows();
    assert_eq!(rows_now.len(), 1, "{rows_now:?}");
    let row = &rows_now[0];
    for word in [
        "submitted=1",
        "status=pressed",
        "by=harness",
        "text=keep%20going",
    ] {
        assert!(row.split_whitespace().any(|w| w == word), "{word}: {row}");
    }

    // Negative controls: nothing more is recorded.
    typed(Scope::Owner, 7, &[], "OK\n");
    lease("lease acquire ttl=60000 holder=aterm-harness@1 hard");
    typed(Scope::Owner, 7, &[], "OK\n");
    lease("lease release holder=aterm-harness@1");
    lease(&hand_take_here_request());
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    typed(edge, 7, &[], "OK\n");
    typed(Scope::Owner, 7, &[("key", "esc", "OK\n")], "OK\n");
    typed(Scope::Owner, 7, &[], "OK skipped reason=changed\n");
    typed(Scope::Owner, 8, &[], "OK\n");
    input(Scope::Owner, "send", "keep going", "OK skipped\n");
    input(Scope::Owner, "key", "enter", "OK\n");
    input(Scope::Owner, "send", "keep going", "OK\n");
    lease(&hand_give_back_here_request());
    lease(&hand_take_here_request());
    input(Scope::Owner, "key", "enter", "OK\n");
    lease(&hand_give_back_here_request());
    assert_eq!(
        rows(),
        rows_now,
        "no other write is a turn of the harness's"
    );

    // A manager's turn beside it is its caller's: `by` as the dispatch
    // resolves it for an owner-token connection (`turn_driver`, `turn_by`).
    let paste = |_: &str| true;
    let press = |_: &str| true;
    let driver = crate::control::turn_driver(Scope::Owner, &h.ctx);
    let io = TurnIo {
        paste: &paste,
        press: &press,
        by: control_session::turn_by(Scope::Owner, driver.as_ref()),
        ..TurnIo::paste_only()
    };
    let reply = cmd_turn(&h.term, &store, 0, "timeout=300 idle=1 next", &h.ctx, &io);
    assert!(reply.starts_with("OK "), "{reply}");
    let last = rows().pop().expect("the manager's row");
    assert!(last.split_whitespace().any(|w| w == "by=ctl"), "{last}");
    SERVING_CONNECTION.with(|s| s.set(None));
}

/// A TURN THAT LANDS IN A FINISHED WORKER OPENS ITS TASK AGAIN AT ONCE
/// (`task_record::reopen_at_turn`): a `turn` from `aterm ctl` into a session
/// whose task is `done` moves the record to `working why=turn` as its submit
/// lands, before anything of the worker's turn ends. NEGATIVE CONTROLS: the
/// same turn typed under the window harness's hand (`by=harness`: the live
/// upgrade's READY notice) leaves `done` standing; and a turn whose submit
/// never verifiably landed opens nothing.
#[test]
fn a_turn_into_a_done_worker_opens_its_task_unless_the_harness_typed_it() {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    use crate::turn_ledger::TurnBy;
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let paste = |text: &str| {
        term.lock().unwrap().process(text.as_bytes());
        true
    };
    // The app takes the submit and prints its reply: the press lands.
    let landed = |_: &str| {
        term.lock().unwrap().process(b"\r\nreply\r\n");
        true
    };
    // A press the app swallowed: nothing moves.
    let swallowed = |_: &str| true;
    let turn = |press: &dyn Fn(&str) -> bool, text: &str| {
        let io = TurnIo {
            paste: &paste,
            press,
            by: TurnBy::Ctl,
            ..TurnIo::paste_only()
        };
        cmd_turn(
            term,
            &store,
            0,
            &format!("idle=1 timeout=300 submit_window=50 {text}"),
            &h.ctx,
            &io,
        )
    };
    let done = || {
        let mark = TaskMark {
            why: Some("marker"),
            ..TaskMark::new(TaskState::Done)
        };
        mark_session_task(&h.ctx, 0, &mark).expect("unfenced");
    };
    let state = || {
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .map(|r| (r.state, r.why))
    };
    SERVING_CONNECTION.with(|s| s.set(Some(7)));
    done();
    // Under the harness's hand: its own turn.
    let hand = aterm_agent::harness::upgrade_drive::hand_name();
    assert!(cmd_lease(&h.ctx, &format!("acquire ttl=60000 holder={hand} hard")).starts_with("OK"));
    let reply = turn(&landed, "[aterm harness] reply READY");
    assert!(reply.contains("submitted=1"), "{reply}");
    assert!(cmd_lease(&h.ctx, &format!("release holder={hand}")).starts_with("OK"));
    assert_eq!(
        state(),
        Some((TaskState::Done, Some("marker"))),
        "the harness's turn"
    );
    // A swallowed submit.
    let reply = turn(&swallowed, "also the README");
    assert!(reply.contains("submitted=0"), "{reply}");
    assert_eq!(
        state(),
        Some((TaskState::Done, Some("marker"))),
        "nothing landed"
    );
    // A manager's follow-up that lands.
    let reply = turn(&landed, "also the README");
    assert!(reply.contains("submitted=1"), "{reply}");
    assert_eq!(state(), Some((TaskState::Working, Some("turn"))));
    SERVING_CONNECTION.with(|s| s.set(None));
}

/// A FORCED preemption of a DRIVEN turn wakes the driving session too: the
/// driver's presence facts read the peer's lease (`status hand=`), and the
/// wedged turn's own guard finds the slot taken or empty when it drops and
/// posts nothing — so without the driver's wake its facts keep a lease mark
/// that has moved until something else re-reads them. A refused release
/// posts nothing, and an undriven turn wakes only itself.
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
        [own.clone(), driver],
        "the preempted turn's session AND its driving session are woken"
    );

    // An undriven turn: its own session alone.
    *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
        id: 79,
        driver: None,
        typing: true,
    });
    assert_eq!(cmd_lease(&h.ctx, "release force"), "OK lease released\n");
    assert_eq!(crate::presence::take_lease_wakes(), [own]);
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
/// without a relaunch. Drives the decision function [`dispatch_owner`] calls,
/// against the real launcher with an argv that exits at once (`true`), enough
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
        let Head {
            selector,
            verb,
            rest,
        } = control_request::head(line);
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
            "OK state=absent supervised=0 command=- argv=- reason=- rtt_ms=- link_age_ms=- {unattached}\n"
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
            "OK state=absent supervised=0 command=- argv=- reason=- rtt_ms=- link_age_ms=- {unattached}\n"
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
            "OK state=absent supervised=1 command={command} argv={command} reason=- rtt_ms=- link_age_ms=- {unattached}\n"
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
            "OK state=absent supervised=1 command={command} argv={command} reason=- rtt_ms=- link_age_ms=- {unattached}\n"
        )
    );
}

#[cfg(not(unix))]
fn fabric_attach_body() {
    let store = session_store::new_store();
    assert_eq!(
        dispatch_fabric_verb("status", None, Scope::Owner, &store),
        format!(
            "OK state=absent supervised=0 command=- argv=- reason=- rtt_ms=- link_age_ms=- build={} \
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
        let Head {
            selector,
            verb,
            rest,
        } = control_request::head(line);
        dispatch_bridge_verb(verb, rest, selector.as_ref(), scope, &store)
    };

    for (scope, name) in [(Scope::Owner, "owner"), (edge, "edge")] {
        assert_eq!(
            run(
                scope,
                &format!("deliver {sid} off=1 from=h-a kind=task text=x")
            ),
            "ERR denied\n",
            "deliver from {name} scope"
        );
        // The outbound plane is the same authority: an Owner reading `outbox` would
        // read every session's outbound traffic, and forging `outbox sent` would
        // release a `post --wait` for a message that never left.
        assert_eq!(
            run(scope, "outbox"),
            "ERR denied\n",
            "outbox from {name} scope"
        );
        assert_eq!(
            run(scope, &format!("outbox sent {sid} 1 off=9")),
            "ERR denied\n",
            "outbox sent from {name} scope"
        );
    }
    // `hold` is not on this plane any more: from EVERY scope, the bridge's
    // included, the router hands it to the owner-only dispatch, which is where its
    // gate lives.
    assert_eq!(
        route("hold", aterm_types::control_verbs::spec_for("hold", "")).map(|(r, _)| r),
        Ok(Route::Owner),
    );
    // From the inherited bridge connection, both land.
    assert_eq!(
        run(
            Scope::Bridge,
            &format!("deliver {sid} off=1 from=h-a kind=task trust=human text=x")
        ),
        "OK 1\n"
    );
    assert_eq!(
        run(Scope::Bridge, "outbox"),
        "OK 0\n",
        "an empty outbox is a zero-length frame, not a refusal"
    );
    assert_eq!(
        run(Scope::Bridge, &format!("outbox sent {sid} 1 off=9")),
        "ERR no such post\n",
        "reached the handler, which is the point"
    );
    // A SELECTOR is refused even from the bridge: `@<other> deliver <sid> …`
    // would otherwise aim the bridge plane at a session the sid does not name.
    assert_eq!(
        run(
            Scope::Bridge,
            &format!("@1 deliver {sid} off=2 from=h-a kind=task text=x")
        ),
        "ERR denied\n"
    );
    // Every BridgeOnly row of the table reaches a handler (bare, so each answers
    // its usage or an empty read, never the `no_handler` refusal).
    for spec in aterm_types::control_verbs::VERBS {
        if spec.access == aterm_types::control_verbs::Access::BridgeOnly {
            assert_ne!(
                run(Scope::Bridge, spec.name),
                no_handler(spec.name),
                "bridge-only {:?} has no dispatch arm",
                spec.name
            );
        }
    }
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
        let Head {
            selector,
            verb,
            rest,
        } = control_request::head(line);
        dispatch_bridge_verb(verb, rest, selector.as_ref(), Scope::Bridge, &store)
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
/// `off` lifts only a fleet one. Drives the decision function [`dispatch_owner`] calls.
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
        let Head {
            selector,
            verb,
            rest,
        } = control_request::head(line);
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

/// THE INPUT ADMISSION'S ORDER (`admit`), on a real pty. Every gate armed at once —
/// a hold, a self-feed floor the write overdraws, a turn's lease, an edit ticket's
/// hold, a malformed leading option and a published stall behind the person's
/// unread `q` — and the halt answers; lifting each in turn hands the answer to the
/// next: halt, floor, lease, edit hold, usage, the request's own words, unread-input
/// gate, then the write is admitted. The edit hold answers before the usage step and
/// before the words, which it never asks. The class decides which gates ask:
/// `signal` meets none of the floor, the lease or the stall.
///
/// Negative control: each gate armed alone refuses with its own line, so the
/// answer at each step is the order and not a gate that never fires.
#[test]
#[cfg(target_os = "macos")]
fn admit_orders_its_gates() {
    use amacs::protocol::edit::Ticket;

    use crate::control_edit::hold;
    use crate::input_stall::tests::{publish_stall, raw_pty_pair};
    let (master, slave) = raw_pty_pair();
    let h = registered_session(9, master, b"");
    h.ctx.sink.note_master_nonblocking(true);
    h.ctx
        .sink
        .write_frame_nonparking(b"q")
        .expect("the person's key");
    publish_stall(&h.ctx);
    // The floor's own bucket, which no other test spends.
    let floor_key = 0xC6_0000_0001;
    let overdraw = vec![b'x'; 256 * 1024 + 1];
    let hold = |on: bool| {
        assert!(crate::fabric::apply_hold_for_test(
            &h.ctx,
            on.then(|| crate::fabric::Hold {
                reason: "demo".to_string(),
                origin: "local".to_string(),
            })
        ));
    };
    let lease = |on: bool| {
        *h.ctx.turn_lease.lock().unwrap() = on.then_some(crate::Lease::Turn {
            id: 7,
            driver: None,
            typing: false,
        });
    };
    // The edit hold is keyed by the request's session: the floor's key, which no
    // other test holds either.
    let editing = |on: bool| {
        if on {
            hold::set(
                floor_key,
                hold::Hold {
                    ticket: Ticket(61),
                    view: 6,
                },
            );
        } else {
            hold::clear(floor_key, Ticket(61));
        }
    };
    let usage = "ERR usage: id= given twice\n";
    let admit_key = |args: &[u8], usage: Option<&str>, unread_ok: bool| {
        admit(InputAdmission {
            class: input_of("key"),
            verb: "key",
            ctx: &h.ctx,
            session: floor_key,
            is_cross: false,
            args,
            unread_ok,
            usage: usage.map(str::to_string),
        })
    };
    // A request whose own words refuse, counting each time they are asked.
    let words_refusal = "ERR cause: 999999 names no record of this session\n";
    let asked = std::cell::Cell::new(0u32);
    let admit_words = || {
        admit_with(
            InputAdmission {
                class: input_of("key"),
                verb: "key",
                ctx: &h.ctx,
                session: floor_key,
                is_cross: false,
                args: b"down",
                unread_ok: false,
                usage: None,
            },
            || {
                asked.set(asked.get() + 1);
                Err::<(), _>(words_refusal.to_string())
            },
        )
        .map(|((), warning)| warning)
    };
    let halted = Err("ERR halted reason=demo origin=local\n".to_string());
    let rate = Err(RATE_REFUSAL.to_string());
    let leased = Err("ERR busy turn=7\n".to_string());
    let held = Err("ERR busy editing ticket=61 view=6\n".to_string());

    hold(true);
    lease(true);
    editing(true);
    assert_eq!(admit_key(&overdraw, Some(usage), false), halted);
    hold(false);
    assert_eq!(admit_key(&overdraw, Some(usage), false), rate);
    assert_eq!(admit_key(b"down", Some(usage), false), leased);
    lease(false);
    assert_eq!(
        admit_key(b"down", Some(usage), false),
        held,
        "the edit hold answers before the usage step"
    );
    assert_eq!(admit_words(), held, "and before the request's words");
    assert_eq!(asked.get(), 0, "which it never asks");
    editing(false);
    assert_eq!(
        admit_key(b"down", Some(usage), false),
        Err(usage.to_string())
    );
    assert_eq!(admit_words(), Err(words_refusal.to_string()));
    assert_eq!(asked.get(), 1);
    let stalled = admit_key(b"down", None, false).expect_err("the stall refuses");
    assert!(stalled.starts_with("ERR busy input-unread"), "{stalled}");
    assert_eq!(admit_key(b"down", None, true), Ok(None), "admitted");

    // Each gate alone.
    hold(true);
    assert_eq!(admit_key(b"down", None, true), halted);
    hold(false);
    assert_eq!(admit_key(&overdraw, None, true), rate);
    lease(true);
    assert_eq!(admit_key(b"down", None, true), leased);
    lease(false);
    editing(true);
    assert_eq!(admit_key(b"down", None, true), held);
    editing(false);
    assert_eq!(
        admit_key(b"down", Some(usage), true),
        Err(usage.to_string())
    );

    // The class decides: everything but the halt armed, a `signal` is admitted.
    lease(true);
    let signal = admit(InputAdmission {
        class: input_of("signal"),
        verb: "signal",
        ctx: &h.ctx,
        session: floor_key,
        is_cross: false,
        args: &overdraw,
        unread_ok: false,
        usage: None,
    });
    assert_eq!(
        signal,
        Ok(None),
        "the remedy meets no floor, lease or stall"
    );
    lease(false);
    crate::inject_floor::forget(floor_key);
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);
}

/// A REQUEST'S OWN WORDS ARE TOLD BEFORE THE UNREAD-INPUT GATE (`admit_with`
/// asking `input_words`, as `serve_session_verb` does). Into a program published
/// as not reading, a `send` whose `cause=` names nothing this session holds is
/// answered with the cause's refusal, an edge without read with `ERR denied`, and
/// a `turn` with a bad option, or a `cause=` among its options that names
/// nothing, with that refusal: never `ERR busy input-unread`. A held session
/// still says `ERR halted` first, and its words are never asked.
///
/// Negative control: the same requests with well-formed words are refused by the
/// stall, so each answer above is the order and not a gate that never fires.
#[test]
#[cfg(target_os = "macos")]
fn a_requests_own_words_are_told_before_the_unread_input_gate() {
    use crate::input_stall::tests::{publish_stall, raw_pty_pair};
    use crate::session_log::CauseRef;
    let (master, slave) = raw_pty_pair();
    let h = registered_session(9, master, b"");
    let store = session_store::new_store();
    store.write().unwrap().register(h.clone());
    // Mail this session holds, the cause a well-formed request names.
    assert!(
        crate::fabric::cmd_deliver(
            &store,
            &format!(
                "{} off=100 from=h-a kind=task trust=agent text=x",
                h.sid.as_str()
            )
        )
        .starts_with("OK")
    );
    h.ctx.sink.note_master_nonblocking(true);
    h.ctx
        .sink
        .write_frame_nonparking(b"q")
        .expect("the person's key");
    publish_stall(&h.ctx);
    let asked = std::cell::Cell::new(0u32);
    let admit_words = |scope: Scope, verb: &str, rest: &str, cause: Option<CauseRef>| {
        admit_with(
            InputAdmission {
                class: input_of(verb),
                verb,
                ctx: &h.ctx,
                session: 9,
                is_cross: true,
                args: rest.as_bytes(),
                unread_ok: false,
                usage: None,
            },
            || {
                asked.set(asked.get() + 1);
                input_words(scope, verb, rest, cause, &h.ctx)
            },
        )
        .map(|(words, _)| words.cause.map(|c| c.to_string()))
    };
    let unread = |answer: Result<Option<String>, String>| {
        answer.is_err_and(|refusal| refusal.starts_with("ERR busy input-unread"))
    };
    let no_record = Err("ERR cause: 999999 names no record of this session\n".to_string());

    assert_eq!(
        admit_words(Scope::Owner, "send", "hi", Some(CauseRef::Rec(999_999))),
        no_record
    );
    assert_eq!(
        admit_words(edge(), "send", "hi", Some(CauseRef::Mail(100))),
        Err("ERR denied\n".to_string()),
        "an edge without read learns nothing of the stall"
    );
    assert_eq!(
        admit_words(Scope::Owner, "turn", "idle=abc hi", None),
        Err(control_session::TURN_USAGE.to_string())
    );
    assert_eq!(
        admit_words(Scope::Owner, "turn", "idle=5 cause=999999 hi", None),
        no_record
    );
    assert_eq!(
        admit_words(
            Scope::Owner,
            "turn",
            "idle=5 cause=@100 hi",
            Some(CauseRef::Mail(100))
        ),
        Err(crate::session_log::CAUSE_USAGE.to_string()),
        "one leading and one among the options"
    );

    // Negative control: well-formed words meet the stall.
    assert!(unread(admit_words(
        Scope::Owner,
        "send",
        "hi",
        Some(CauseRef::Mail(100))
    )));
    assert!(unread(admit_words(Scope::Owner, "send", "hi", None)));
    assert!(unread(admit_words(Scope::Owner, "turn", "idle=5 hi", None)));
    assert!(unread(admit_words(
        Scope::Owner,
        "turn",
        "idle=5 cause=@100 hi",
        None
    )));

    // The halt is asked before the words.
    assert!(crate::fabric::apply_hold_for_test(
        &h.ctx,
        Some(crate::fabric::Hold {
            reason: "demo".to_string(),
            origin: "local".to_string(),
        })
    ));
    asked.set(0);
    assert_eq!(
        admit_words(Scope::Owner, "send", "hi", Some(CauseRef::Rec(999_999))),
        Err("ERR halted reason=demo origin=local\n".to_string())
    );
    assert_eq!(asked.get(), 0, "a halted request's words are never asked");
    assert!(crate::fabric::apply_hold_for_test(&h.ctx, None));
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);
}

/// The admission binds drivers, never the person at the glass: on a session that
/// is held AND has a published stall behind unread input, the person's `↓`
/// through the real App input seam (`Source::Human`) reaches the PTY, behind the
/// unread `q`. Negative control: a driver's `key` on that same session is refused
/// by the halt, and with the hold lifted by the stall.
#[test]
#[cfg(target_os = "macos")]
fn a_person_types_into_a_held_and_stalled_session() {
    use crate::input::Source;
    use crate::input_stall::tests::{publish_stall, raw_pty_pair, slave_read_all};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let (master, slave) = raw_pty_pair();
    let sink = Arc::new(SinkWriter::new(master));
    sink.note_master_nonblocking(true);
    let mut app = crate::App::headless_for_test_with_sink(sink);
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    ctx.sink
        .write_frame_nonparking(b"q")
        .expect("the person's earlier key");
    publish_stall(&ctx);
    assert!(crate::fabric::apply_hold_for_test(
        &ctx,
        Some(crate::fabric::Hold {
            reason: "demo".to_string(),
            origin: "fleet".to_string(),
        })
    ));
    let driver = |ctx: &SessionCtx| {
        admit(InputAdmission {
            class: input_of("key"),
            verb: "key",
            ctx,
            session: 0,
            is_cross: true,
            args: b"down",
            unread_ok: false,
            usage: None,
        })
    };
    assert_eq!(
        driver(&ctx),
        Err("ERR halted reason=demo origin=fleet\n".to_string())
    );

    let down = InputEvent::Key {
        key: Key::Named(NamedKey::ArrowDown),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let _ = app.input(crate::WindowId(0), down, Source::Human);
    assert_eq!(
        slave_read_all(slave),
        b"q\x1b[B",
        "the person's arrow reached the PTY behind the unread key"
    );

    assert!(crate::fabric::apply_hold_for_test(&ctx, None));
    ctx.sink
        .write_frame_nonparking(b"q")
        .expect("unread input again");
    publish_stall(&ctx);
    let stalled = driver(&ctx).expect_err("the stall refuses the driver");
    assert!(stalled.starts_with("ERR busy input-unread"), "{stalled}");
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);
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

    // The dispatch's admission, then its claim around the write.
    let key = format!("{}:4:1", h.ctx.nonce.to_hex());
    let attempt = || match admit(InputAdmission {
        class: input_of("key"),
        verb: "key",
        ctx: &h.ctx,
        session: 9,
        is_cross: true,
        args: b"down",
        unread_ok: false,
        usage: None,
    }) {
        Err(refusal) => refusal,
        Ok(_) => {
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
    // A local hold: the halt, asked first, is the answer.
    assert!(crate::fabric::apply_hold_for_test(
        &h.ctx,
        Some(crate::fabric::Hold {
            reason: "demo".to_string(),
            origin: "local".to_string(),
        })
    ));
    assert_eq!(attempt(), "ERR halted reason=demo origin=local\n");
    assert!(crate::fabric::apply_hold_for_test(&h.ctx, None));

    // The refused attempt never reached the claim, so its key is unspent.
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
/// `@<sid> tab …` drives `hosting_window(sid)`, so `tab close N` retires whatever
/// session tab N hosts. Gated only on the aimed session's hold, a partial halt
/// (`bridge_lost` halts only `LINK.touched` sids) would let `@<unheld> tab close N`
/// retire a held sibling that `@<held> close` and bare `tab close` refuse. So
/// [`app_verb_halt`] asks the aimed session first, which a held aim answers with
/// its own `reason=`, and then the App lane, which every App verb passes.
#[test]
fn an_aimed_app_verb_asks_the_app_lane_halt_question_inbox_hold() {
    crate::fabric::with_link_reset(an_aimed_app_verb_asks_the_app_lane_halt_question_body);
}

fn an_aimed_app_verb_asks_the_app_lane_halt_question_body() {
    let store = session_store::new_store();
    let handles = [1, 2, 3].map(crate::session_store::test_handle);
    let aims = handles
        .each_ref()
        .map(|h| (h.term.clone(), h.master, h.local_id, h.ctx.clone()));
    for handle in handles {
        store.write().unwrap().register(handle);
    }
    let sid = |n: usize| aims[n].3.self_id.as_str().to_string();
    for (n, reason) in [(0, "first"), (1, "second")] {
        assert_eq!(
            dispatch_hold_verb(
                &format!("{} on reason={reason} origin=fleet", sid(n)),
                None,
                Scope::Bridge,
                &store,
            ),
            "OK hold=1\n",
        );
    }
    let unheld = AppAim::Aimed(aims[2].clone());

    // The session gate alone says the request may proceed, because the sid it
    // names is not the sid it would retire.
    assert!(
        crate::fabric::halt_refusal(&aims[2].3, "tab").is_none(),
        "the unheld window-mate is not itself held: this is precisely why the \
         session gate is the wrong question for a verb that drives a window",
    );
    // The APP-lane gate refuses it, aimed or not.
    let lane = crate::fabric::app_halt_refusal(&store, "tab").expect("a session is held");
    assert_eq!(
        app_verb_halt(&unheld, &store, input_of("tab"), "tab", ""),
        Some(lane.clone())
    );
    assert_eq!(
        app_verb_halt(&AppAim::Front, &store, input_of("tab"), "tab", ""),
        Some(lane.clone())
    );
    // A held aim answers with its OWN hold, though the App lane would have named
    // whichever held session it found first (the negative control: one of the two
    // differs from the lane's line).
    let mut own_differs = false;
    for aim in &aims[..2] {
        let own = crate::fabric::halt_refusal(&aim.3, "tab").expect("held");
        own_differs |= own != lane;
        assert_eq!(
            app_verb_halt(
                &AppAim::Aimed(aim.clone()),
                &store,
                input_of("tab"),
                "tab",
                ""
            ),
            Some(own)
        );
    }
    assert!(
        own_differs,
        "two holds, two reasons: the lane names only one"
    );
    // `spawn` rides the same lane and is a named residual of the halt set (it mints
    // a session rather than reaching one; its row argues it), so neither gate
    // sweeps it in.
    assert!(crate::fabric::app_halt_refusal(&store, "spawn").is_none());
    assert!(
        app_verb_halt(
            &AppAim::Aimed(aims[0].clone()),
            &store,
            input_of("spawn"),
            "spawn",
            ""
        )
        .is_none()
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
        ),
        "OK 1\n"
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
/// would desync the stream). Only a row whose body is [`Body::PostLen`] is read for
/// one, and the lanes wait for exactly the body it announces.
#[test]
fn the_post_frame_detector_agrees_with_the_body_rule_inbox_hold() {
    let frame = |line: &str| Request::parse(line.to_string()).post_frame();
    assert_eq!(frame("post to=h-a kind=note len=12"), Some(Ok(12)));
    assert_eq!(frame("@s-a post to=h-a kind=ask --wait len=3"), Some(Ok(3)));
    assert_eq!(
        frame("post to=h-a kind=note hello len=12"),
        None,
        "a `len=` inside the BODY is not a frame"
    );
    assert_eq!(frame("post to=h-a kind=note hi"), None);
    // After `--` everything is body: a `len=` there announces no frame.
    assert_eq!(frame("post to=h-a kind=note -- len=12"), None);
    // Negative control: the same options on a row with no post body.
    assert_eq!(frame("send len=4"), None);
    assert_eq!(frame("feed-bin len=4"), None);
    let refused = |line: &str| match frame(line) {
        Some(Err(refusal)) => refusal,
        other => panic!("{line}: {other:?}"),
    };
    assert!(refused("post to=h-a kind=note len=99999999").starts_with("ERR too large"));
    assert!(
        refused("post to=h-a kind=note len=abc").starts_with("ERR usage"),
        "a typo is not a size problem"
    );
    // The lanes' wholeness test reads the same rule off the raw line: a refused
    // length announces no body (the request is answered and the connection closed).
    for (line, body) in [
        ("post to=h-a kind=note len=12", 12),
        ("post to=h-a kind=note hello len=12", 0),
        ("post to=h-a kind=note len=abc", 0),
        ("send len=4", 0),
    ] {
        assert_eq!(announced_body_of(line), body, "{line}");
        assert_eq!(Request::parse(line.to_string()).body_len(), body, "{line}");
    }
}

/// A WORKER's `spawn … len=<n>` frame is detected with the handler's own
/// tokenizer: a `len=` inside a quoted `cwd=` is no frame, a quoted one is,
/// and a line the tokenizer refuses still frames any `len=` it holds (its
/// bytes are consumed, never read as verbs). Past the 64 KiB cap, or not a
/// length: answered unread, and the serve loop closes. The lanes wait for
/// exactly that many body bytes (`announced_body`).
#[test]
fn the_worker_spawn_frame_detector_agrees_with_the_spawn_tokenizer() {
    assert_eq!(
        spawn_frame_len(&control_request::head("spawn agent=claude len=12")),
        Some(Ok(12))
    );
    assert_eq!(
        spawn_frame_len(&control_request::head(
            "@s-ab spawn cwd=/w agent=claude model=opus len=0"
        )),
        Some(Ok(0))
    );
    assert_eq!(
        spawn_frame_len(&control_request::head(
            "spawn cwd=\"/a len=9\" agent=claude"
        )),
        None,
        "a `len=` inside a quoted value is no frame"
    );
    assert_eq!(
        spawn_frame_len(&control_request::head("spawn \"len=7\" agent=claude")),
        Some(Ok(7))
    );
    assert_eq!(
        spawn_frame_len(&control_request::head("spawn cwd=\"/a agent=claude len=3")),
        Some(Ok(3)),
        "an unterminated quote still frames its len="
    );
    assert_eq!(
        spawn_frame_len(&control_request::head("spawn cwd=/w")),
        None
    );
    assert_eq!(
        spawn_frame_len(&control_request::head("post to=h-a kind=note len=3")),
        None
    );
    assert_eq!(spawn_frame_len(&control_request::head("send len=4")), None);
    assert_eq!(
        spawn_frame_len(&control_request::head("spawn agent=claude len=65537")),
        Some(Err(
            "ERR spawn agent=claude: the task is 65537 bytes (max 65536)\n".to_string()
        ))
    );
    assert_eq!(
        spawn_frame_len(&control_request::head("spawn agent=muse len=65537")),
        Some(Err(
            "ERR spawn agent=muse: the task is 65537 bytes (max 65536)\n".to_string()
        )),
        "named by the spawn's own worker word"
    );
    assert!(
        spawn_frame_len(&control_request::head("spawn agent=claude len=abc"))
            .is_some_and(|len| len.is_err_and(|e| e.starts_with("ERR usage: spawn")))
    );
    assert_eq!(announced_body_of("spawn agent=claude len=65536"), 65536);
    assert_eq!(announced_body_of("spawn agent=claude len=65537"), 0);
    assert_eq!(announced_body_of("spawn cwd=/w"), 0);
}

/// A worker's task is read only behind a `GO <n>` the client waits for: a
/// lane reading its connection waiting answers the line first, and the task
/// follows; a task already here (a parked connection's line had its `GO`
/// from `frame_header`, and was served once its task was whole) is read with
/// no second answer.
#[cfg(unix)]
#[test]
fn a_workers_task_is_read_behind_its_go() {
    use std::io::{BufRead as _, Read as _};
    let (server, client) = std::os::unix::net::UnixStream::pair().expect("pair");
    let task = b"line one\n$(two)\n".to_vec();
    let peer = {
        let task = task.clone();
        std::thread::spawn(move || {
            let mut r = std::io::BufReader::new(&client);
            let mut go = String::new();
            r.read_line(&mut go).expect("the GO");
            (&client).write_all(&task).expect("the task");
            go
        })
    };
    let mut unread = control_lanes::Unread::default();
    let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
    assert_eq!(
        read_worker_task(&server, &mut reader, task.len()).expect("read"),
        task
    );
    assert_eq!(
        peer.join().expect("peer"),
        aterm_types::control_verbs::task_frame_go(task.len())
    );

    let (server, mut client) = std::os::unix::net::UnixStream::pair().expect("pair");
    let mut here = task.clone();
    here.extend_from_slice(b"next\n");
    {
        let mut unread = control_lanes::Unread::holding(here);
        let mut reader = control_lanes::LaneReader::new(&server, &mut unread);
        assert_eq!(
            read_worker_task(&server, &mut reader, task.len()).expect("read"),
            task
        );
        assert_eq!(
            read_request_line(&mut reader).as_deref(),
            Some("next"),
            "the stream stays framed"
        );
    }
    drop(server);
    let mut said = Vec::new();
    client
        .read_to_end(&mut said)
        .expect("the server's end closed");
    assert!(said.is_empty(), "no second answer: {said:?}");
}

/// A worker's spawn frame aimed at a session this instance does not host is
/// refused unsent where it is, never relayed: the `GO` it waits for is this
/// instance's to say, and another instance — one that predates the frame —
/// would serve its task as requests. Aimed here (no selector, `@.`, a
/// local id, a hosted sid), or no frame at all, it is not refused here.
#[test]
fn a_worker_frame_for_another_instance_is_refused_never_relayed() {
    let store = session_store::new_store();
    let local = registered_session(0, -1, b"");
    let local_sid = local.ctx.self_id.as_str().to_string();
    store.write().unwrap().register(local);
    let refused = worker_frame_refusal(
        &control_request::head("@s-0ffcafe spawn agent=claude len=4"),
        &store,
    );
    assert!(
        refused.as_deref().is_some_and(|r| r
            .starts_with("ERR spawn agent=claude: @s-0ffcafe is not a session of this instance")),
        "{refused:?}"
    );
    let gemini = worker_frame_refusal(
        &control_request::head("@s-0ffcafe spawn agent=gemini len=4"),
        &store,
    );
    assert!(
        gemini.as_deref().is_some_and(|r| r
            .starts_with("ERR spawn agent=gemini: @s-0ffcafe is not a session of this instance")),
        "{gemini:?}"
    );
    assert!(!request_waits(
        "@s-0ffcafe spawn agent=claude len=4",
        Scope::Owner,
        &store
    ));
    assert!(
        request_waits("@s-0ffcafe spawn cwd=/w", Scope::Owner, &store),
        "a plain spawn is relayed as before"
    );
    for here in [
        "spawn agent=claude len=4".to_string(),
        "@. spawn agent=claude len=4".to_string(),
        "@0 spawn agent=claude len=4".to_string(),
        format!("@{local_sid} spawn agent=claude len=4"),
        "@s-0ffcafe spawn cwd=/w".to_string(),
        "@s-0ffcafe text".to_string(),
    ] {
        assert_eq!(
            worker_frame_refusal(&control_request::head(&here), &store),
            None,
            "{here}"
        );
    }
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
    let mut out = Vec::new();
    assert!(
        feed_bin_routed(
            line,
            &mut reader,
            FeedBinRoute {
                active,
                store,
                scope: Scope::Owner,
            },
            &mut dispatch,
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
    assert_eq!(&*doubt[0].payload, "producer=4 seq=1 reply=ERR-write");
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
    assert!(
        !aterm_types::control_verbs::input_of("paste")
            .contains(aterm_types::control_verbs::Input::KEYED)
    );

    // The dispatch's scan takes nothing: the tail is the whole line.
    let (leading, rest) = control_input::take_leading_options(input_of("paste"), &line);
    assert_eq!(leading.idem, None);
    assert!(leading.refusal.is_none());
    assert_eq!(rest, line);

    // The `paste` arm's event, delivered through the real sink: the key
    // lands on the PTY as text, byte for byte, and the `\n` as CR.
    assert_eq!(
        background(
            "paste",
            &h.term,
            &h.ctx,
            InputEvent::Paste(paste_text(rest), PasteFraming::AtDrain),
        ),
        Ok(InputOutcome::Ok)
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
    // The sink borrows the test's write end; this scope owns and closes it.
    // SAFETY: guarded_pipe_session returned this new write descriptor; the
    // session/sink borrow it and no other owner closes it. No writer is pending.
    let _write_end = unsafe { std::os::fd::OwnedFd::from_raw_fd(h.master) };
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
        stamp_input_seq(input_of("key"), "OK skipped\n".to_string(), &h.term),
        format!("OK skipped seq={seq}\n")
    );
    assert_eq!(
        stamp_input_seq(input_of("key"), "OK\n".to_string(), &h.term),
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

    // The actual worker launch admission uses this same guarded pipe. No
    // shell/program is run, and no timed race is needed to interpose a take
    // over or changed input between the observed prompt and final write.
    let store = session_store::new_store();
    store.write().unwrap().register(h.clone());
    let observe = || {
        crate::term_lock(&h.term).process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        crate::task_launch::prompt_fence(&crate::term_lock(&h.term), h.ctx.human_input.seq())
            .expect("an input-ready shell prompt")
    };
    let launch =
        |prompt| crate::task_launch::type_launch_line(&store, &h.sid, h.nonce, prompt, "worker");
    let observed = observe();
    assert_eq!(launch(observed), Ok("written whole"));
    assert_eq!(
        drain_pipe(&rx),
        b"worker\r",
        "the unchanged prompt admits one command"
    );

    let observed = observe();
    crate::fabric::person_hold(&*h.ctx, true, &|| {});
    assert!(
        launch(observed).is_err(),
        "Take Over after the prompt refuses the command"
    );
    assert!(drain_pipe(&rx).is_empty());
    crate::fabric::person_hold(&*h.ctx, false, &|| {});
    assert_eq!(
        launch(observed),
        Ok("written whole"),
        "a fresh explicit admission after hand-back"
    );
    assert_eq!(drain_pipe(&rx), b"worker\r");

    let observed = observe();
    h.ctx
        .human_input
        .note(crate::metrics::now_us(), crate::human_input::Gesture::Other);
    assert!(
        launch(observed).is_err(),
        "a person's key need not have echoed yet"
    );
    assert!(drain_pipe(&rx).is_empty());

    let observed = observe();
    crate::term_lock(&h.term).process(b"changed");
    assert!(launch(observed).is_err(), "output replaced the observation");
    assert!(drain_pipe(&rx).is_empty());

    let observed = observe();
    crate::term_lock(&h.term).process(b"\x1b]133;C\x07");
    assert!(
        launch(observed).is_err(),
        "a command already started on the prompt"
    );
    assert!(drain_pipe(&rx).is_empty());

    let observed = observe();
    h.ctx.update_parked.park_for_test();
    assert!(launch(observed).is_err());
    assert!(drain_pipe(&rx).is_empty());
    h.ctx.update_parked.resume_for_test();

    assert!(
        crate::task_launch::type_launch_line(
            &store,
            &h.sid,
            LaunchNonce::generate(),
            observed,
            "worker"
        )
        .is_err(),
        "a different launch nonce has no authority"
    );
    assert!(drain_pipe(&rx).is_empty());
    store
        .write()
        .unwrap()
        .set_state(h.local_id, SessionState::Exited);
    assert!(
        launch(observed).is_err(),
        "a dead session remains readable but cannot launch"
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
    let (key_first, guard_first) = (
        format!("id={key} if=Do.you.want 1"),
        format!("if=Do.you.want id={key} 1"),
    );
    let (a, rest_a) = control_input::take_leading_options(input_of("key"), &key_first);
    let (b, rest_b) = control_input::take_leading_options(input_of("key"), &guard_first);
    assert_eq!(a, b);
    assert_eq!((rest_a, rest_b), ("1", "1"));
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
        line.contains(" attention_owner=human attention_owners=2 attention_kinds=-:sup,-:human "),
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

/// THE ATTENTION KIND'S GRAMMAR (`meta set attention owner=<k> [kind=ask|wait]
/// <text>`, `docs/DESIGN-owner-surfaces-2026-10-16.md` §2.4): `kind=` follows
/// `owner=` and names one of the closed set; a bare entry is an ask and takes
/// no `kind=`; the text after the kind is the entry's, unchanged; a kind that
/// moves alone is a change; every refusal stores nothing.
#[test]
fn the_attention_kind_rides_a_keyed_entry_and_a_refusal_stores_nothing() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let meta = |rest: &str| cmd_meta(&h.term, &store, 0, &h.ctx, rest);
    let field = |key: &str| {
        let line = meta("").0;
        line.split_whitespace()
            .find_map(|kv| kv.strip_prefix(&format!("{key}=")).map(str::to_string))
    };
    // `owner`'s entry: its text, and its kind as `attention_kinds=` says it.
    let kinds = |owner: &str| {
        let text = h
            .ctx
            .meta
            .lock()
            .unwrap()
            .attention_owners
            .get(owner)?
            .to_string();
        let listed = field("attention_kinds").unwrap_or_default();
        let kind = listed
            .split(',')
            .filter_map(|pair| pair.split_once(':'))
            .find(|(_, o)| *o == owner)
            .and_then(|(kind, _)| aterm_types::needs::AttentionKind::parse(kind));
        Some((text, kind))
    };
    use aterm_types::needs::AttentionKind::{self, Ask, Wait};
    // `sup`'s entry after a request: its text and its kind.
    type Left = Option<(&'static str, Option<AttentionKind>)>;
    // (request, accepted, the entry left for owner `sup`)
    let table: &[(&str, bool, Left)] = &[
        (
            "set attention owner=sup kind=wait limited: resets 7pm",
            true,
            Some(("limited: resets 7pm", Some(Wait))),
        ),
        // The same text, the kind moved alone: a change, and the text stays.
        (
            "set attention owner=sup kind=ask limited: resets 7pm",
            true,
            Some(("limited: resets 7pm", Some(Ask))),
        ),
        // No kind: the writer named none.
        ("set attention owner=sup a box", true, Some(("a box", None))),
        // A kind outside the closed set, or with no text, is refused.
        (
            "set attention owner=sup kind=control a box",
            false,
            Some(("a box", None)),
        ),
        (
            "set attention owner=sup kind=wait",
            false,
            Some(("a box", None)),
        ),
        (
            "set attention owner=sup kind= a box",
            false,
            Some(("a box", None)),
        ),
        ("unset attention owner=sup", true, None),
    ];
    for (request, accepted, left) in table {
        let (reply, _) = meta(request);
        assert_eq!(reply.starts_with("OK"), *accepted, "{request}: {reply}");
        assert_eq!(
            kinds("sup"),
            left.map(|(text, kind)| (text.to_string(), kind)),
            "{request}"
        );
    }
    // The kind moving alone is news (the session's need moves).
    meta("set attention owner=sup kind=wait limited: x");
    assert_eq!(
        meta("set attention owner=sup kind=wait limited: x"),
        ("OK\n".to_string(), false)
    );
    assert_eq!(
        meta("set attention owner=sup kind=ask limited: x"),
        ("OK\n".to_string(), true)
    );
    // A bare entry is an ask, and a bare `kind=` is a usage error, not text.
    assert!(
        meta("set attention kind=wait limited: x")
            .0
            .starts_with("ERR usage")
    );
    assert_eq!(kinds("-"), None);
    assert_eq!(meta("set attention lunch").0, "OK\n");
    assert_eq!(kinds("-"), Some(("lunch".to_string(), Some(Ask))));
    assert_eq!(field("attention_kinds").as_deref(), Some("ask:sup,ask:-"));
    meta("unset attention");
    meta("unset attention owner=sup");
    assert_eq!(field("attention_kinds").as_deref(), Some("-"));
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
    // the owner's columns (`path_evidence= copy= upgrade=`), the harness's
    // `land_at= task= pause=` and the task record's `task_state= task_rev=`
    // follow it.
    let row = || {
        let body = control_session::cmd_sessions_store(&store, None);
        let line = body.lines().nth(1).expect("one row");
        let (head, owner) = line.split_once(" path_evidence=").expect("owner columns");
        assert!(
            owner.ends_with(" copy=- upgrade=- land_at=- task=- pause=- task_state=- task_rev=0"),
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
    release_connection_claims(&store, None, 40);
    assert!(row().ends_with(" supervisor=sup-a"));
    release_connection_claims(&store, None, 41);
    assert!(row().ends_with(" supervisor=-"), "{}", row());

    // A `ttl=` claim is not bound to the connection that set it.
    assert_eq!(
        sup(Scope::Owner, Some(43), "set supervisor sup-b ttl=60000"),
        Some(("OK\n".to_string(), true))
    );
    release_connection_claims(&store, None, 43);
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
        tl.slog_mut().open_session(None, None, now);
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
    let sid = Selector::parse(ctx.self_id.as_str());
    let local = local_of(&app, &ctx);
    assert_eq!(
        closed_session_reply(Some(&sid), "log", "", &app.store, Scope::Owner),
        "ERR no such session\n",
        "CONTROL: nothing kept while it lives"
    );
    app.retire_session_registration(local);
    let at_close = super::control_session::cmd_log(&ctx, "");
    assert!(at_close.contains(" close - "), "{at_close}");
    for sel in [
        Selector::parse(ctx.self_id.as_str()),
        Selector::Local(local),
    ] {
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
            Some(&Selector::parse(
                aterm_session::SessionId::generate().as_str()
            )),
            "log",
            "",
            &app.store,
            Scope::Owner
        ),
        "ERR no such session\n"
    );
}

/// A CLOSE WAKES ITS WATCHERS ONLY ONCE ITS LOG IS KEPT. A watcher on the
/// session's list and the instance's holds the session's edge table, which the
/// shipping retire takes between its deregister and the closed-session entry's
/// link: once the store no longer lists the session, neither list has rung (a
/// ring there wakes a reader into `ERR no such session`). Released, the close
/// rings, and the reader it wakes is answered from the closed index.
#[test]
fn a_close_wakes_its_watchers_only_once_its_log_is_kept() {
    use std::sync::atomic::Ordering;

    use super::{Scope, Selector, closed_session_reply};
    let (mut app, ctx, _) = closed_session_fixture(&[]);
    let local = local_of(&app, &ctx);
    let store = app.store.clone();
    let lists = [
        std::sync::Arc::clone(&ctx.watchers),
        store.read().unwrap().instance_watchers(),
    ];
    let (edges, sid) = (std::sync::Arc::clone(&ctx.edges), ctx.self_id.clone());
    let (armed, ready) = std::sync::mpsc::channel();
    let watcher = std::thread::spawn(move || {
        let mut sub = crate::session_watch::Subscription::new();
        let flags: Vec<_> = lists.iter().map(|list| sub.watch(list)).collect();
        for flag in &flags {
            flag.store(false, Ordering::Release);
        }
        let between = edges.lock().unwrap();
        armed.send(()).expect("the test waits for the watcher");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the close to deregister the session",
            || store.read().unwrap().by_sid(&sid).is_none().then_some(()),
        );
        let rang: Vec<bool> = flags.iter().map(|f| f.load(Ordering::Acquire)).collect();
        drop(between);
        assert!(
            sub.wait(aterm_spec::wait::MUST_HAPPEN),
            "the close rings its lists"
        );
        let sel = Selector::parse(sid.as_str());
        let log = closed_session_reply(Some(&sel), "log", "", &store, Scope::Owner);
        (rang, log)
    });
    ready.recv().expect("the watcher is armed");
    app.retire_session_registration(local);
    let (rang, log) =
        crate::test_socket::join_after_hang_up(watcher, "the woken watcher to read the log");
    assert_eq!(
        rang,
        [false, false],
        "no list rang between the deregister and the link"
    );
    assert!(
        log.contains(" close - "),
        "the woken reader reads the kept log: {log}"
    );
}

/// `LOG N` IS THE OWNER'S (session log 5J-3): the instance's node log answers an
/// Owner-class connection with no terminal; an edge, granted or not, is
/// refused (`ERR denied`, audited), and so is a selector (node `n` is no
/// session's). NEGATIVE CONTROL: the same edge's session `log` is no
/// escalation, and `brief` is unchanged.
#[test]
fn log_n_is_the_owners_and_names_no_session() {
    use super::{Scope, Selector, node_log_reply};
    let ctx = test_ctx();
    let read_edge = edge_granted(Op::ReadScreen, &ctx);
    let owner = node_log_reply("n", None, Scope::Owner);
    assert!(owner.starts_with("OK "), "{owner}");
    assert!(
        owner
            .lines()
            .skip(1)
            .all(|l| l.split(' ').nth(2) == Some("n") || l.starts_with("gap ")),
        "node n's records only: {owner}"
    );
    assert!(node_log_reply("n", Some(&Selector::SelfTok), Scope::Owner).starts_with("OK "));
    for scope in [read_edge, Scope::Edge(aterm_session::EdgeToken::generate())] {
        assert_eq!(
            node_log_reply("n", None, scope),
            "ERR denied: log n is owner-only\n"
        );
    }
    assert_eq!(
        node_log_reply("n", Some(&Selector::Local(3)), Scope::Owner),
        "ERR denied: log n takes no @<sid>\n"
    );
    assert_eq!(
        escalation("log", ""),
        None,
        "NEGATIVE CONTROL: a session's log"
    );
    assert_eq!(escalation("brief", ""), None, "brief is unchanged");
    assert_eq!(escalation("brief", "all"), Some(Need::OwnerOnly));
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
    let sid = Selector::parse(ctx.self_id.as_str());
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
    let sid = Selector::parse(ctx.self_id.as_str());
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
    let sid = Selector::parse(ctx.self_id.as_str());
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
        let selector = Selector::parse(dst.as_str());
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
        let selector = Selector::parse(ctx.self_id.as_str());
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
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    let _override = crate::identity_claim::rendezvous_override(dir.path());
    let (mut app, ctx, _) = closed_session_fixture(&[]);
    let local = local_of(&app, &ctx);
    app.retire_session_registration(local);
    crate::identity_claim::note_adopted(&ctx.self_id);
    let sid = Selector::parse(ctx.self_id.as_str());
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
    let sel = Selector::parse(sid.as_str());
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
    let unknown_sid = aterm_session::SessionId::generate();
    let unknown = Selector::parse(unknown_sid.as_str());
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
    let sel = Selector::parse(ctx.self_id.as_str());
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
    let (o, tail) = control_input::take_leading_options(input_of("send"), "cause=@5 id=a:b:c hi");
    assert_eq!(
        (o.cause, o.idem.as_deref(), tail),
        (Some(CauseRef::Mail(5)), Some("a:b:c"), "hi")
    );
    let (o, tail) =
        control_input::take_leading_options(input_of("send"), "id=a:b:c cause=12 if=x hi");
    assert_eq!((o.cause, tail), (Some(CauseRef::Rec(12)), "hi"));
    let (o, tail) = control_input::take_leading_options(input_of("turn"), "cause=7 idle=5 hi");
    assert_eq!((o.cause, tail), (Some(CauseRef::Rec(7)), "idle=5 hi"));
    for bad in [
        "cause=@",
        "cause=0",
        "cause=x",
        "cause=-1",
        "cause=@5 cause=6",
        "cause=@1x",
    ] {
        let (o, _) = control_input::take_leading_options(input_of("send"), &format!("{bad} hi"));
        assert_eq!(
            o.refusal.as_deref(),
            Some(crate::session_log::CAUSE_USAGE),
            "{bad}"
        );
    }
    let (o, tail) = control_input::take_leading_options(input_of("key"), "cause=7 enter");
    assert_eq!((o.cause, tail), (None, "cause=7 enter"), "key takes none");
}

/// `act=` (phase 5, ruling T1): taken among `key`'s and `send`'s leading
/// options in any order, once, a closed word; a malformed or repeated one is
/// a carried usage refusal (answered before a byte); `paste` and `turn` take
/// none (their bodies keep it).
#[test]
fn act_is_taken_among_key_and_send_leading_options_once() {
    use aterm_slog::ActWord;
    let (o, tail) = control_input::take_leading_options(input_of("key"), "act=approve 1");
    assert_eq!(
        (o.act, o.refusal.as_deref(), tail),
        (Some(ActWord::Approve), None, "1")
    );
    let (o, tail) =
        control_input::take_leading_options(input_of("send"), "id=a:b:c act=answer cause=@5 hi");
    assert_eq!(
        (o.act, o.idem.as_deref(), tail),
        (Some(ActWord::Answer), Some("a:b:c"), "hi")
    );
    let (o, tail) = control_input::take_leading_options(input_of("key"), "if-gen=1.2 act=deny 2");
    assert_eq!((o.act, tail), (Some(ActWord::Deny), "2"));
    for bad in [
        "act=",
        "act=yes",
        "act=Approve",
        "act=unknown",
        "act=approve act=approve",
        "act=approve act=deny",
    ] {
        for verb in ["key", "send"] {
            let (o, _) = control_input::take_leading_options(input_of(verb), &format!("{bad} 1"));
            assert_eq!(
                o.refusal.as_deref(),
                Some(control_input::ACT_USAGE),
                "{verb} {bad}"
            );
        }
    }
    for verb in ["paste", "turn"] {
        let (o, tail) = control_input::take_leading_options(input_of(verb), "act=approve hi");
        assert_eq!((o.act, tail), (None, "act=approve hi"), "{verb}");
    }
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
    let _ = h.ctx.timeline.lock().unwrap().slog_take_wake();
    let sub = crate::session_watch::Subscription::single(&h.ctx.watchers);
    let mark = crate::control_input_ack::InputMark::at_epoch(
        &h.ctx,
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
pub(super) fn with_open_approval(h: &SessionHandle) -> aterm_slog::Node {
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
#[cfg(unix)]
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
    let step = with_open_approval(&h);
    let (edge, src) = edge_to(&h, aterm_session::Op::WriteInput);
    let act = format!("{step} act word=approve rule=r1 re=1");
    for (scope, harness, by) in [
        (Scope::Owner, false, "ctl".to_string()),
        (Scope::Owner, true, "harness".to_string()),
    ] {
        let _principal = harness.then(|| ServingPrincipal::enter(Principal::Harness));
        let reply = dispatch_mark(&h.ctx, scope, &act);
        assert!(
            reply.starts_with("OK seq=") && reply.ends_with(&format!(" node={step} by={by}\n")),
            "{reply}"
        );
    }
    // The edge, which may not read the session, marks the deepest open node
    // (the approval's step) and is told nothing of it.
    let reply = dispatch_mark(&h.ctx, edge, "- act word=approve rule=r1");
    assert_eq!(reply, format!("OK by={}\n", src.as_str()));
    // The positive control, through the write a `send` makes to a background
    // session.
    assert_eq!(
        background(
            "send",
            &h.term,
            &h.ctx,
            InputEvent::KeySequence(b"Z".to_vec())
        ),
        Ok(InputOutcome::Ok),
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
    let reply = dispatch_mark(&h.ctx, Scope::Owner, "- verdict word=done as=reviewer");
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
            "- label text=trust%20me as=harness",
        ),
        "ERR mark: as=harness is a session log actor\n"
    );
    let reply = dispatch_mark(
        &h.ctx,
        Scope::Edge(forged),
        "- label text=trust%20me as=bot",
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
            dispatch_mark(&h.ctx, Scope::Edge(writer), &probe),
            "ERR denied\n",
            "{probe}"
        );
    }
    let reply = dispatch_mark(&h.ctx, Scope::Edge(writer), "- note text=x");
    assert_eq!(reply, format!("OK by={}\n", source.as_str()));
    // NEGATIVE CONTROL: the owner may read, and is told.
    let reply = dispatch_mark(&h.ctx, Scope::Owner, &format!("{step} note text=y re=1"));
    assert!(
        reply.starts_with("OK seq=") && reply.contains(&format!(" node={step} by=ctl")),
        "{reply}"
    );
    let reply = dispatch_mark(&h.ctx, Scope::Owner, "r999 note text=z");
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
    let step = with_open_approval(&h);
    let reply = dispatch_mark(&h.ctx, Scope::Owner, "- note text=x");
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
        dispatch_mark(&h.ctx, Scope::Owner, &format!("{step} act word=approve"),),
        format!("ERR mark: {step} is not open\n")
    );
    assert_eq!(
        dispatch_mark(&h.ctx, Scope::Owner, "p999 note text=x"),
        "ERR mark: no node p999\n"
    );
}

/// One claim a second with bursts of twenty per author: the twenty-first is
/// `ERR busy retry_ms=`, a second later one more lands, and the harness's
/// bucket is untouched by `ctl`'s spending.
#[test]
fn the_twenty_first_mark_is_busy() {
    let h = registered_session(1, -1, b"");
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
        let reply = dispatch_mark(&h.ctx, Scope::Owner, &format!("- note text=n{i}"));
        assert!(reply.starts_with("OK "), "{i}: {reply}");
    }
    let busy = dispatch_mark(&h.ctx, Scope::Owner, "- note text=more");
    assert!(busy.starts_with("ERR busy retry_ms="), "{busy}");
    {
        let _principal = ServingPrincipal::enter(Principal::Harness);
        let reply = dispatch_mark(&h.ctx, Scope::Owner, "- verdict word=on-track");
        assert!(
            reply.ends_with(" by=harness\n"),
            "the harness's own bucket: {reply}"
        );
    }
    ms.fetch_add(1_000, std::sync::atomic::Ordering::SeqCst);
    let again = dispatch_mark(&h.ctx, Scope::Owner, "- note text=later");
    assert!(again.starts_with("OK "), "{again}");
}

/// A claim's need ranks last, and its `clear` lifts only claims: a failed
/// command's `need=failure` survives `mark - clear` from an Owner client
/// (design §3.7; the 2A rule let any in-session agent lift it).
#[test]
fn a_marks_need_ranks_last_and_its_clear_lifts_only_claims() {
    let h = registered_session(1, -1, b"");
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
    assert!(dispatch_mark(&h.ctx, Scope::Owner, "- need word=help").starts_with("OK "));
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
    assert!(dispatch_mark(&h.ctx, Scope::Owner, "- clear").starts_with("OK "));
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
    assert!(dispatch_mark(&h.ctx, Scope::Owner, "- note text=held").starts_with("OK "));
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
fn await_log(h: &SessionHandle, store: &Store, args: &str) -> String {
    control_session::cmd_await(&h.term, store, h.local_id, &h.ctx, &format!("log {args}"))
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
    let step = with_open_approval(&h);
    carried_across_an_update(&h);
    for call in 0..2 {
        let (tx, rx) = std::sync::mpsc::channel();
        let (h2, store2) = (h.clone(), store.clone());
        std::thread::spawn(move || {
            let _ = tx.send(await_log(&h2, &store2, "need timeout=600000"));
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
    assert!(dispatch_mark(&h3.ctx, Scope::Owner, "- need word=help").starts_with("OK "));
    assert_eq!(await_log(&h3, &store, "need timeout=0"), "OK timeout\n");
}

/// R2: a wait that crosses a loss with no match answers the gap, its
/// `resync_to` the read's own `next`; re-armed there it does not meet the same
/// gap. With a match past a loss, it answers the match and the gap before it.
#[test]
fn a_rearm_at_resync_to_never_returns_the_same_gap() {
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
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
    let reply = await_log(&h, &store, &format!("kind=note since=1 ep={ep} timeout=0"));
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
            &format!("kind=note since={resync} ep={ep} timeout=0")
        ),
        "OK timeout\n",
        "re-armed at resync_to: no gap again"
    );
    // A match past the loss carries the gap before it.
    assert!(dispatch_mark(&h.ctx, Scope::Owner, "- note text=here").starts_with("OK "));
    let reply = await_log(&h, &store, &format!("kind=note since=1 ep={ep} timeout=0"));
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
    let head = |h: &SessionHandle| {
        let g = h.ctx.timeline.lock().unwrap();
        (g.slog().log().ep(), g.slog().log().last_seq())
    };
    let (ep, s0) = head(&h);
    assert!(dispatch_mark(&h.ctx, Scope::Owner, "- verdict word=blocked").starts_with("OK "));
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
        await_log(&h, &store, &format!("need since={s0} ep={ep} timeout=0")),
        "OK timeout\n",
        "a verdict claim and a status mark raise no need"
    );
    let step = with_open_approval(&h);
    let reply = await_log(&h, &store, &format!("need since={s0} ep={ep} timeout=0"));
    assert!(
        reply.contains(&format!(" open {step} approval ")),
        "{reply}"
    );
    let (_, s1) = head(&h);
    assert!(dispatch_mark(&h.ctx, Scope::Owner, "- need word=help").starts_with("OK "));
    let reply = await_log(&h, &store, &format!("need since={s1} ep={ep} timeout=0"));
    assert!(
        reply.contains(" need word=help by=ctl src=mark conf=claim "),
        "{reply}"
    );
    let reply = await_log(
        &h,
        &store,
        &format!("open=step since={s0} ep={ep} timeout=0"),
    );
    assert!(reply.contains(&format!(" open {step} ")), "{reply}");
    // From the start of this epoch: no reset gap that never happened.
    let reply = await_log(
        &h,
        &store,
        &format!("kind=verdict since=0 ep={ep} timeout=0"),
    );
    assert!(
        reply.starts_with("OK seq=") && reply.contains(" mark - verdict word=blocked "),
        "{reply}"
    );
    assert!(!reply.contains("gap="), "{reply}");
}

/// `await log kind=program` SEES THE PROGRAM'S OWN REPORT (phase 6, 6F-4;
/// ruling U17, T24's reversal): a waiter parked for the next program mark is
/// answered by the move the status sweep's gate shows (one-minute hang
/// detector). NEGATIVE CONTROL: before the move, the same wait times out at
/// once, and a `bell` wait over the move times out.
#[test]
fn await_log_kind_program_answers_on_the_programs_move() {
    use aterm_core::terminal::program_state::{ProgramState, ProgramStateWord};
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
    let (ep, head) = {
        let g = h.ctx.timeline.lock().unwrap();
        (g.slog().log().ep(), g.slog().log().last_seq())
    };
    let wait = format!("kind=program since={head} ep={ep}");
    assert_eq!(
        await_log(&h, &store, &format!("{wait} timeout=0")),
        "OK timeout\n",
        "no move yet"
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let (h2, store2, parked) = (h.clone(), store.clone(), format!("{wait} timeout=600000"));
    std::thread::spawn(move || {
        let _ = tx.send(await_log(&h2, &store2, &parked));
    });
    let _ = h
        .ctx
        .timeline
        .lock()
        .unwrap()
        .note_program_state(Some(ProgramState {
            state: ProgramStateWord::Working,
            kind: None,
        }));
    let reply = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the program's move answers the wait");
    assert!(
        reply.starts_with("OK seq=")
            && reply.contains(" mark - program state=working src=engine conf=claim "),
        "{reply}"
    );
    assert_eq!(
        await_log(
            &h,
            &store,
            &format!("kind=bell since={head} ep={ep} timeout=0")
        ),
        "OK timeout\n",
        "a program mark is no bell"
    );
}

/// A wrong epoch reads `why=reset`; usage errors answer at once and park
/// nothing; `timeout=0` is the dirty check.
#[test]
fn await_log_refuses_its_usage_at_once_and_says_a_reset() {
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
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
            await_log(&h, &store, bad),
            crate::session_log::AWAIT_LOG_USAGE,
            "{bad:?}"
        );
    }
    let reply = await_log(&h, &store, "close=run since=1 ep=999 timeout=0");
    assert!(reply.starts_with("OK gap why=reset "), "{reply}");
    assert_eq!(await_log(&h, &store, "close=run timeout=0"), "OK timeout\n");
}

/// 5F-1: A PREDICATE LIST IS ONE WAITER. `await log kind=bell,kind=note`
/// parked from the head registers ONE watcher on the session, as one
/// predicate does, and its second member's record answers it (a one-minute
/// hang detector: a missing wake parks it to its deadline), after which the
/// session has no watcher. A since-less list holding `need` latches as `need`
/// alone does, whatever member comes first.
#[test]
fn an_await_log_list_parks_one_waiter_and_answers_on_either_member() {
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
    let (ep, head) = {
        let g = h.ctx.timeline.lock().unwrap();
        (g.slog().log().ep(), g.slog().log().last_seq())
    };
    let args = format!("kind=bell,kind=note since={head} ep={ep} timeout=600000");
    let (tx, rx) = std::sync::mpsc::channel();
    let (h2, store2) = (h.clone(), store.clone());
    std::thread::spawn(move || {
        let _ = tx.send(await_log(&h2, &store2, &args));
    });
    until_within(MUST_HAPPEN, "the list's waiter registers", || {
        (h.ctx.watchers.count() > 0).then_some(())
    });
    assert_eq!(h.ctx.watchers.count(), 1, "one waiter for two");
    assert!(dispatch_mark(&h.ctx, Scope::Owner, "- note text=second").starts_with("OK "));
    let reply = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("woken by the second member's record");
    assert!(reply.contains(" mark - note text=second "), "{reply}");
    until_within(MUST_HAPPEN, "the answered waiter leaves", || {
        (h.ctx.watchers.count() == 0).then_some(())
    });

    let step = with_open_approval(&h);
    for list in ["close=turn,need", "need,kind=bell"] {
        let (tx, rx) = std::sync::mpsc::channel();
        let (h2, store2) = (h.clone(), store.clone());
        let args = format!("{list} timeout=600000");
        std::thread::spawn(move || {
            let _ = tx.send(await_log(&h2, &store2, &args));
        });
        let reply = rx.recv_timeout(MUST_HAPPEN).expect("a latched need");
        assert!(
            reply.contains(" latched=1 ") && reply.contains(&format!(" open {step} approval ")),
            "{list}: {reply}"
        );
    }
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
    let park = |what: &str| {
        let (ep, head) = {
            let g = h.ctx.timeline.lock().unwrap();
            (g.slog().log().ep(), g.slog().log().last_seq())
        };
        let args = format!("{what} since={head} ep={ep} timeout=600000");
        let (tx, rx) = std::sync::mpsc::channel();
        let (h2, store2) = (h.clone(), store.clone());
        std::thread::spawn(move || {
            let _ = tx.send(await_log(&h2, &store2, &args));
        });
        rx
    };
    // A mark.
    let rx = park("kind=note");
    assert!(dispatch_mark(&h.ctx, Scope::Owner, "- note text=wake").starts_with("OK "));
    let reply = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("woken by the mark");
    assert!(reply.contains(" mark - note text=wake "), "{reply}");
    // A turn's first byte.
    let rx = park("kind=input");
    let mark = crate::control_input_ack::InputMark::at_epoch(
        &h.ctx,
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
    h.ctx.watchers.ring_watchers();
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
    h.ctx.timeline.lock().unwrap().slog_mut().note_close_due();
    store.write().unwrap().set_state(1, SessionState::Exited);
    for what in ["close=session", "need", "close=run", "kind=verdict"] {
        assert_eq!(
            await_log(&h, &store, &format!("{what} timeout=0")),
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
        let (h2, store2, tx) = (h.clone(), store.clone(), tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(await_log(&h2, &store2, &args));
        });
    }
    store
        .write()
        .unwrap()
        .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Unknown);
    h.ctx.watchers.ring_watchers();
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
            await_log(&held, &store, &format!("{what} timeout=0")),
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
        || {
            looks += 1;
            if looks == 1 {
                let mut tl = kept.ctx.timeline.lock().unwrap();
                assert!(tl.slog_mut().take_close_due(), "the close was due");
                drop(tl);
                kept.ctx.watchers.ring_watchers();
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
    h.ctx.timeline.lock().unwrap().slog_mut().note_close_due();
    store.write().unwrap().set_state(1, SessionState::Exited);
    let mut retired = 0;
    let reply = control_session::await_log_seamed(
        &store,
        1,
        &h.ctx,
        crate::session_log::LogAwait::parse(&["close=session"]).expect("parses"),
        u64::try_from(aterm_spec::wait::MUST_HAPPEN.as_millis()).unwrap_or(u64::MAX),
        || {
            retired += 1;
            if retired == 1 {
                store.write().unwrap().deregister_local_as(
                    1,
                    ExitReason::ShellExit,
                    ExitActor::Unknown,
                );
                h.ctx.watchers.ring_watchers();
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
        run_subscribe(line, &active, &store, Scope::Owner, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), want, "{line}");
        assert_eq!(watched_sessions(&store), 0, "{line}");
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
        Scope::Edge(tok),
        &mut out,
    );
    assert_eq!(String::from_utf8_lossy(&out), "ERR denied\n");
}

/// THE AUTOPILOT WORD'S `meta` LINES ARE ANSWERED ONCE THE JOURNAL CARRIES
/// THEM (ATERM-KI-1016-1, `word_journaled`): `set autopilot …` and `unset
/// autopilot` take the wait, and so does `hold`'s `OK`; past the bound the
/// `OK` says `journaled=0` (the word stands, no image on disk carries it). A
/// refusal is answered as it is. NEGATIVE CONTROLS: another field's `meta`
/// line, and a reading, take no wait; an unarmed registry answers at once.
#[cfg(unix)]
#[test]
fn the_autopilot_words_meta_lines_wait_on_the_journal() {
    use super::{meta_moves_autopilot, word_journaled};
    for line in [
        "set autopilot land",
        "set autopilot cruise until=+5m",
        "unset autopilot",
    ] {
        assert!(meta_moves_autopilot(line), "{line}");
    }
    for line in ["", "set title x", "unset questions", "autopilot", "set"] {
        assert!(!meta_moves_autopilot(line), "{line:?}");
    }
    let store = crate::session_store::new_store();
    assert_eq!(
        word_journaled(&store, "OK\n".to_string()),
        "OK\n",
        "unarmed: at once"
    );
    // Armed by a live lane whose loop never turns: the bound answers.
    let dir = crate::crash_journal::tests::scratch("cj-meta");
    let lane = crate::crash_journal::Lane::new(
        Some(dir.path().to_path_buf()),
        crate::crash_journal::tests::reaped_pid(),
    );
    let durable = lane.durable();
    assert_eq!(
        durable.settle("OK\n".to_string(), std::time::Duration::from_millis(20)),
        "OK journaled=0\n"
    );
    assert_eq!(
        durable.settle(
            "OK hold=1\n".to_string(),
            std::time::Duration::from_millis(20)
        ),
        "OK hold=1 journaled=0\n"
    );
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .set_journal_durable(durable);
    assert_eq!(
        word_journaled(&store, "ERR denied\n".to_string()),
        "ERR denied\n",
        "a refusal is answered as it is"
    );
}

/// THE DRIVE GATE (the step-in, design §5.1) admits its allowlist and
/// nothing else: each admitted form, with the write it makes; each refused
/// form — another verb (settings, open, invoke, grant, the fleet's and the
/// bus's verbs, privacy, update, a subscription), any frame but a worker's
/// task, a selector that is no hosted sid, a close of a worker another node
/// spawned, a spawn that is no worker's or would take the front window or
/// another session's place, a hold that names an origin, an await that is
/// no task list, a turn whose `timeout=` (or default) outlives the drive's
/// connection — answered with its reason. CONTROL: the same verbs at a
/// hosted sid pass, so each refusal is the form's.
#[test]
fn the_drive_gate_admits_its_allowlist_and_refuses_every_other_line() {
    const W: &str = "s-00000000000000aa";
    const MINE: &str = "s-00000000000000bb";
    let hosts = |sid: &SessionId| [W, MINE].contains(&sid.as_str());
    let spawned = |sid: &SessionId| sid.as_str() == W;
    let gate = |line: &str| drive_gate(line, &hosts, &spawned);
    let wrote = |line: &str| match gate(line) {
        Ok(DriveLine::Act { write, .. }) => write.map(|act| act.verb),
        other => panic!("{line:?} was not admitted: {other:?}"),
    };
    assert_eq!(gate("survey"), Ok(DriveLine::Fleet));
    assert_eq!(gate("version"), Ok(DriveLine::Fleet));
    for read in [
        format!("@{W} status"),
        format!("@{W} text"),
        format!("@{W} screen"),
        format!("@{W} result"),
        format!("@{W} result full"),
        format!("await task @{W} since=0"),
        format!("await task @{W}:3,@{MINE} all timeout=20000"),
    ] {
        assert_eq!(wrote(&read), None, "{read}");
    }
    for (line, verb) in [
        (
            format!("@{W} turn timeout=20000 typed=1 settle=submitted hi"),
            "turn",
        ),
        (
            format!("@{W} turn id=1:2:3 idle=5 timeout=22000 hi"),
            "turn",
        ),
        (format!("@{W} key Enter"), "key"),
        (format!("@{W} send hi"), "send"),
        (format!("hold {W} on reason=build"), "hold"),
        (format!("hold {W} off"), "hold"),
        (format!("@{W} close"), "close"),
        (
            "spawn agent=claude cwd=/tmp from=s-0000000000000c03 len=5".to_owned(),
            "spawn",
        ),
    ] {
        assert_eq!(wrote(&line), Some(verb), "{line}");
    }
    // A hold keeps the direction it asked, for its record.
    for (way, on) in [("on", true), ("off", false)] {
        match gate(&format!("hold {W} {way}")) {
            Ok(DriveLine::Act {
                write: Some(DriveAct {
                    hold: Some(ask), ..
                }),
                ..
            }) => assert_eq!(ask.on, on, "{way}"),
            other => panic!("{way}: {other:?}"),
        }
    }
    // A worker's spawn never takes this Mac's front window.
    match gate("spawn agent=claude model=haiku len=5") {
        Ok(DriveLine::Act { line, .. }) => assert!(line.ends_with(" raise=false"), "{line}"),
        other => panic!("{other:?}"),
    }
    match gate("spawn agent=claude raise=false len=5") {
        Ok(DriveLine::Act { line, .. }) => assert_eq!(line, "spawn agent=claude raise=false len=5"),
        other => panic!("{other:?}"),
    }
    // Every harness's worker is a drive's to spawn.
    for word in ["gemini", "agy", "muse"] {
        match gate(&format!("spawn agent={word} model=m-1 cwd=/tmp len=5")) {
            Ok(DriveLine::Act { line, .. }) => assert_eq!(
                line,
                format!("spawn agent={word} model=m-1 cwd=/tmp len=5 raise=false")
            ),
            other => panic!("{word}: {other:?}"),
        }
    }
    for line in [
        "settings set columns 80".to_owned(),
        "open settings".to_owned(),
        "invoke TakeOver".to_owned(),
        format!("@{W} grant read"),
        "fleet".to_owned(),
        "fabric status".to_owned(),
        "dial lab version".to_owned(),
        "privacy".to_owned(),
        "update check".to_owned(),
        "sessions".to_owned(),
        "subscribe @* events".to_owned(),
        format!("@{W} feed-bin 5"),
        format!("@{W} paste-bin 3"),
        format!("post to=@{W} kind=note len=3"),
        format!("@{W} signal INT"),
        format!("@{W} restart"),
        format!("@{W} meta set role x"),
        // A selector that is no hosted sid.
        "@. status".to_owned(),
        "@1 status".to_owned(),
        "@s-00000000000000cc status".to_owned(),
        format!("@{W},@{MINE} status"),
        "status".to_owned(),
        "turn hi".to_owned(),
        // A turn whose budget outlives its connection: one past the drive's
        // longest, and one naming none (its own default is minutes).
        format!("@{W} turn timeout=22001 hi"),
        format!("@{W} turn id=1:2:3 typed=1 cadence=2000 timeout=600000 hi"),
        format!("@{W} turn hi"),
        // A close of a worker this node did not spawn.
        format!("@{MINE} close"),
        format!("@{W} close now"),
        // A spawn that is no worker's, or reaches past its own.
        "spawn".to_owned(),
        "spawn agent=claude".to_owned(),
        "spawn len=5".to_owned(),
        "spawn agent=codex len=5".to_owned(),
        "spawn agent=generic len=5".to_owned(),
        "spawn agent=gemini".to_owned(),
        "spawn agent=agy identity=x len=5".to_owned(),
        "spawn agent=claude cwd=src len=5".to_owned(),
        format!("spawn agent=claude from={MINE} len=5"),
        "spawn agent=claude raise=true len=5".to_owned(),
        "spawn agent=claude window=1 len=5".to_owned(),
        "spawn agent=claude split=v len=5".to_owned(),
        "spawn agent=claude theme=x len=5".to_owned(),
        format!("spawn connected=controlled place=tab of={W}"),
        format!("@{W} spawn agent=claude len=5"),
        // A hold that names an origin, or a session not hosted here.
        format!("hold {W} on origin=local"),
        format!("hold {W} on origin=remote"),
        "hold s-00000000000000cc on".to_owned(),
        format!("hold {W}"),
        // An await that is no task list.
        "await inbox".to_owned(),
        format!("@{W} await task"),
        "await task @s-00000000000000cc".to_owned(),
        "await task @1".to_owned(),
    ] {
        assert!(
            gate(&line).is_err(),
            "{line:?} was admitted: {:?}",
            gate(&line)
        );
    }
}

/// A DRIVE CONNECTION IS A DRIVE SCOPE ON A THREAD THAT NAMES ITS NODE:
/// there an admitted act goes on as an Owner line and a refused one is
/// answered `ERR denied:`; a fleet verb keeps the fleet scope (its gate
/// judges it). Either half alone — `drive` set with no node on the thread,
/// or a node on the thread of a reader's connection — is a fleet peer's,
/// whose gate refuses the act.
#[test]
#[cfg(unix)]
fn only_a_drive_scope_on_a_drivers_thread_passes_the_drive_gate() {
    let store = crate::session_store::new_store();
    let (target, _rx) = pipe_session(91);
    let sid = target.sid.clone();
    store.write().unwrap().register(target);
    let line = format!("@{} status", sid.as_str());
    let drive = Scope::Fleet {
        content: true,
        drive: true,
    };
    let reader = Scope::Fleet {
        content: true,
        drive: false,
    };
    let driver = crate::fabric::Driver {
        node: "03".repeat(16),
        host: "mac-03".to_owned(),
    };
    // No node on the thread: the line passes untouched, to the fleet gate.
    let (passed, scope, _) = drive_admission(Request::parse(line.clone()), drive, &store).unwrap();
    assert_eq!((passed.line(), scope), (line.as_str(), drive));
    assert!(
        fleet_refusal(&passed, scope).is_some(),
        "the fleet gate refuses it"
    );
    {
        let _driving = crate::fabric::DrivingAs::enter(driver.clone());
        let (_, scope, _) = drive_admission(Request::parse(line.clone()), reader, &store).unwrap();
        assert_eq!(scope, reader, "a reader's connection is never driven");
        let (_, scope, write) =
            drive_admission(Request::parse(line.clone()), drive, &store).unwrap();
        assert_eq!((scope, write), (Scope::Owner, None), "an admitted read");
        let (_, scope, _) =
            drive_admission(Request::parse("survey".to_owned()), drive, &store).unwrap();
        assert_eq!(scope, drive, "a fleet verb keeps the fleet scope");
        let refused = drive_admission(
            Request::parse("settings set columns 80".to_owned()),
            drive,
            &store,
        );
        assert!(refused.is_err_and(|why| why.starts_with("ERR denied: ")));
        let typing = format!("@{} send hi", sid.as_str());
        let (_, scope, write) = drive_admission(Request::parse(typing), drive, &store).unwrap();
        assert_eq!(scope, Scope::Owner);
        assert_eq!(write.map(|act| act.verb), Some("send"));
    }
    assert!(
        crate::fabric::serving_driver().is_none(),
        "the guard put it back"
    );
}

/// THE RECORD OF A DRIVE names what moved. A hold is named by the
/// direction it asked and recorded only when the driving node's remote slot
/// moved that way: an `off` answered `OK hold=1` — the person's, the owner's
/// or the bridge's slot still standing — is a hand-back, never a take; an
/// `off` that lifted nothing of the node's, an `on` it held already, and a
/// refusal record nothing. A typing verb that typed, and a spawn's or a
/// close's `OK`, are records. And a spawn over a drive makes its worker the
/// node's to close, which its close takes off — a refused close keeps it
/// (the NEGATIVE CONTROL of the close the gate then admits).
#[test]
fn a_drives_record_names_what_moved() {
    let sid = SessionId::new("s-0000000000d71e00");
    let hold = |on, held| DriveAct {
        verb: "hold",
        sid: Some(sid.clone()),
        hold: Some(HoldAsk { on, held }),
    };
    for (act, reply, holds_now, words) in [
        (
            hold(true, false),
            "OK hold=1\n",
            true,
            Some("took (hold on)"),
        ),
        (
            hold(false, true),
            "OK hold=1\n",
            false,
            Some("handed back (hold off)"),
        ),
        (
            hold(false, true),
            "OK hold=0\n",
            false,
            Some("handed back (hold off)"),
        ),
        (hold(false, false), "OK hold=1\n", false, None),
        (hold(true, true), "OK hold=1\n", true, None),
        (
            hold(true, false),
            "ERR halted reason=taken-over origin=person\n",
            false,
            None,
        ),
    ] {
        assert_eq!(
            drive_record(&act, reply, holds_now),
            words,
            "{act:?} {reply:?} {holds_now}"
        );
    }
    let act = |verb| DriveAct {
        verb,
        sid: Some(sid.clone()),
        hold: None,
    };
    assert_eq!(drive_record(&act("send"), "OK\n", false), Some("send"));
    assert_eq!(
        drive_record(&act("send"), "ERR halted reason=- origin=remote\n", false),
        None
    );
    assert_eq!(
        drive_record(&act("close"), "OK closed\n", false),
        Some("closed")
    );

    let store = crate::session_store::new_store();
    let driver = crate::fabric::Driver {
        node: "07".repeat(16),
        host: "mac-07".to_owned(),
    };
    let worker = SessionId::new("s-0000000000d71e01");
    let _driving = crate::fabric::DrivingAs::enter(driver.clone());
    let spawn = DriveAct {
        verb: "spawn",
        sid: None,
        hold: None,
    };
    let reply = format!("OK {} task_rev=1\n", worker.as_str());
    assert_eq!(
        drive_record(&spawn, &reply, false),
        Some("started a worker in")
    );
    note_drive_act(&spawn, &reply, &store);
    assert_eq!(drive_spawned_by(&worker), Some(driver.node.clone()));
    let close = DriveAct {
        verb: "close",
        sid: Some(worker.clone()),
        hold: None,
    };
    note_drive_act(&close, "ERR denied: busy\n", &store);
    assert_eq!(
        drive_spawned_by(&worker),
        Some(driver.node.clone()),
        "a refused close keeps it the node's"
    );
    note_drive_act(&close, "OK closed\n", &store);
    assert_eq!(drive_spawned_by(&worker), None);
}

/// THE TYPED-BY STAMP OF A DRIVE names the host over the fleet: an admitted
/// line runs as Owner, and its caller is the node the thread serves, never
/// the client process the Owner arm would read.
#[test]
fn a_drives_typing_is_credited_to_its_host_over_the_fleet() {
    let ctx = test_ctx();
    let driver = crate::fabric::Driver {
        node: "03".repeat(16),
        host: "mac-03".to_owned(),
    };
    let _driving = crate::fabric::DrivingAs::enter(driver);
    let caller = serving_caller(Scope::Owner, &ctx).expect("a caller");
    assert_eq!(
        caller.who,
        crate::typed_by::Who::Word("mac-03 over the fleet".to_owned())
    );
}

/// `sys` escalates to Owner-only exactly the forms whose purpose is other
/// sessions' processes, folders or the instance's parts (the `aterm` view,
/// `procs`/`jobs` beyond the target's tab, `disk` of aterm or the machine,
/// `instances=all`, `name`), classified by `SysQuery`'s own parser and failing
/// closed on anything it cannot read. NEGATIVE CONTROL: the aggregates and the
/// target's own rows stay a Scoped read.
#[test]
fn sys_escalates_its_owner_only_forms_by_its_own_parser() {
    for scoped in [
        "",
        "overview",
        "tab",
        "global",
        "cores --json",
        "gpu",
        "mem top=20",
        "jobs",
        "procs",
        "procs scope=pane",
        "disk scope=window",
        "since=7f3a.12 fresh=0",
    ] {
        assert_eq!(argument_escalation("sys", scoped), None, "{scoped}");
    }
    for owner in [
        "aterm",
        "procs scope=window",
        "procs scope=aterm",
        "jobs scope=global",
        "disk scope=aterm",
        "global instances=all",
        "name job=a40977",
        "bogus",
        "procs top=0",
    ] {
        assert_eq!(
            argument_escalation("sys", owner),
            Some(Need::OwnerOnly),
            "{owner}"
        );
    }
    // The machine wait: its tab's jobs and the volume stay a read (a wait its
    // parser cannot read is refused by the verb before it reads anything);
    // every tab's (`scope=aterm`) is Owner-only.
    for scoped in [
        "sys",
        "sys job=a1 bogus",
        "sys job=a40977 done",
        "sys jobs.stuck=0 scope=tab",
        "sys free>=10GB",
        "text ready",
    ] {
        assert_eq!(argument_escalation("await", scoped), None, "{scoped}");
    }
    for owner in [
        "sys jobs.stuck=0 scope=aterm",
        "sys scope=aterm job=a1 done",
    ] {
        assert_eq!(
            argument_escalation("await", owner),
            Some(Need::OwnerOnly),
            "{owner}"
        );
    }
}
