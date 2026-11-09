// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual owner-engine lifecycle over private CPU, memory and journal authorities.

use super::*;
use aterm_capacity::{CohortClaimState, CohortClosureRef, Fixed, Need, Polled, Reading};
use aterm_ctl::resource_admission_transport::{self as wire, Attachments};
#[cfg(target_os = "macos")]
use aterm_types::resource_admission::Outcome;
use aterm_types::resource_admission::{Event, EventFrame, Submission};
use aterm_uds::{
    CtlStream,
    process::{self, ProcessPresence},
};
use std::{
    ffi::OsString,
    num::NonZeroU32,
    os::{
        fd::OwnedFd,
        unix::{
            ffi::OsStrExt as _,
            fs::{DirBuilderExt as _, PermissionsExt as _},
        },
    },
};

const HANG: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(10);
const MEMORY: u64 = 1024 * 1024 * 1024;

#[test]
#[cfg(target_os = "macos")]
fn queue_io_fault_ends_original_job_as_host_fault() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::queue_io_fault_ends_original_job_as_host_fault",
    ) {
        return;
    }
    assert_fault_terminal(io::Error::from_raw_os_error(libc::ENOTCONN), "NotConnected");
}

#[test]
#[cfg(target_os = "macos")]
fn queue_progress_fault_ends_original_job_as_host_fault() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::queue_progress_fault_ends_original_job_as_host_fault",
    ) {
        return;
    }
    assert_fault_terminal("launched without GO", "launched without GO");
}

#[cfg(target_os = "macos")]
fn assert_fault_terminal(detail: impl std::fmt::Debug, expected: &str) {
    crate::runtime::test_endpoint().unwrap();
    let mut world = World::new();
    let (binding, _, client) = world.queue_sleep(HANG);
    assert_eq!(read_event(&client, &binding), Event::Queued);
    world.owner.tick_queue();
    assert!(matches!(
        read_event(&client, &binding),
        Event::Started { .. }
    ));
    world
        .owner
        .journal
        .fault(&mut world.owner.faulted, "injected", detail);
    assert!(world.owner.journal.fault_summary().contains(expected));
    world.settle();
    assert!(matches!(
        read_event(&client, &binding),
        Event::Finished {
            outcome: Outcome::Signal(libc::SIGKILL),
            cause: CompletionCause::HostFault,
            ..
        }
    ));
    assert_eq!(world.owner.debt.reserved().unwrap(), 0);
    assert!(world.owner.faulted);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[path = "resource_admission_owner_tests/bootstrap_object_engineering.rs"]
mod bootstrap_object_engineering;

#[cfg(target_os = "macos")]
fn owner_fixture_sdk_lease(
    prefix: &std::path::Path,
    directory: &std::path::Path,
    who: &str,
    started: Instant,
) -> std::io::Result<atpkg::lease::Lease> {
    let remaining = crate::fixture_deadline(started)
        .checked_duration_since(Instant::now())
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::TimedOut))?;
    match atpkg::lease::hold_for(prefix, directory, who, remaining) {
        Ok(atpkg::lease::Hold::Held(lease)) => Ok(lease),
        _ => Err(std::io::Error::other(
            "managed fixture SDK lease unavailable",
        )),
    }
}

#[test]
#[cfg(target_os = "macos")]
fn native_owner_fixture_sdk_lease_keeps_original_gc_custody() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let layout = atpkg::store::Layout {
        prefix: scratch.path().join("manager"),
    };
    let build = layout.build_dir("trust", 1);
    std::fs::create_dir_all(&build).unwrap();
    let subject = atpkg::lease::Subject::build("trust", 1).unwrap();
    let lease = owner_fixture_sdk_lease(
        &layout.prefix,
        &build,
        "fixture custody",
        Instant::now() - Duration::from_secs(3),
    )
    .unwrap();
    assert_eq!(lease.subject(), &subject);
    lease.check_current().unwrap();
    assert!(matches!(
        atpkg::lease::reclaim(&layout.prefix, &subject),
        atpkg::lease::Reclaim::Keep(atpkg::lease::Holders::Held(_))
    ));
    assert_eq!(
        owner_fixture_sdk_lease(
            &layout.prefix,
            &build,
            "expired",
            Instant::now() - Duration::from_secs(61)
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::TimedOut
    );
    drop(lease);
    let atpkg::lease::Reclaim::Clear(guard) = atpkg::lease::reclaim(&layout.prefix, &subject)
    else {
        panic!("fixture lease still retained after drop")
    };
    drop(guard);
    std::fs::remove_dir(&build).unwrap();
    assert!(owner_fixture_sdk_lease(&layout.prefix, &build, "missing", Instant::now()).is_err());
}

#[cfg(target_os = "macos")]
fn native_owner_fixture_hello(role: &str) {
    let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    let peer = wire::local_peer(&server).unwrap();
    let hello = aterm_json::json!({"role": role});
    crate::runtime::write_json(&client, &hello, crate::fixture_deadline(Instant::now())).unwrap();
    let end = crate::fixture_deadline(Instant::now() - Duration::from_secs(9));
    assert_eq!(crate::runtime::read_json(&server, end).unwrap(), hello);
    assert_eq!(wire::local_peer(&server).unwrap(), peer);
    wire::write_frame(
        &client,
        b"not-json",
        crate::fixture_deadline(Instant::now()),
    )
    .unwrap();
    assert_eq!(
        crate::runtime::read_json(&server, crate::fixture_deadline(Instant::now()))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Other,
    );
    crate::runtime::write_json(&client, &hello, crate::fixture_deadline(Instant::now())).unwrap();
    assert_eq!(
        crate::runtime::read_json(
            &server,
            crate::fixture_deadline(Instant::now() - Duration::from_secs(61)),
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::TimedOut,
    );
}

#[test]
#[cfg(target_os = "macos")]
fn native_owner_fixture_metadata_waits_for_original_hello() {
    native_owner_fixture_hello("native-metadata-ready-v1");
}

#[test]
#[cfg(target_os = "macos")]
fn native_owner_fixture_resolver_waits_for_original_hello() {
    native_owner_fixture_hello(trust_resource_controller::resolver::RESOLVER_READY_ROLE);
}

#[test]
fn loan_bookkeeping_does_not_resurrect_a_native_terminal_phase() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::loan_bookkeeping_does_not_resurrect_a_native_terminal_phase",
    ) {
        return;
    }
    let world = World::new();
    let binding = Binding {
        session: "s-ab".into(),
        attempt: [17; 16],
    };
    world.owner.journal.create(&binding).unwrap();
    world
        .owner
        .journal
        .update(&binding, "member_terminal", aterm_json::json!({}))
        .unwrap();
    world
        .owner
        .journal
        .patch(
            &binding,
            aterm_json::json!({"cpu_loan":{"state":"returned"}}),
        )
        .unwrap();
    let records = world.owner.journal.outstanding().unwrap();
    let encoded = aterm_codec::hex::encode_lower(&binding.attempt);
    let row = records
        .iter()
        .find(|row| row["attempt"].as_str() == Some(encoded.as_str()))
        .unwrap();
    assert_eq!(row["phase"].as_str(), Some("member_terminal"));
    assert_eq!(row["cpu_loan"]["state"].as_str(), Some("returned"));
    world.owner.journal.settle(&binding).unwrap();
}

struct Scratch {
    path: PathBuf,
    legacy: PathBuf,
    preserve: bool,
}

