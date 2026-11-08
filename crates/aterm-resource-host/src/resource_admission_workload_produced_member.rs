// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Producing targets use the original native owning wait and cohort engine.
//! No legacy fixed budget, CPU delegation or decoded registration creates GO.
use super::*;
use crate::actor::owner::{job::Events, member_custody};
use aterm_types::resource_admission::Event;
use trust_resource_controller::workload::host::MemberKind;

pub(in crate::actor::owner) struct Member {
    binding: Binding,
    peer: Peer,
    state: State,
}
enum State {
    Active(Box<Active>),
    Refused { events: Box<Events>, settled: bool },
}
struct Active {
    attempt: member::OwnedAttempt<Resources>,
    terminal: bool,
    settled: bool,
}
struct Resources {
    registered: Arc<Registered>,
    request: wire::ProducedRequest,
    gnu: [OwnedFd; 2],
    handoff: jobserver_utils::ScopedHandoff,
    prelude: crate::metadata_prelude::Owner,
}

fn control_identity(pid: u32, birth: u64) -> trust_resource_controller::Identity {
    trust_resource_controller::Identity {
        pid,
        birth,
        birth_kind: if cfg!(target_os = "macos") {
            trust_resource_controller::BirthKind::MacosWallMicroseconds
        } else {
            trust_resource_controller::BirthKind::LinuxStartTicks
        },
    }
}
impl member::MemberResources for Resources {
    fn configure(&mut self, command: &mut spawnfd::Command) -> io::Result<()> {
        if command.guard_targets().len() != 0 {
            return Err(refused(
                "produced command already carries foreign resources",
            ));
        }
        self.registered
            .program
            .validate(native_inputs::deadline(Instant::now()))?;
        let mut guards = vec![
            (self.gnu[0].try_clone()?, 3),
            (self.gnu[1].try_clone()?, 4),
            (self.registered._sdk.descriptor()?, sdk::TARGET),
        ];
        guards.push(self.prelude.extra()?);
        command.inherit_guards_at(guards)?;
        for name in ["MAKEFLAGS", "MFLAGS", "CARGO_MAKEFLAGS"] {
            command.env(name, "-j --jobserver-fds=3,4 --jobserver-auth=3,4");
        }
        command.env(
            jobserver_utils::DESCENDANT_HANDOFF_ENV,
            self.handoff.advertisement(),
        );
        command.env("ATERM_SDK_LEASE_V3", sdk::TARGET.to_string());
        self.prelude.configured(command)
    }
    fn bind_child(&mut self, pid: u32) -> io::Result<()> {
        self.handoff.bind_child(pid)?;
        self.registered.program.validate_loaded(
            trust_resource_provenance::process::read_identity(pid)?,
            native_inputs::deadline(Instant::now()),
        )
    }
    fn prepare_target(&mut self, child: &mut spawnfd::Child) -> io::Result<bool> {
        let held = &self.registered.program;
        self.prelude.poll(child, |pid| {
            held.validate_loaded(
                trust_resource_provenance::process::read_identity(pid)?,
                native_inputs::deadline(Instant::now()),
            )
        })
    }
    fn publish_prepared(
        &mut self,
        (pid, birth): (u32, u64),
        events: &mut Events,
    ) -> io::Result<()> {
        let held = &self.registered.program;
        held.validate(native_inputs::deadline(Instant::now()))?;
        let observed = held.observation();
        let inventory = wire::ProducedPrepared {
            key: self.request.key.clone(),
            member_attempt: self.request.member.member_attempt,
            identity: control_identity(pid, birth),
            output_root: held.output_root().as_os_str().as_bytes().to_vec(),
            sdk_root: self
                .registered
                ._sdk
                .execution()
                .as_os_str()
                .as_bytes()
                .to_vec(),
            policy: observed.policy.into(),
            scope: observed.scope.into(),
            executable: observed_file(&observed.executable)?,
            runtime: observed
                .runtime
                .iter()
                .map(observed_file)
                .collect::<io::Result<_>>()?,
            loader_paths: observed
                .loader_paths
                .iter()
                .map(|path| path.as_os_str().as_bytes().to_vec())
                .collect(),
            system_dependencies: observed.system_dependencies.clone(),
            host: observed.host.clone(),
            boot: observed.boot.clone(),
        };
        inventory.validate_for(&self.request)?;
        events.produced_prepared(&inventory)
    }
    fn finish_inputs(&mut self) -> Option<Result<(), String>> {
        Some(
            self.registered
                .program
                .validate(native_inputs::deadline(Instant::now()))
                .map_err(|error| error.to_string()),
        )
    }
}

