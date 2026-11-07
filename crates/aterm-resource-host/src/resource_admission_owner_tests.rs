// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual owner-engine lifecycle over private CPU, memory and journal authorities.

use super::*;
use aterm_capacity::{CohortClaimState, CohortClosureRef, Fixed, Need, Polled, Reading};
use aterm_ctl::resource_admission_transport::{self as wire, Attachments};
use aterm_types::resource_admission::{Event, EventFrame, Outcome, Submission};
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

impl World {
    fn new() -> Self {
        Self::with_scratch(Scratch::new())
    }
    fn with_scratch(scratch: Scratch) -> Self {
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
        };
        Self {
            owner,
            namespace,
            retirement,
            scratch,
        }
    }

    fn queue_sleep(&mut self, timeout: Duration) -> (Binding, CohortClosureRef, CtlStream) {
        self.queue_sleep_attempt(timeout, [7; 16])
    }

    fn queue_sleep_attempt(
        &mut self,
        timeout: Duration,
        attempt: [u8; 16],
    ) -> (Binding, CohortClosureRef, CtlStream) {
        let binding = Binding {
            session: "s-acde".into(),
            attempt,
        };
        self.owner.journal.create(&binding).unwrap();
        let machine = Fixed::new(Reading::of_gib(64, 60, 2));
        let need = Need::new("resource-run: actual private owner fixture")
            .mem(MEMORY)
            .margin(aterm_capacity::build_cpu::default_memory_reserve());
        // Run the real private memory admission AFTER host journal preparation. Do not
        // admit around a backend-layout refusal or replace its authority with observations.
        let claim = match Waiter::enter_cohort(&self.owner.ledger, &machine, need)
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
                cwd: self.scratch.path.as_os_str().as_bytes().to_vec(),
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
        self.owner.pending.push_back(Pending {
            submission,
            events,
            recipe: Some(recipe::Recipe {
                kind: recipe::Kind::Ay,
                compiler: None,
                program: "/bin/sleep".into(),
                arguments: vec![OsString::from("60")],
                environment: Vec::new(),
                cwd: self.scratch.path.clone(),
                width: 1,
            }),
            probe: None,
            waiter: None,
            claim: Some(claim),
            recovery: None,
        });
        (binding, reference, client)
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
        self.scratch.preserve =
            !self.owner.idle() || self.owner.debt.reserved().map_or(true, |units| units != 0);
    }
}

fn read_event(client: &CtlStream, binding: &Binding) -> Event {
    let bytes = wire::read_frame(client, Instant::now() + HANG).unwrap();
    EventFrame::decode(&bytes, binding).unwrap().event
}

#[test]
fn actual_owner_timeout_and_caller_cancel_reap_and_settle_original_resources() {
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
        assert!(job.launched && !job.reaped && !job.settled);
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
fn impossible_memory_head_refuses_before_arm_and_next_job_runs() {
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

#[path = "resource_admission_owner_tests/context_rpc.rs"]
mod context_rpc;

#[test]
fn known_member_attempt_never_receives_a_new_no_child_refusal() {
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
pub(super) fn member_fixture(args: &[OsString]) -> std::process::ExitCode {
    member_native::run(args)
}