impl Scratch {
    fn new() -> Self {
        let root =
            aterm_uds::test_binary::scratch_root().expect("fixture requires Cargo test isolation");
        Self::at(root)
    }
    fn at(root: &std::path::Path) -> Self {
        let path = root.join(".ubd").join(format!(
            "owner-engine-{}.noindex",
            aterm_uds::rand::hex_token::<16>().unwrap()
        ));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .unwrap();
        eprintln!("actual owner fixture: {}", path.display());
        Self {
            legacy: path.join("legacy"),
            path,
            preserve: false,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.legacy.exists() {
            // Scoped retirement intentionally removed write permission. Restore only this
            // fixture's original legacy directory so successful scratch can be removed.
            let _ = std::fs::set_permissions(&self.legacy, std::fs::Permissions::from_mode(0o700));
        }
        if self.preserve || std::thread::panicking() {
            eprintln!("preserved actual owner fixture: {}", self.path.display());
        } else {
            std::fs::remove_dir_all(&self.path).unwrap();
        }
    }
}

struct World {
    // Drop actual authorities before removing any of their original storage.
    owner: Owner,
    namespace: Namespace,
    retirement: aterm_capacity::build_cpu::LegacyRetirement,
    scratch: Scratch,
}

#[test]
#[cfg(target_os = "macos")]
fn metadata_ancestry_admits_after_scheduling_delay_but_still_detects_a_hang() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::metadata_ancestry_admits_after_scheduling_delay_but_still_detects_a_hang",
    ) {
        return;
    }
    let mut world = World::new();
    let (_, _, events) = world.queue_sleep(HANG);
    let pending = world.owner.pending.front_mut().unwrap();
    pending.scope_reference = Some(
        pending
            .claim
            .as_ref()
            .unwrap()
            .cohort_closure_ref()
            .unwrap(),
    );
    let pending = world.owner.pending.front().unwrap();
    // Backdate the operation start instead of sleeping or demanding a latency.
    // The real native walk and original ledger scan still run, including their
    // final reread. Restoring the 250 ms budget fails this admission.
    world
        .owner
        .check_fresh_metadata_ancestry_at(pending, Instant::now() - Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        world
            .owner
            .check_fresh_metadata_ancestry_at(pending, Instant::now() - HANG)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    drop(events);
    world.settle();
}

#[test]
#[cfg(target_os = "macos")]
fn fresh_metadata_refuses_funded_native_ancestor_but_admits_unrelated_original_capacity() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::fresh_metadata_refuses_funded_native_ancestor_but_admits_unrelated_original_capacity",
    ) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    let mut world = World::new();
    let (binding, memory, events) = world.queue_sleep(HANG);
    let first = world.owner.pending.front_mut().unwrap();
    first.submission.request.argv = vec![b"/bin/sleep".to_vec(), b"60".to_vec()];
    world.owner.tick_queue();
    assert_eq!(read_event(&events, &binding), Event::Queued);
    let Event::Started { pid, .. } = read_event(&events, &binding) else {
        panic!("original funded root did not start")
    };
    let native = process::read_identity(pid as i32).unwrap();
    assert_eq!(world.owner.debt.reserved().unwrap(), 1);
    assert!(
        Fence::acquire(&world.namespace, Instant::now()).is_err(),
        "a different locator cannot acquire another canonical owner fence"
    );

    let (second_binding, second_memory, second_events) = world.queue_sleep_attempt(HANG, [8; 16]);
    let mut second = world.owner.pending.pop_back().unwrap();
    second.submission.request.argv = vec![
        b"/not-selected/targo".to_vec(),
        b"--resource-metadata-v1".to_vec(),
    ];
    second.submission.request.env.clear();
    second.submission.peer = Peer {
        pid,
        birth: native.birth.0,
    };
    second.submission.request.peer_pid = pid;
    second.submission.request.peer_birth = native.birth.0;
    world.owner.pending.push_front(second);
    world.owner.tick_queue();
    assert_eq!(read_event(&second_events, &second_binding), Event::Queued);
    assert!(matches!(
        read_event(&second_events, &second_binding),
        Event::Refused { .. }
    ));
    assert!(world.owner.pending.is_empty());
    assert_eq!(
        world
            .owner
            .ledger
            .cohort_claim_state(&second_memory)
            .unwrap(),
        CohortClaimState::Closed
    );
    assert_eq!(world.owner.debt.reserved().unwrap(), 1);

    // The same original pool still has one real unit. An unrelated requesting
    // process is allowed beside the first funded root, without shadow pools or
    // a blanket refusal whenever any other reservation exists.
    let (third_binding, third_memory, third_events) = world.queue_sleep_attempt(HANG, [9; 16]);
    let third = world.owner.pending.back_mut().unwrap();
    third.submission.request.argv = vec![
        b"/not-selected/targo".to_vec(),
        b"--resource-metadata-v1".to_vec(),
    ];
    let (probe, release) = recipe::Probe::paused();
    third.recipe = None;
    third.probe = Some(probe);
    world.owner.tick_queue();
    let third = world.owner.pending.front_mut().unwrap();
    assert!(third.admitted.is_some());
    assert_eq!(world.owner.debt.reserved().unwrap(), 2);
    // Cancel the controlled preparation and drain both original reservations.
    third.cancelled = true;
    drop(release);
    world
        .owner
        .jobs
        .get_mut(&(binding.session.clone(), binding.attempt))
        .unwrap()
        .child
        .request_group(aterm_capacity::cohort::launch::CohortSignal::Kill)
        .unwrap();
    world.settle();
    world.assert_settled(&binding, &memory, pid);
    assert_eq!(
        world
            .owner
            .ledger
            .cohort_claim_state(&third_memory)
            .unwrap(),
        CohortClaimState::Closed
    );
    assert!(!world.owner.journal.fresh(&second_binding).unwrap());
    assert!(!world.owner.journal.fresh(&third_binding).unwrap());
    drop(third_events);
    drop(second_events);
}

impl World {
    fn new() -> Self {
        Self::with_scratch(Scratch::new())
    }
    fn with_scratch(scratch: Scratch) -> Self {
        let (owner, namespace, retirement) = Self::prepare(&scratch);
        Self {
            owner,
            namespace,
            retirement,
            scratch,
        }
    }
    fn prepare(
        scratch: &Scratch,
    ) -> (
        Owner,
        Namespace,
        aterm_capacity::build_cpu::LegacyRetirement,
    ) {
        let home = scratch.path.join("home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch.legacy)
            .unwrap();
        let namespace = Namespace::create_private(&scratch.path, &home).unwrap();
        let ledger = Ledger::at(namespace.fence());
        let pool =
            Pool::open_in(&scratch.path.join("capacity"), NonZeroU32::new(2).unwrap()).unwrap();
        let retirement = aterm_capacity::build_cpu::LegacyRetirement::retire(&scratch.legacy, 1)
            .expect("private scoped retirement requires a non-root test process");
        let debt = ExternalDebt::activate(&pool, &retirement).unwrap();
        journal::Journal::prepare(&namespace, &debt).unwrap();
        let journal = journal::Journal::open(&namespace, &debt).unwrap();
        let fence = Fence::acquire(&namespace, Instant::now() + HANG).unwrap();
        let owner = Owner {
            _domain: None,
            fence,
            ledger,
            pool,
            debt,
            journal,
            pending: VecDeque::new(),
            jobs: BTreeMap::new(),
            faulted: false,
            recovered_at: Instant::now(),
            failed: Vec::new(),
            recipe_layout: None,
            #[cfg(target_os = "macos")]
            metadata_pause: None,
        };
        (owner, namespace, retirement)
    }
}

// A queued job's root starts stopped at its exec (`Recipe::command`), which only
// Darwin's spawn does: elsewhere the launch refuses before any child exists.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl World {
    fn queue_sleep(&mut self, timeout: Duration) -> (Binding, CohortClosureRef, CtlStream) {
        self.queue_sleep_attempt(timeout, [7; 16])
    }

    fn queue_sleep_attempt(
        &mut self,
        timeout: Duration,
        attempt: [u8; 16],
    ) -> (Binding, CohortClosureRef, CtlStream) {
        queue_owned(&mut self.owner, &self.scratch.path, timeout, attempt)
    }

    fn settle(&mut self) {
        let deadline = Instant::now() + HANG;
        while !self.owner.idle() {
            self.owner.tick();
            assert!(
                Instant::now() < deadline,
                "actual owner did not reap and settle: jobs={}, failed={}, pending={}, faulted={}",
                self.owner.jobs.len(),
                self.owner.failed.len(),
                self.owner.pending.len(),
                self.owner.faulted
            );
            if !self.owner.idle() {
                std::thread::sleep(POLL);
            }
        }
    }