impl Member {
    pub(in crate::actor::owner) fn group(&self) -> Option<i32> {
        match &self.state {
            State::Active(active) => active.attempt.group(),
            _ => None,
        }
    }
    pub(in crate::actor::owner) fn covers(&mut self, peer: Peer) -> bool {
        match &mut self.state {
            State::Active(active) => active.attempt.covers(peer),
            _ => false,
        }
    }
    pub(in crate::actor::owner) fn subject(
        &self,
    ) -> Option<aterm_types::resource_inspection::Subject> {
        let State::Active(active) = &self.state else {
            return None;
        };
        let (pid, birth) = active.attempt.identity()?;
        Some(
            aterm_types::resource_inspection::Subject::RegisteredMember {
                pid,
                birth,
                attempt: self.binding.attempt,
            },
        )
    }
    fn new(
        owner: &Owner,
        job: &mut Job,
        incoming: crate::workload_transport::ProducedIncoming,
    ) -> io::Result<Self> {
        let crate::workload_transport::ProducedIncoming {
            request,
            peer,
            stdin,
            stdout,
            stderr,
            events,
        } = incoming;
        // Retire the caller's intake handle without consuming its bytes. The
        // actual closed target gets a separately owned empty reader below.
        drop(stdin);
        let binding = Binding {
            session: request.member.binding.session.clone(),
            attempt: request.member.member_attempt,
        };
        let mut events = Events::new(events, binding.clone())?;
        events.produced(request.key.clone(), request.kind)?;
        events.emit(Event::Queued)?;
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("original produced phase missing"))?;
        let registered = state.produced.registered(&request.key)?;
        request.validate_registered(&registered.request)?;
        let gnu = state.phase.descriptions(&request.key)?;
        let handoff = jobserver_utils::ScopedHandoff::jobserver(
            [gnu[0].try_clone()?, gnu[1].try_clone()?],
            [3, 4],
        )?;
        let build = || -> io::Result<_> {
            registered
                .program
                .validate(native_inputs::deadline(Instant::now()))?;
            let mut command = spawnfd::command(registered.program.program());
            command
                .env_clear()
                .envs(
                    request
                        .member
                        .command
                        .environment
                        .iter()
                        .map(|(name, value)| {
                            (
                                std::ffi::OsStr::from_bytes(name),
                                std::ffi::OsStr::from_bytes(value),
                            )
                        }),
                )
                .envs(registered.program.runtime_environment()?)
                .args(
                    request
                        .member
                        .command
                        .args
                        .iter()
                        .map(|word| std::ffi::OsStr::from_bytes(word)),
                )
                .current_dir(std::ffi::OsStr::from_bytes(&request.member.command.cwd));
            let (prelude, mut command) = crate::metadata_prelude::Owner::new(
                &command,
                &[3, 4, sdk::TARGET as u32],
                native_inputs::deadline(Instant::now()),
            )?;
            // A direct peer's source handle is never consumed by these closed
            // producing profiles. The real target observes owned empty input.
            command
                .stdin(spawnfd::Stdio::from(File::open("/dev/null")?))
                .stdout(spawnfd::Stdio::from(stdout))
                .stderr(spawnfd::Stdio::from(stderr));
            Ok((prelude, command))
        };
        let state = match build() {
            Ok((prelude, command)) => {
                let timeout = request.member.timeout_ns.map(Duration::from_nanos);
                let attempt = member::OwnedAttempt::launch(
                    &mut member_custody::Borrowed {
                        job,
                        fence: &owner.fence,
                        debt: &owner.debt,
                        peer,
                        proof: None,
                        member: binding.attempt,
                        loan: None,
                        journal: &owner.journal,
                    },
                    command,
                    Resources {
                        registered,
                        request,
                        gnu,
                        handoff,
                        prelude,
                    },
                    events,
                    binding.clone(),
                    timeout,
                    crate::actor::wake,
                );
                State::Active(Box::new(Active {
                    attempt,
                    terminal: false,
                    settled: false,
                }))
            }
            Err(error) => {
                events.emit(Event::Refused {
                    reason: reason(&error),
                })?;
                State::Refused {
                    events: Box::new(events),
                    settled: false,
                }
            }
        };
        Ok(Self {
            binding,
            peer,
            state,
        })
    }
    pub(in crate::actor::owner) fn tick(
        &mut self,
        owner: &Owner,
        job: &mut Job,
        faulted: bool,
    ) -> io::Result<bool> {
        match &mut self.state {
            State::Refused { events, settled } => {
                if !*settled {
                    *settled = owner.journal.settle(&self.binding).is_ok()
                }
                if *settled && events.drained() {
                    job.workload
                        .as_mut()
                        .ok_or_else(|| refused("produced census disappeared"))?
                        .refused(self.binding.attempt)?;
                    return Ok(true);
                }
                Ok(false)
            }
            State::Active(active) => {
                let cause = if faulted {
                    Some(CompletionCause::ResourceLimit)
                } else if job.reaped || job.cleanup_at.is_some() {
                    Some(if job.cause == CompletionCause::Exited {
                        CompletionCause::Cancelled
                    } else {
                        job.cause
                    })
                } else {
                    None
                };
                let mut custody = member_custody::Borrowed {
                    job,
                    fence: &owner.fence,
                    debt: &owner.debt,
                    peer: self.peer,
                    proof: None,
                    member: self.binding.attempt,
                    loan: None,
                    journal: &owner.journal,
                };
                let progress = active.attempt.poll(&mut custody, cause);
                custody
                    .job
                    .workload
                    .as_mut()
                    .ok_or_else(|| refused("original produced census disappeared"))?
                    .observe(self.binding.attempt, active.attempt.identity(), &progress)?;
                if progress.uncertain
                    || (progress.launched && !progress.go_attempted)
                    || active.attempt.binding() != &self.binding
                {
                    return Err(refused("produced original native custody became unknown"));
                }
                if progress.ready_to_start && !faulted {
                    let (pid, birth) = active
                        .attempt
                        .identity()
                        .ok_or_else(|| refused("produced actual child identity unavailable"))?;
                    owner.journal.update(
                        &self.binding,
                        "member_bound",
                        aterm_json::json!({"pid":pid,"birth":birth}),
                    )?;
                    owner
                        .journal
                        .update(&self.binding, "member_running", aterm_json::json!({}))?;
                    match active.attempt.start(&mut custody) {
                        Ok(started) => custody
                            .job
                            .workload
                            .as_mut()
                            .ok_or_else(|| refused("produced census unavailable"))?
                            .started(self.binding.attempt, started)?,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) => return Err(error),
                    }
                }
                if progress.completed.is_some() && !active.terminal {
                    active.terminal = owner
                        .journal
                        .update(&self.binding, "member_terminal", aterm_json::json!({}))
                        .is_ok();
                }
                if progress.retired && !active.settled {
                    active.settled = owner.journal.settle(&self.binding).is_ok()
                }
                Ok(progress.retired && progress.drained && active.settled)
            }
        }
    }
}