    fn assert_settled(&mut self, binding: &Binding, memory: &CohortClosureRef, pid: u32) {
        assert!(self.owner.jobs.is_empty() && self.owner.failed.is_empty());
        assert_eq!(
            process::process_presence(i32::try_from(pid).unwrap()),
            ProcessPresence::Absent,
            "the original child must actually be reaped"
        );
        assert_eq!(self.owner.debt.reserved().unwrap(), 0);
        assert!(
            self.owner
                .pool
                .snapshot()
                .unwrap()
                .iter()
                .all(|slot| matches!(slot, aterm_capacity::build_cpu::SlotState::Available { .. }))
        );
        let (_, external) =
            ExternalDebt::observe(&self.owner.pool, Some(&self.retirement)).unwrap();
        assert!(external.unwrap().iter().all(|slot| matches!(
            slot,
            aterm_capacity::build_cpu::RetainedSlotState::Available { .. }
        )));
        assert_eq!(
            self.owner.ledger.reap_cohort_claim(memory).unwrap(),
            CohortClaimState::Closed
        );
        assert!(self.owner.ledger.list().unwrap().is_empty());
        let reopened = journal::Journal::open(&self.namespace, &self.owner.debt).unwrap();
        assert!(reopened.outstanding().unwrap().is_empty());
        assert!(
            !reopened.fresh(binding).unwrap(),
            "settlement must leave a durable replay tombstone"
        );
        assert!(!self.owner.accepts(binding));
        assert!(!self.owner.faulted);
    }
}

fn queue_owned(
    owner: &mut Owner,
    cwd: &std::path::Path,
    timeout: Duration,
    attempt: [u8; 16],
) -> (Binding, CohortClosureRef, CtlStream) {
    let binding = Binding {
        session: "s-acde".into(),
        attempt,
    };
    owner.journal.create(&binding).unwrap();
    let machine = Fixed::new(Reading::of_gib(64, 60, 2));
    let need = Need::new("resource-run: actual private owner fixture")
        .mem(MEMORY)
        .margin(aterm_capacity::build_cpu::default_memory_reserve());
    // Run the real private memory admission AFTER host journal preparation. Do not
    // admit around a backend-layout refusal or replace its authority with observations.
    let claim = match Waiter::enter_cohort(&owner.ledger, &machine, need)
        .unwrap()
        .poll(&machine)
    {
        Polled::Admitted(claim) => claim,
        other => panic!("private memory was not admitted: {other:?}"),
    };
    let reference = claim.cohort_closure_ref().unwrap();
    let (host, client) = CtlStream::pair().unwrap();
    let host: OwnedFd = host.into();
    let mut events = job::Events::new(host.try_clone().unwrap(), binding.clone()).unwrap();
    events.emit(Event::Queued).unwrap();
    let pid = std::process::id();
    let birth = process::read_birth(i32::try_from(pid).unwrap()).unwrap().0;
    let null = || -> OwnedFd { File::open("/dev/null").unwrap().into() };
    let output = || -> OwnedFd {
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .unwrap()
            .into()
    };
    let submission = AuthorizedSubmission {
        request: Submission {
            binding: binding.clone(),
            peer_pid: pid,
            peer_birth: birth,
            argv: vec![b"/bin/sleep".to_vec(), b"60".to_vec()],
            cwd: cwd.as_os_str().as_bytes().to_vec(),
            env: Vec::new(),
            cpu_units: 1,
            memory_bytes: MEMORY,
            label: "actual private owner fixture".into(),
            timeout_ms: Some(timeout.as_millis().try_into().unwrap()),
        },
        peer: Peer { pid, birth },
        fds: Attachments {
            stdin: null(),
            stdout: output(),
            stderr: output(),
            events: host,
        },
    };
    submission.request.validate().unwrap();
    owner.pending.push_back(Pending {
        queued_at: Instant::now(),
        primary: None,
        resolver: None,
        bootstrap_graph: None,
        submission,
        events,
        recipe: Some(recipe::Recipe {
            kind: recipe::Kind::Ay,
            compiler: None,
            program: "/bin/sleep".into(),
            arguments: vec![OsString::from("60")],
            environment: Vec::new(),
            cwd: cwd.to_path_buf(),
            width: 1,
            metadata_sdk: None,
        }),
        sdk: None,
        probe: None,
        waiter: None,
        claim: Some(claim),
        recovery: None,
        admitted: None,
        scope_reference: None,
        cancelled: false,
    });
    (binding, reference, client)
}

impl Drop for World {
    fn drop(&mut self) {
        // Failure cleanup still runs the actual owner over its original child handles.
        // Closing interest grants no CPU/memory release and never licenses numeric killing.
        for pending in &mut self.owner.pending {
            pending.events.abandon();
        }
        for job in self.owner.jobs.values_mut() {
            job.events.abandon();
        }
        let deadline = Instant::now() + HANG;
        while !self.owner.idle() && Instant::now() < deadline {
            self.owner.tick();
            if !self.owner.idle() {
                std::thread::sleep(POLL);
            }
        }
        self.scratch.preserve |=
            !self.owner.idle() || self.owner.debt.reserved().map_or(true, |units| units != 0);
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn read_event(client: &CtlStream, binding: &Binding) -> Event {
    let bytes = wire::read_frame(client, Instant::now() + HANG).unwrap();
    EventFrame::decode(&bytes, binding).unwrap().event
}

#[test]
#[cfg(target_os = "macos")]
fn actual_owner_timeout_and_caller_cancel_reap_and_settle_original_resources() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::actual_owner_timeout_and_caller_cancel_reap_and_settle_original_resources",
    ) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    for cancel_caller in [false, true] {
        let mut world = World::new();
        let budget = Duration::from_millis(250);
        let (binding, memory, client) = world.queue_sleep(budget);
        assert_eq!(read_event(&client, &binding), Event::Queued);
        assert_eq!(world.owner.debt.reserved().unwrap(), 0);
        // Deliberately remain queued longer than the execution budget. A queue timestamp
        // used as the launch clock would now fail the Started/actual-runtime assertions.
        let queued = Instant::now();
        while queued.elapsed() < budget + POLL {
            std::thread::sleep(POLL);
        }
        assert!(world.owner.jobs.is_empty());
        world.owner.tick_queue(); // Real advance -> prepare -> retained fixed grant -> launch.
        assert!(world.owner.failed.is_empty() && !world.owner.faulted);
        let Event::Started {
            pid,
            birth,
            monotonic_ns,
            unix_ns,
        } = read_event(&client, &binding)
        else {
            panic!("actual host failed to publish Started")
        };
        assert!(pid > 1 && birth > 0 && monotonic_ns > 0 && unix_ns > 0);
        let capability = *world.owner.jobs[&(binding.session.clone(), binding.attempt)]
            .delegation
            .capability
            .as_bytes();
        let query = trust_resource_controller::ContextQuery {
            binding: trust_resource_controller::JobBinding {
                session: binding.session.clone(),
                root_attempt: binding.attempt,
                capability,
            },
            call_attempt: [91; 16],
        };
        let context = world.owner.context(Peer { pid, birth }, &query).unwrap();
        // Backdate the start, never sleep or budget successful native reads.
        let delayed = Instant::now() - Duration::from_secs(3);
        let delayed_context = world.owner.context_at(Peer { pid, birth }, &query, delayed);
        let inspection = world
            .owner
            .inspect_at(Peer { pid, birth }, &binding.session, delayed);
        assert!(
            delayed_context.is_ok() && inspection.is_ok(),
            "context={delayed_context:?}; inspection={inspection:?}"
        );
        assert_eq!(inspection.unwrap().root_pid, pid);
        let expired = Instant::now() - HANG;
        assert!(
            world
                .owner
                .context_at(Peer { pid, birth }, &query, expired)
                .is_err()
        );
        assert!(
            world
                .owner
                .inspect_at(Peer { pid, birth }, &binding.session, expired)
                .is_err()
        );
        assert_eq!(context.context.original_root.pid, pid);
        assert_eq!(context.context.memory.bytes, MEMORY);
        assert!(
            !context
                .context
                .may_acquire_initial_cpu(context.context.original_root)
        );
        let mut foreign = query.clone();
        foreign.binding.capability[0] ^= 1;
        assert!(world.owner.context(Peer { pid, birth }, &foreign).is_err());
        assert!(
            world
                .owner
                .context(
                    Peer {
                        pid,
                        birth: birth + 1
                    },
                    &query
                )
                .is_err()
        );

        let job = world
            .owner
            .jobs
            .get_mut(&(binding.session.clone(), binding.attempt))
            .unwrap();
        assert!(job.launched && !job.reaped && !job.is_settled());
        assert_eq!((job.child.child_mut().id(), job.root_birth), (pid, birth));
        assert_eq!(
            process::read_birth(i32::try_from(pid).unwrap()).unwrap().0,
            birth
        );
        assert!(matches!(&job.custody, Custody::Fixed(_)));
        assert_eq!(world.owner.debt.reserved().unwrap(), 1);
        assert_eq!(
            world.owner.ledger.cohort_claim_state(&memory).unwrap(),
            CohortClaimState::Retained
        );
        if cancel_caller {
            drop(client);
            assert!(
                world
                    .owner
                    .jobs
                    .contains_key(&(binding.session.clone(), binding.attempt))
            );
            assert_eq!(
                world.owner.debt.reserved().unwrap(),
                1,
                "caller cancellation cannot release custody before actual cleanup"
            );
            world.settle();
        } else {
            world.settle();
            let Event::Finished {
                outcome,
                cause,
                runtime_ns,
            } = read_event(&client, &binding)
            else {
                panic!("actual host failed to publish Finished")
            };
            assert_eq!(cause, CompletionCause::Timeout);
            assert_eq!(outcome, Outcome::Signal(libc::SIGKILL));
            assert!(runtime_ns >= budget.as_nanos() as u64);
        }
        world.assert_settled(&binding, &memory, pid);
    }
}

#[test]
#[cfg(target_os = "macos")]
fn impossible_memory_head_refuses_before_arm_and_next_job_runs() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::impossible_memory_head_refuses_before_arm_and_next_job_runs",
    ) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    let mut world = World::new();
    let (impossible, _, client) = world.queue_sleep(Duration::from_secs(60));
    assert_eq!(read_event(&client, &impossible), Event::Queued);
    let pending = world.owner.pending.front_mut().unwrap();
    drop(pending.claim.take()); // The fixture's original description has never armed.
    pending.submission.request.memory_bytes = u64::MAX;
    world.owner.tick_queue();
    assert!(matches!(
        read_event(&client, &impossible),
        Event::Refused { .. }
    ));
    assert!(world.owner.idle());
    assert_eq!(world.owner.debt.reserved().unwrap(), 0);
    assert!(world.owner.ledger.list().unwrap().is_empty());
    assert!(world.owner.journal.outstanding().unwrap().is_empty());

    let (next, memory, next_client) = world.queue_sleep_attempt(Duration::from_secs(60), [8; 16]);
    assert_eq!(read_event(&next_client, &next), Event::Queued);
    world.owner.tick_queue();
    let Event::Started { pid, .. } = read_event(&next_client, &next) else {
        panic!("attainable job behind the refused head did not run")
    };
    drop(next_client);
    world.settle();
    world.assert_settled(&next, &memory, pid);
}

/// `world`'s queued sleep, its fixture's claim let go before it armed, so that its memory is
/// admitted by the queue's own look ([`Owner::poll_memory`]); and the stream its events go to.
fn unclaimed_sleep(world: &mut World, attempt: [u8; 16]) -> (Pending, CtlStream) {
    let (_, _, client) = world.queue_sleep_attempt(HANG, attempt);
    let mut pending = world.owner.pending.pop_back().unwrap();
    drop(pending.claim.take());
    (pending, client)
}

/// The `kind`, `outcome`, `stop`, `sig` and `meter` of `row`.
fn admission_words(
    row: &aterm_telemetry::Row,
) -> (
    aterm_telemetry::Kind,
    aterm_telemetry::Outcome,
    aterm_telemetry::Stop,
    Option<i32>,
    aterm_telemetry::Meter,
) {
    (row.kind.clone(), row.outcome, row.stop, row.sig, row.meter)
}

/// A RETAINED RUN'S MEMORY ADMISSION WRITES ONE ROW, WHERE IT IS DECIDED (TM-1 review): the
/// queue drives a waiter of its own in this user's ledger across its ticks, not `admit_with`,
/// so its `kind=admission` row is written at the look that decides, however many looks waited:
/// three looks that met another admission holding the ledger's lock write none, and the look in
/// its turn writes one, admitted, with the wait its record's stamps say; a look once it holds its
/// claim writes none. NEGATIVE CONTROLS (by hand): the queue writing no row, as before: none at
/// the decision; a row each look: three for the looks that waited.
#[test]
fn a_retained_runs_memory_admission_writes_one_row_where_it_is_decided() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::a_retained_runs_memory_admission_writes_one_row_where_it_is_decided",
    ) {
        return;
    }
    use aterm_capacity::telemetry::{capture, wait};
    use aterm_telemetry::{Kind, Meter, Outcome, Stop};
    let mut world = World::new();
    let machine = Fixed::new(Reading::of_gib(64, 60, 2));
    let user = world.owner.ledger.clone();
    let (mut pending, client) = unclaimed_sleep(&mut world, [7; 16]);
    let turn = world
        .owner
        .ledger
        .hold_turn()
        .expect("the ledger's lock")
        .expect("free");
    let ((), rows) = capture(user.clone(), || {
        for look in 0..3 {
            world.owner.poll_memory(&mut pending, &machine).unwrap();
            assert!(
                pending.claim.is_none() && pending.waiter.is_some(),
                "look {look} met the ledger's lock"
            );
        }
    });
    assert!(rows.is_empty(), "no row a look: {rows:?}");
    drop(turn);
    let ((), rows) = capture(user.clone(), || {
        world.owner.poll_memory(&mut pending, &machine).unwrap();
    });
    let claim = pending.claim.as_ref().expect("admitted in its turn");
    assert_eq!(rows.len(), 1, "one row however many looks: {rows:?}");
    assert_eq!(
        admission_words(&rows[0]),
        (Kind::Admission, Outcome::Ok, Stop::Exit, None, Meter::Exact)
    );
    assert_eq!(
        rows[0].wait_ms,
        wait(claim.asked_ms(), claim.record().held.map(|h| h.admitted_ms)),
        "the wait its record's stamps say"
    );
    let ((), rows) = capture(user, || {
        world.owner.poll_memory(&mut pending, &machine).unwrap();
    });
    assert!(rows.is_empty(), "a look once it holds its claim: {rows:?}");
    pending.cancelled = true;
    world.owner.pending.push_back(pending);
    world.settle();
    assert!(world.owner.ledger.list().unwrap().is_empty());
    drop(client);
}

/// A RETAINED RUN'S MEMORY WAIT GIVEN UP, OR REFUSED, WRITES ONE ROW (TM-1 review): a request
/// cancelled while its memory waits leaves the queue with its row, `outcome=fail`, written as
/// its wait drops; an impossible head is refused before it enters the wait, and its row says
/// `outcome=refused`. NEGATIVE CONTROLS (by hand): a wait given up writing no row, as before:
/// none for the first; the refusal written before its ask is taken: none for the second.
#[test]
fn a_retained_runs_memory_wait_given_up_or_refused_writes_one_row() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::a_retained_runs_memory_wait_given_up_or_refused_writes_one_row",
    ) {
        return;
    }
    use aterm_capacity::telemetry::capture;
    use aterm_telemetry::{Kind, Meter, Outcome, Stop};
    let mut world = World::new();
    let machine = Fixed::new(Reading::of_gib(64, 60, 2));
    let user = world.owner.ledger.clone();
    let (mut pending, client) = unclaimed_sleep(&mut world, [7; 16]);
    let turn = world
        .owner
        .ledger
        .hold_turn()
        .expect("the ledger's lock")
        .expect("free");
    let ((), rows) = capture(user.clone(), || {
        world.owner.poll_memory(&mut pending, &machine).unwrap();
        assert!(pending.waiter.is_some(), "it waits for its turn");
        pending.cancelled = true;
        world.owner.pending.push_back(pending);
        world.owner.tick_queue();
    });
    assert!(world.owner.pending.is_empty(), "the cancelled request left");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        admission_words(&rows[0]),
        (
            Kind::Admission,
            Outcome::Fail,
            Stop::Exit,
            None,
            Meter::Stamped
        )
    );
    drop(turn);

    let (mut pending, impossible) = unclaimed_sleep(&mut world, [8; 16]);
    pending.submission.request.memory_bytes = u64::MAX;
    let (refused, rows) = capture(user, || world.owner.poll_memory(&mut pending, &machine));
    assert_eq!(
        refused.expect_err("it can never fit").kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(pending.waiter.is_none(), "it never entered the wait");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        admission_words(&rows[0]),
        (
            Kind::Admission,
            Outcome::Refused,
            Stop::Exit,
            None,
            Meter::Stamped
        )
    );
    pending.cancelled = true;
    world.owner.pending.push_back(pending);
    world.settle();
    assert!(world.owner.ledger.list().unwrap().is_empty());
    drop((client, impossible));
}