fn environment(ready: &Ready, request: &wire::ProducedRequest) -> bool {
    let env = &request.member.command.environment;
    let fixed = |key: &[u8], value: &[u8]| {
        env.iter()
            .find(|(name, _)| name == key)
            .is_some_and(|(_, actual)| actual == value)
    };
    if !fixed(b"LANG", b"C")
        || !fixed(b"LC_ALL", b"C")
        || !fixed(b"CARGO", &ready.invocation.program.path)
        || !fixed(b"RUSTC", &ready.invocation.compiler.path)
        || !fixed(b"CARGO_HOME", &ready.invocation.cargo_home)
        || !fixed(b"CARGO_CACHE_RUSTC_INFO", b"0")
    {
        return false;
    }
    if env.iter().any(|(key, _)| {
        wire::protected_environment_name(key)
            || wire::loader_override_name(key)
            || matches!(
                key.as_slice(),
                b"RUSTC_WRAPPER" | b"RUSTC_WORKSPACE_WRAPPER"
            )
    }) {
        return false;
    }
    if request.kind != MemberKind::BuildScript {
        return request.member.command.cwd == ready.invocation.cwd
            && fixed(
                b"AY_PB_REFUTATION_PROOF_SRC",
                &ready.invocation.refutation_step.path,
            )
            && fixed(
                b"AY_PB_REFUTATION_LOOP_PROOF_SRC",
                &ready.invocation.refutation_loop.path,
            )
            && fixed(b"TRUST_SPEC_SRC", &ready.invocation.trust_spec_source.path)
            && fixed(b"TRUST_VC_DISABLE_VC_CACHE", b"1")
            && fixed(b"RUST_TEST_THREADS", b"1")
            && fixed(
                b"TRUST_TOOLCHAIN_BIN",
                &[ready.invocation.root.as_slice(), b"/bin"].concat(),
            );
    }
    // Normal Cargo package/DEP/scalar inputs are retained verbatim. They do
    // not authorize a spawned tool: no-fork still applies, and any future
    // delegated child needs its own actual input and custody owner.
    let cwd = std::path::Path::new(std::ffi::OsStr::from_bytes(&request.member.command.cwd));
    let under =
        |base: &[u8]| cwd.starts_with(std::path::Path::new(std::ffi::OsStr::from_bytes(base)));
    under(&ready.invocation.cwd) || under(&ready.invocation.cargo_home)
}

impl Owner {
    pub(in crate::actor) fn authorize_workload_produced(
        &mut self,
        peer: Peer,
        request: &wire::ProducedRequest,
    ) -> bool {
        if request.validate().is_err() || self.faulted || self.fence.check_current().is_err() {
            return false;
        }
        let binding = Binding {
            session: request.member.binding.session.clone(),
            attempt: request.member.member_attempt,
        };
        if !self.accepts(&binding) {
            return false;
        }
        let Some(job) = self.jobs.get_mut(&(
            request.member.binding.session.clone(),
            request.key.root_attempt,
        )) else {
            return false;
        };
        let capability = job
            .delegation
            .capability
            .as_bytes()
            .iter()
            .zip(request.member.binding.capability)
            .fold(0_u8, |diff, (a, b)| diff | (*a ^ b));
        if capability != 0
            || validate(
                job,
                peer,
                &wire::Registration {
                    key: request.key.clone(),
                    program: trust_resource_controller::workload::InputFile {
                        path: request.member.command.program.clone(),
                        sha256: request.program_sha256,
                    },
                    runtime_roots: request.runtime_roots.clone(),
                    producer_attempt: None,
                },
            )
            .is_err()
        {
            return false;
        }
        let Some(state) = job.workload.as_ref() else {
            return false;
        };
        let Some(active) = state.phase.active() else {
            return false;
        };
        let expected = match request.kind {
            MemberKind::BuildScript => host::phase::Kind::BuildScript,
            MemberKind::Inventory | MemberKind::Sample => host::phase::Kind::Harnesses,
            MemberKind::CompilerQuery => return false,
        };
        if active.kind != expected
            || state.produced.preparing()
            || job.members.values().any(|slot| !slot.is_produced())
            || !job.loans.is_empty()
            || !state.bank.drained()
        {
            return false;
        }
        let Ok(registered) = state.produced.registered(&request.key) else {
            return false;
        };
        request.validate_registered(&registered.request).is_ok()
            && state
                .ready
                .as_ref()
                .is_some_and(|ready| environment(ready, request))
            && registered
                .program
                .validate(native_inputs::deadline(Instant::now()))
                .is_ok()
    }
    pub(in crate::actor) fn submit_workload_produced(
        &mut self,
        incoming: crate::workload_transport::ProducedIncoming,
    ) -> io::Result<()> {
        if !self.authorize_workload_produced(incoming.peer, &incoming.request) {
            return Err(refused("produced request lost original active custody"));
        }
        let root = (
            incoming.request.member.binding.session.clone(),
            incoming.request.key.root_attempt,
        );
        let mut job = self
            .jobs
            .remove(&root)
            .ok_or_else(|| refused("original produced root unavailable"))?;
        let binding = Binding {
            session: incoming.request.member.binding.session.clone(),
            attempt: incoming.request.member.member_attempt,
        };
        let kind = match incoming.request.kind {
            MemberKind::BuildScript => CensusKind::BuildScript,
            MemberKind::Inventory => CensusKind::Inventory,
            MemberKind::Sample => CensusKind::Sample,
            MemberKind::CompilerQuery => {
                self.jobs.insert(root, job);
                return Err(refused("query cannot become produced"));
            }
        };
        let result = (|| {
            self.journal.create(&binding)?;
            job.workload
                .as_mut()
                .ok_or_else(|| refused("original produced census unavailable"))?
                .register(binding.attempt, kind)?;
            self.journal.update(&binding,"member_prepared",aterm_json::json!({"parent_session":job.binding.session,"parent_attempt":aterm_codec::hex::encode_lower(&job.binding.attempt),"phase":incoming.request.key.ordinal}))?;
            let member = Member::new(self, &mut job, incoming)?;
            job.members.insert(
                binding.attempt,
                member_queue::Slot::produced(binding, member),
            );
            Ok(())
        })();
        if result.is_err() {
            job.cleanup_at.get_or_insert_with(Instant::now);
            job.cause = CompletionCause::ResourceLimit;
            self.faulted = true;
        }
        self.jobs.insert(root, job);
        result
    }
}