#[cfg(target_os = "macos")]
#[path = "resource_admission_owner_tests/context_rpc.rs"]
mod context_rpc;

#[test]
fn cancelled_paid_metadata_preparation_joins_before_native_release_and_settlement() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::cancelled_paid_metadata_preparation_joins_before_native_release_and_settlement",
    ) {
        return;
    }
    for fault_owner in [false, true] {
        let mut world = World::new();
        let (binding, memory, client) = world.queue_sleep(HANG);
        let mut pending = world.owner.pending.pop_back().unwrap();
        pending.submission.request.argv = vec![
            b"/unselected/targo".to_vec(),
            b"--resource-metadata-v1".to_vec(),
        ];
        // This test starts at paid preparation. Native ancestry admission is
        // covered separately; its observation deadline is not a cancellation precondition.
        let queue::Advance::Launch(custody) = world.owner.prepare(&mut pending).unwrap() else {
            panic!("private preparation did not acquire original custody")
        };
        pending.admitted = Some(*custody);
        pending.recipe = None;
        let (probe, release) = recipe::Probe::paused();
        pending.probe = Some(probe);
        world.owner.pending.push_back(pending);
        assert!(world.owner.pending.front().unwrap().admitted.is_some());
        assert_eq!(
            world.owner.debt.reserved().unwrap(),
            1,
            "expensive metadata preparation has one actual original CPU"
        );
        assert_eq!(
            world.owner.ledger.list().unwrap().len(),
            1,
            "original memory claim stays retained"
        );
        assert!(world.owner.jobs.is_empty(), "no native target was launched");
        drop(client);
        world.owner.faulted = fault_owner;
        world.owner.tick();
        assert_eq!(
            world.owner.pending.len(),
            1,
            "unjoined preparation remains occupied even on owner failure"
        );
        assert_eq!(world.owner.debt.reserved().unwrap(), 1);
        assert!(
            !world.owner.journal.outstanding().unwrap().is_empty(),
            "cancel is not a settlement receipt"
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + HANG;
        while !world
            .owner
            .pending
            .front()
            .unwrap()
            .probe
            .as_ref()
            .unwrap()
            .finished()
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL);
        }
        world.owner.tick_queue();
        assert!(world.owner.pending.is_empty());
        assert_eq!(
            world.owner.failed.len(),
            1,
            "original unused custody is retained for affirmative finish"
        );
        assert_eq!(world.owner.debt.reserved().unwrap(), 1);
        assert!(
            !world.owner.journal.outstanding().unwrap().is_empty(),
            "journal cannot settle before original CPU/memory finish"
        );
        world.owner.tick_failed();
        assert!(world.owner.failed.is_empty());
        assert_eq!(world.owner.debt.reserved().unwrap(), 0);
        assert!(world.owner.journal.outstanding().unwrap().is_empty());
        assert_eq!(
            world.owner.ledger.reap_cohort_claim(&memory).unwrap(),
            CohortClaimState::Closed
        );
        assert!(!world.owner.journal.fresh(&binding).unwrap());
    }
}

#[path = "resource_admission_owner_tests/metadata_native.rs"]
#[cfg(target_os = "macos")]
mod metadata_native;

#[test]
fn workload_ancestry_slice_waits_without_renewing_or_granting_original_resources() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::workload_ancestry_slice_waits_without_renewing_or_granting_original_resources",
    ) {
        return;
    }
    let mut world = World::new();
    let (binding, memory, client) = world.queue_sleep(HANG);
    assert_eq!(read_event(&client, &binding), Event::Queued);
    let mut pending = world.owner.pending.pop_front().unwrap();
    pending.events.account().unwrap();
    pending.submission.request.argv = vec![
        b"/unselected/targo".to_vec(),
        b"--resource-workload-v1".to_vec(),
    ];
    pending.submission.request.cpu_units = 2;
    pending.recipe = None;
    let original = pending.original_deadline().unwrap();
    for _ in 0..2 {
        assert!(
            world
                .owner
                .workload_inspection_wait_test(&mut pending, Instant::now())
                .unwrap()
        );
        assert_eq!(pending.original_deadline().unwrap(), original);
        assert!(pending.primary.is_none());
        assert!(world.owner.jobs.is_empty());
        assert_eq!(world.owner.debt.reserved().unwrap(), 0);
        assert_eq!(world.owner.ledger.list().unwrap().len(), 1);
    }
    // Published metadata admission uses its own one-minute observation hang
    // detector, not the NEW workload's resumable inspection slice. Its actual
    // expired observation still refuses instead of becoming queued work.
    pending.submission.request.argv[1] = b"--resource-metadata-v1".to_vec();
    pending.submission.request.cpu_units = 1;
    assert_eq!(
        world
            .owner
            .check_fresh_metadata_ancestry_at(&pending, Instant::now() - HANG)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    pending.submission.request.argv[1] = b"--resource-workload-v1".to_vec();
    pending.submission.request.cpu_units = 2;
    pending.queued_at = Instant::now()
        .checked_sub(HANG + Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        world
            .owner
            .workload_inspection_wait_test(&mut pending, Instant::now() + HANG)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    world.owner.pending.push_back(pending);
    world.owner.tick_queue();
    assert!(world.owner.pending.is_empty());
    assert_eq!(world.owner.failed.len(), 1);
    let deadline = Instant::now() + HANG;
    while !world.owner.idle() {
        assert!(Instant::now() < deadline);
        world.owner.tick();
    }
    let terminal = aterm_types::resource_admission::AccountedEventFrame::decode(
        &wire::read_frame(&client, deadline).unwrap(),
        &binding,
    )
    .unwrap();
    assert!(
        matches!(terminal.finished.event, Event::Refused { reason } if reason.contains("deadline"))
    );
    assert_eq!(world.owner.debt.reserved().unwrap(), 0);
    assert_eq!(
        world.owner.ledger.reap_cohort_claim(&memory).unwrap(),
        CohortClaimState::Closed
    );
    assert!(!world.owner.journal.fresh(&binding).unwrap());
}

#[test]
fn workload_final_go_preserves_the_original_absolute_clock() {
    let queued = Instant::now();
    let deadline = queued + Duration::from_secs(2);
    let early = start::remaining_workload_timeout(deadline, queued).unwrap();
    let later_go = queued + Duration::from_secs(1);
    let later = start::remaining_workload_timeout(deadline, later_go).unwrap();
    assert!(later < early);
    assert!(queued.checked_add(early).unwrap() <= deadline);
    assert!(later_go.checked_add(later).unwrap() <= deadline);
    assert!(start::remaining_workload_timeout(deadline, deadline).is_none());
    assert!(
        start::remaining_workload_timeout(deadline, deadline + Duration::from_nanos(1)).is_none()
    );
}

#[test]
fn workload_queue_expiry_and_parent_close_keep_paid_preparation_until_join() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::workload_queue_expiry_and_parent_close_keep_paid_preparation_until_join",
    ) {
        return;
    }
    for expire in [true, false] {
        let mut world = World::new();
        let (binding, memory, client) = world.queue_sleep(HANG);
        assert_eq!(read_event(&client, &binding), Event::Queued);
        let mut pending = world.owner.pending.pop_front().unwrap();
        pending.events.account().unwrap();
        pending.submission.request.argv = vec![
            b"/unselected/targo".to_vec(),
            b"--resource-workload-v1".to_vec(),
        ];
        pending.submission.request.cpu_units = 2;
        pending.recipe = None;
        let width = NonZeroU32::new(2).unwrap();
        let context = recovery::context(&binding, &memory, MEMORY, width.get());
        let material = world
            .owner
            .debt
            .prepare_recoverable(
                aterm_capacity::build_cpu::Owner::current(None),
                width,
                context,
            )
            .unwrap();
        let (parent, leaf) = world.owner.journal.authority(&binding).unwrap();
        let mut prepared = material.persist(&parent, &leaf).unwrap();
        let reservation = world
            .owner
            .debt
            .try_acquire_all_recoverable(&mut prepared)
            .unwrap()
            .unwrap();
        pending.primary = Some(workload::Primary::new(reservation));
        pending.recovery = Some(prepared);
        let (probe, release) = recipe::Probe::paused();
        pending.probe = Some(probe);
        if expire {
            pending.queued_at = Instant::now()
                .checked_sub(HANG + Duration::from_secs(1))
                .unwrap();
        }
        let client = if expire {
            Some(client)
        } else {
            drop(client);
            None
        };
        world.owner.pending.push_back(pending);
        world.owner.tick_queue();
        assert_eq!(world.owner.pending.len(), 1);
        assert_eq!(world.owner.debt.reserved().unwrap(), 2);
        assert_eq!(world.owner.ledger.list().unwrap().len(), 1);
        assert!(world.owner.jobs.is_empty());
        release.send(()).unwrap();
        let deadline = Instant::now() + HANG;
        while !world
            .owner
            .pending
            .front()
            .unwrap()
            .probe
            .as_ref()
            .unwrap()
            .finished()
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL);
        }
        world.owner.tick_queue();
        assert!(world.owner.pending.is_empty());
        assert_eq!(world.owner.failed.len(), 1);
        assert_eq!(world.owner.debt.reserved().unwrap(), 2);
        while !world.owner.idle() {
            assert!(Instant::now() < deadline);
            world.owner.tick();
        }
        if let Some(client) = client {
            let terminal = aterm_types::resource_admission::AccountedEventFrame::decode(
                &wire::read_frame(&client, deadline).unwrap(),
                &binding,
            )
            .unwrap();
            assert!(
                matches!(terminal.finished.event, Event::Refused { reason } if reason.contains("deadline"))
            );
        }
        assert_eq!(world.owner.debt.reserved().unwrap(), 0);
        assert_eq!(
            world.owner.ledger.reap_cohort_claim(&memory).unwrap(),
            CohortClaimState::Closed
        );
        assert!(!world.owner.journal.fresh(&binding).unwrap());
    }
}
#[cfg(target_os = "macos")]
pub(super) fn metadata_native_role() {
    metadata_native::early_role();
}

#[path = "resource_admission_owner_tests/resolver_native.rs"]
#[cfg(target_os = "macos")]
mod resolver_native;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[path = "resource_admission_owner_tests/workload_native.rs"]
mod workload_native;

#[path = "resource_admission_owner_tests/workload_leadership.rs"]
mod workload_leadership;

#[path = "resource_admission_owner_tests/resolver_engineering.rs"]
#[cfg(target_os = "macos")]
mod resolver_engineering;

#[test]
fn known_member_attempt_never_receives_a_new_no_child_refusal() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::known_member_attempt_never_receives_a_new_no_child_refusal",
    ) {
        return;
    }
    use aterm_ctl::resource_subgroup_client::{
        AuthorizedSubgroup, Delegations, RemoteChild, Rights,
    };
    use aterm_types::resource_subgroup::{Capability, CpuDelegation, GnuTransport, Request};
    let mut world = World::new();
    let binding = Binding {
        session: "s-acde".into(),
        attempt: [92; 16],
    };
    world.owner.journal.create(&binding).unwrap();
    let (host, client) = CtlStream::pair().unwrap();
    let mut observer = RemoteChild::from_private_events(binding.clone(), client.into()).unwrap();
    let pid = std::process::id();
    let peer = Peer {
        pid,
        birth: process::read_birth(pid as i32).unwrap().0,
    };
    let null = || -> OwnedFd {
        File::options()
            .read(true)
            .write(true)
            .open("/dev/null")
            .unwrap()
            .into()
    };
    world.owner.submit_member(AuthorizedSubgroup {
        request: Request {
            job: Binding {
                session: binding.session.clone(),
                attempt: [91; 16],
            },
            capability: Capability::from_bytes([1; 32]),
            member: Submission {
                binding,
                peer_pid: peer.pid,
                peer_birth: peer.birth,
                argv: vec![b"/usr/bin/true".to_vec()],
                cwd: b"/".to_vec(),
                env: vec![],
                cpu_units: 1,
                memory_bytes: MEMORY,
                label: "known attempt".into(),
                timeout_ms: None,
            },
            cpu: CpuDelegation::FixedBorrow { units: 1 },
            gnu: GnuTransport::None,
            fence_targets: vec![],
        },
        peer,
        rights: Rights {
            stdin: null(),
            stdout: null(),
            stderr: null(),
            events: host.into(),
            delegated: Delegations {
                cpu: None,
                gnu: vec![],
                fences: vec![],
            },
        },
    });
    assert_eq!(
        observer.try_wait().unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert!(world.owner.idle());
    assert_eq!(world.owner.debt.reserved().unwrap(), 0);
}

#[cfg(target_os = "macos")]
#[path = "resource_admission_owner_tests/member_native.rs"]
mod member_native;
#[cfg(target_os = "macos")]
#[path = "resource_admission_owner_tests/runtime_inputs.rs"]
mod runtime_inputs;
#[cfg(target_os = "macos")]
pub(super) fn member_fixture(
    args: &[OsString],
    gnu: Option<[std::os::fd::OwnedFd; 2]>,
) -> std::process::ExitCode {
    member_native::run(args, gnu)
}

#[test]
#[cfg(target_os = "macos")]
fn accounted_root_waits_for_original_settlement_and_preserves_real_outcomes() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::accounted_root_waits_for_original_settlement_and_preserves_real_outcomes",
    ) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    use aterm_types::resource_admission::AccountedEventFrame;
    use std::io::Read as _;
    for (script, cancel, expected) in [
        ("exit 0", false, Some(0)),
        ("exit 7", false, Some(7)),
        ("exec /bin/sleep 60", true, None),
    ] {
        let mut world = World::new();
        let (binding, memory, events) = world.queue_sleep(HANG);
        let pending = world.owner.pending.front_mut().unwrap();
        pending.events.account().unwrap();
        let recipe = pending.recipe.as_mut().unwrap();
        recipe.program = "/bin/sh".into();
        recipe.arguments = vec!["-c".into(), script.into()];
        pending.submission.request.argv = vec![
            b"/bin/sh".to_vec(),
            b"-c".to_vec(),
            script.as_bytes().to_vec(),
        ];
        world.owner.tick_queue();
        assert_eq!(read_event(&events, &binding), Event::Queued);
        let Event::Started { pid, .. } = read_event(&events, &binding) else {
            panic!("actual accounted root did not start")
        };
        // Delay only durable settlement in this fixture. Original exit/reap
        // must not publish the accounted terminal or release its paid custody.
        let key = (binding.session.clone(), binding.attempt);
        world.owner.jobs.get_mut(&key).unwrap().settlement_at = Some(Instant::now() + HANG);
        if cancel {
            wire::send_cancel(
                &events,
                aterm_types::resource_admission::Cancel {
                    binding: binding.clone(),
                },
                Instant::now() + HANG,
            )
            .unwrap();
        }
        let deadline = Instant::now() + HANG;
        while !world.owner.jobs.get(&key).unwrap().reaped {
            world.owner.tick();
            assert!(
                Instant::now() < deadline,
                "original root owning wait did not finish"
            );
            std::thread::sleep(POLL);
        }
        let job = world.owner.jobs.get_mut(&key).unwrap();
        assert!(!job.is_settled() && !job.terminal_sent && job.completion.is_some());
        assert_eq!(world.owner.debt.reserved().unwrap(), 1);
        events.set_nonblocking(true).unwrap();
        assert_eq!(
            (&events).read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let job = world.owner.jobs.get_mut(&key).unwrap();
        let cached = job
            .child
            .child_mut()
            .try_wait_with_rusage()
            .unwrap()
            .unwrap();
        assert_eq!(
            job.completion
                .as_ref()
                .unwrap()
                .1
                .cpu_times
                .unwrap()
                .user_ns,
            u64::try_from(cached.user.as_nanos()).unwrap()
        );
        assert_eq!(
            job.completion
                .as_ref()
                .unwrap()
                .1
                .cpu_times
                .unwrap()
                .system_ns,
            u64::try_from(cached.system.as_nanos()).unwrap()
        );
        job.settlement_at = None;
        world.settle();
        world.assert_settled(&binding, &memory, pid);
        let frame = AccountedEventFrame::decode(
            &wire::read_frame(&events, Instant::now() + HANG).unwrap(),
            &binding,
        )
        .unwrap();
        let Event::Finished { outcome, cause, .. } = frame.finished.event else {
            panic!("no accounted finished")
        };
        if let Some(code) = expected {
            assert_eq!(outcome, Outcome::Exit(code));
        } else {
            assert!(matches!(outcome, Outcome::Signal(_)));
        }
        assert_eq!(
            cause,
            if cancel {
                CompletionCause::Cancelled
            } else {
                CompletionCause::Exited
            }
        );
        assert!(
            frame.observation.cpu_times.is_some() && frame.observation.peak_rss_bytes.is_some()
        );
        assert_eq!((&events).read(&mut [0]).unwrap(), 0);
    }
}

#[test]
#[cfg(target_os = "macos")]
fn original_root_settlement_is_live_single_owner_and_bound_to_the_actual_attempt() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::original_root_settlement_is_live_single_owner_and_bound_to_the_actual_attempt",
    ) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    let mut world = World::new();
    let (binding, memory, events) = world.queue_sleep(HANG);
    world
        .owner
        .pending
        .front_mut()
        .unwrap()
        .events
        .account()
        .unwrap();
    world.owner.tick_queue();
    assert_eq!(read_event(&events, &binding), Event::Queued);
    let Event::Started { pid, .. } = read_event(&events, &binding) else {
        panic!("actual original root did not start")
    };
    let key = (binding.session.clone(), binding.attempt);
    {
        let Owner {
            jobs,
            fence,
            journal,
            ..
        } = &mut world.owner;
        let job = jobs.get_mut(&key).unwrap();
        assert!(settlement::settle(job, fence, journal).is_err());
        assert!(job.closed.is_none());
        // Delay automatic settlement so the actual owning wait and the
        // independently retried native closure remain separately observable.
        job.settlement_at = Some(Instant::now() + HANG);
    }
    assert_eq!(world.owner.debt.reserved().unwrap(), 1);
    wire::send_cancel(
        &events,
        aterm_types::resource_admission::Cancel {
            binding: binding.clone(),
        },
        Instant::now() + HANG,
    )
    .unwrap();
    let deadline = Instant::now() + HANG;
    while !world.owner.jobs.get(&key).unwrap().reaped {
        world.owner.tick();
        assert!(
            Instant::now() < deadline,
            "actual original root did not reap"
        );
        std::thread::sleep(POLL);
    }
    {
        let Owner {
            jobs,
            fence,
            journal,
            ..
        } = &mut world.owner;
        let job = jobs.get_mut(&key).unwrap();
        assert!(job.closed.is_none(), "exit/reap is not complete settlement");
        settlement::settle(job, fence, journal).unwrap();
        assert!(job.is_settled());
        assert!(job.closed.as_ref().unwrap().matches(job));
        assert!(
            settlement::settle(job, fence, journal).is_err(),
            "one actual root cannot mint two original settlement results"
        );
        let original = job.binding.clone();
        job.binding.attempt[0] ^= 1;
        assert!(
            !job.is_settled(),
            "foreign attempt inherited original settlement"
        );
        job.binding = original;
        let original_birth = job.root_birth;
        job.root_birth ^= 1;
        assert!(
            !job.is_settled(),
            "foreign native birth inherited original settlement"
        );
        job.root_birth = original_birth;
        assert!(job.is_settled());
    }
    world.settle();
    world.assert_settled(&binding, &memory, pid);
}

#[test]
#[cfg(target_os = "macos")]
fn accounted_paid_no_child_refusal_waits_for_original_custody_settlement() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::accounted_paid_no_child_refusal_waits_for_original_custody_settlement",
    ) {
        return;
    }
    use aterm_types::resource_admission::AccountedEventFrame;
    use std::io::Read as _;
    crate::runtime::test_endpoint().unwrap();
    let mut world = World::new();
    let (binding, memory, events) = world.queue_sleep(HANG);
    let missing = world
        .scratch
        .path
        .join("actual-image-disappeared-before-launch");
    let pending = world.owner.pending.front_mut().unwrap();
    pending.events.account().unwrap();
    pending.recipe.as_mut().unwrap().program = missing.clone();
    pending.submission.request.argv[0] = missing.as_os_str().as_bytes().to_vec();
    world.owner.tick_queue();
    assert_eq!(read_event(&events, &binding), Event::Queued);
    assert_eq!(
        world.owner.failed.len(),
        1,
        "actual native launch must produce a retained no-child failure"
    );
    assert_eq!(world.owner.debt.reserved().unwrap(), 1);
    world.owner.failed[0].checked_at = Some(Instant::now() + HANG);
    world.owner.tick();
    events.set_nonblocking(true).unwrap();
    let premature = (&events).read(&mut [0]);
    world.owner.failed[0].checked_at = None;
    assert!(
        matches!(&premature, Err(error) if error.kind() == io::ErrorKind::WouldBlock),
        "accounted refusal must not return before paid original custody settles: {premature:?}"
    );
    world.settle();
    assert_eq!(world.owner.debt.reserved().unwrap(), 0);
    assert_eq!(
        world.owner.ledger.reap_cohort_claim(&memory).unwrap(),
        CohortClaimState::Closed
    );
    assert!(world.owner.journal.outstanding().unwrap().is_empty());
    let refused = AccountedEventFrame::decode(
        &wire::read_frame(&events, Instant::now() + HANG).unwrap(),
        &binding,
    )
    .unwrap();
    assert!(matches!(refused.finished.event, Event::Refused { .. }));
    assert_eq!(
        refused.observation,
        aterm_types::resource_admission::AccountingObservation::uncollected()
    );
    assert_eq!((&events).read(&mut [0]).unwrap(), 0);
}

#[test]
#[cfg(target_os = "macos")]
fn accounted_memory_only_refusal_keeps_original_funded_ancestor_untouched() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::accounted_memory_only_refusal_keeps_original_funded_ancestor_untouched",
    ) {
        return;
    }
    use aterm_types::resource_admission::AccountedEventFrame;
    use std::io::Read as _;
    crate::runtime::test_endpoint().unwrap();
    let mut world = World::new();
    let (original, original_memory, original_events) = world.queue_sleep(HANG);
    world.owner.tick_queue();
    assert_eq!(read_event(&original_events, &original), Event::Queued);
    let Event::Started { pid, .. } = read_event(&original_events, &original) else {
        panic!("original funded root absent")
    };
    let native = process::read_identity(pid as i32).unwrap();
    let (binding, memory, events) = world.queue_sleep_attempt(HANG, [39; 16]);
    let pending = world.owner.pending.back_mut().unwrap();
    pending.events.account().unwrap();
    pending.submission.request.argv = vec![
        b"/not-selected/targo".to_vec(),
        b"--resource-metadata-v1".to_vec(),
    ];
    pending.submission.peer = Peer {
        pid,
        birth: native.birth.0,
    };
    pending.submission.request.peer_pid = pid;
    pending.submission.request.peer_birth = native.birth.0;
    world.owner.tick_queue();
    assert_eq!(read_event(&events, &binding), Event::Queued);
    assert_eq!(world.owner.failed.len(), 1);
    assert_eq!(
        world.owner.debt.reserved().unwrap(),
        1,
        "no second CPU was admitted"
    );
    assert!(matches!(
        world.owner.failed[0].custody,
        Some(Custody::Cooperative(_))
    ));
    world.owner.failed[0].checked_at = Some(Instant::now() + HANG);
    world.owner.tick_failed();
    events.set_nonblocking(true).unwrap();
    let premature = (&events).read(&mut [0]);
    world.owner.failed[0].checked_at = None;
    assert!(matches!(premature, Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    world.owner.tick_failed();
    assert!(world.owner.failed.is_empty());
    assert_eq!(
        world.owner.ledger.reap_cohort_claim(&memory).unwrap(),
        CohortClaimState::Closed
    );
    let refusal = AccountedEventFrame::decode(
        &wire::read_frame(&events, Instant::now() + HANG).unwrap(),
        &binding,
    )
    .unwrap();
    assert!(matches!(refusal.finished.event, Event::Refused { .. }));
    assert_eq!(
        refusal.observation,
        aterm_types::resource_admission::AccountingObservation::uncollected()
    );
    assert_eq!((&events).read(&mut [0]).unwrap(), 0);
    assert_eq!(world.owner.debt.reserved().unwrap(), 1);
    assert!(process::read_identity(pid as i32).is_some());
    wire::send_cancel(
        &original_events,
        aterm_types::resource_admission::Cancel {
            binding: original.clone(),
        },
        Instant::now() + HANG,
    )
    .unwrap();
    world.settle();
    world.assert_settled(&original, &original_memory, pid);
}

// Reproducer for the original GNU epoch/cold-recovery gap. These are real
// private World memory/CPU authorities, not a fake SDK/Ready/closure receipt.
// The extra Host journal field is DATA only: current readers ignore it.
fn cold_gnu_recovery_preserves_original_debt(test: &str, claimed_retired: bool) {
    use std::io::Read as _;
    if !aterm_uds::test_binary::isolated_lifecycle(test) {
        return;
    }
    let mut world = World::new();
    let (binding, memory, client) = world.queue_sleep(HANG);
    let mut pending = world.owner.pending.pop_front().unwrap();
    let width = NonZeroU32::new(2).unwrap();
    let material = world
        .owner
        .debt
        .prepare_recoverable(
            aterm_capacity::build_cpu::Owner::current(None),
            width,
            recovery::context(&binding, &memory, MEMORY, width.get()),
        )
        .unwrap();
    let (parent, leaf) = world.owner.journal.authority(&binding).unwrap();
    let mut prepared = material.persist(&parent, &leaf).unwrap();
    world
        .owner
        .journal
        .update(
            &binding,
            "prepared",
            aterm_json::json!({
                "cpu_mode":"retained_fixed", "cpu_units":2,
                "memory_bytes":MEMORY, "memory":memory.to_json(),
                "gnu_epoch":{"version":1,"retired":claimed_retired}
            }),
        )
        .unwrap();
    let cpu = world
        .owner
        .debt
        .try_reserve_recoverable(&mut prepared, world.owner.pool.slots().get())
        .unwrap()
        .unwrap();
    let mut custody =
        RetainedCohort::new_bundle(&world.owner.fence, pending.claim.take().unwrap(), cpu).unwrap();
    let ([read, write], fifo) = custody
        .prepare_gnu(&world.owner.fence, width)
        .unwrap()
        .into_parts();
    let mut read = File::from(read);
    let write = File::from(write);
    let mut actual_borrow = [0];
    read.read_exact(&mut actual_borrow).unwrap();
    assert_eq!(actual_borrow, *b"+");
    // The actual unused memory cohort may close, but its real GNU borrow has
    // not returned. The live wrapper correctly refuses every CPU Free write.
    assert!(custody.try_finish(&world.owner.fence).is_err());
    assert_eq!(world.owner.debt.reserved().unwrap(), 2);
    let state = world.owner.ledger.reap_cohort_claim(&memory).unwrap();
    eprintln!("actual memory state before cold restore: {state:?}");
    drop(custody);
    drop(prepared); // No live recovery-material issuance lock remains.
    pending.events.abandon();
    drop(pending);
    drop(client);
    // The offered original FIFO descriptions remain live even if Drop has
    // removed its namespace. Neither fact is completion/absence authority.
    use std::os::unix::fs::FileTypeExt as _;
    assert!(read.metadata().unwrap().file_type().is_fifo());
    assert!(write.metadata().unwrap().file_type().is_fifo());
    eprintln!("original FIFO pathname survives Drop: {}", fifo.exists());
    world.owner.journal = journal::Journal::open(&world.namespace, &world.owner.debt).unwrap();
    // Both a conservative refusal and a retained-state result are valid;
    // neither permits recovering the aggregate as reusable CPU.
    let recovered = world.owner.recover();
    eprintln!("actual cold restore result: {recovered:?}");
    assert_eq!(
        world.owner.debt.reserved().unwrap(),
        2,
        "cold restore freed real original CPU while one GNU byte was still borrowed"
    );
    assert!(
        world
            .owner
            .debt
            .try_reserve(aterm_capacity::build_cpu::Owner::current(None), width, 2)
            .unwrap()
            .is_none()
    );
    assert_eq!(actual_borrow, *b"+");
    assert!(
        !world.owner.journal.outstanding().unwrap().is_empty(),
        "missing GNU return must not acquire a settled Host tombstone"
    );
}

#[test]
fn cold_gnu_recovery_keeps_the_real_missing_byte_and_original_cpu_debt() {
    cold_gnu_recovery_preserves_original_debt(
        "actor::owner::tests::cold_gnu_recovery_keeps_the_real_missing_byte_and_original_cpu_debt",
        false,
    );
}

#[test]
fn cold_gnu_recovery_cannot_turn_decoded_retired_data_into_epoch_closure() {
    cold_gnu_recovery_preserves_original_debt(
        "actor::owner::tests::cold_gnu_recovery_cannot_turn_decoded_retired_data_into_epoch_closure",
        true,
    );
}

#[test]
fn original_nongnu_cold_restore_still_settles_the_exact_unused_bundle() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::original_nongnu_cold_restore_still_settles_the_exact_unused_bundle",
    ) {
        return;
    }
    let mut world = World::new();
    let (binding, memory, client) = world.queue_sleep(HANG);
    let mut pending = world.owner.pending.pop_front().unwrap();
    let width = NonZeroU32::new(2).unwrap();
    let material = world
        .owner
        .debt
        .prepare_recoverable(
            aterm_capacity::build_cpu::Owner::current(None),
            width,
            recovery::context(&binding, &memory, MEMORY, width.get()),
        )
        .unwrap();
    let (parent, leaf) = world.owner.journal.authority(&binding).unwrap();
    let mut prepared = material.persist(&parent, &leaf).unwrap();
    world
        .owner
        .journal
        .update(
            &binding,
            "prepared",
            aterm_json::json!({
                "cpu_mode":"retained_fixed", "cpu_units":2,
                "memory_bytes":MEMORY, "memory":memory.to_json()
            }),
        )
        .unwrap();
    let cpu = world
        .owner
        .debt
        .try_reserve_recoverable(&mut prepared, world.owner.pool.slots().get())
        .unwrap()
        .unwrap();
    let custody =
        RetainedCohort::new_bundle(&world.owner.fence, pending.claim.take().unwrap(), cpu).unwrap();
    assert_eq!(world.owner.debt.reserved().unwrap(), 2);
    drop(custody); // No GNU epoch or issued supplementary credit exists.
    drop(prepared);
    pending.events.abandon();
    drop(pending);
    drop(client);
    world.owner.journal = journal::Journal::open(&world.namespace, &world.owner.debt).unwrap();
    assert_eq!(
        world.owner.ledger.reap_cohort_claim(&memory).unwrap(),
        CohortClaimState::Closed
    );
    world.owner.recover().unwrap();
    assert_eq!(world.owner.debt.reserved().unwrap(), 0);
    assert!(world.owner.journal.outstanding().unwrap().is_empty());
    assert!(!world.owner.journal.fresh(&binding).unwrap());
    let actual = world
        .owner
        .debt
        .try_reserve(aterm_capacity::build_cpu::Owner::current(None), width, 2)
        .unwrap()
        .unwrap();
    assert_eq!(actual.len(), 2);
    for original in actual {
        original.settle().unwrap();
    }
}
