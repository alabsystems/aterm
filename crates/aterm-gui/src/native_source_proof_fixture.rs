// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rust-only joined Source host. Real native authentication and dispatch are
//! shared with the existing host fixture; no image/capability override exists.
use super::*;
use aterm_agent::harness::{
    account_work, cli::Env, codex_enrollment::SourceProofLaunch, codex_session_slot::Caller,
    native_act::Process,
};
use aterm_window::platform::pump_events::EventLoopExtPumpEvents;
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "native_source_proof_fault.rs"]
mod fault;

#[derive(Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    schema: u32,
    request: String,
    action: String,
    #[serde(default)]
    member: Option<String>,
    #[serde(default)]
    credential_generation: Option<u64>,
}

#[derive(Default)]
struct Trace {
    caller: Option<Caller>,
    ready_seen: bool,
    closed_seen: bool,
    ready_evidence: bool,
    closed_members: std::collections::BTreeSet<String>,
    outcomes: std::collections::VecDeque<aterm_json::Value>,
    outcome_sequence: u64,
    ready_loss: Option<fault::ReadyLoss>,
    last: Option<aterm_agent::harness::codex_source_registry::SourceProofObservation>,
}

fn all_members_closed(
    closed: &std::collections::BTreeSet<String>,
    epoch: &str,
    highwater: u64,
) -> bool {
    u64::try_from(closed.len()).ok() == Some(highwater)
        && closed.iter().all(|member| {
            member
                .strip_prefix(epoch)
                .and_then(|ordinal| u64::from_str_radix(ordinal, 16).ok())
                .is_some_and(|ordinal| ordinal > 0 && ordinal <= highwater)
        })
}

// The guardian is nonblocking so the fixture can pump native events. Preserve
// the exact offset under backpressure; never retry a partially sent frame whole.
fn write_guardian(
    stream: &mut CtlStream,
    bytes: &[u8],
    mut pump: impl FnMut(),
) -> Result<(), String> {
    let deadline = Instant::now() + MUST_HAPPEN;
    let mut offset = 0;
    while offset < bytes.len() {
        pump();
        if Instant::now() >= deadline {
            return Err("source proof guardian write deadline".into());
        }
        match stream.write(&bytes[offset..]) {
            Ok(0) => return Err("source proof guardian closed during reply".into()),
            Ok(written) => offset += written,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let mut poll = libc::pollfd {
                    fd: std::os::fd::AsRawFd::as_raw_fd(stream),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: this fixture retains the exact guardian descriptor.
                if unsafe { libc::poll(&mut poll, 1, 10) } < 0
                    && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                {
                    return Err("source proof guardian write poll failed".into());
                }
            }
            Err(_) => return Err("source proof guardian reply failed".into()),
        }
    }
    Ok(())
}

struct Pty {
    master: i32,
    shell: aterm_pty::ShellIdentity,
}
struct ChildGuard(aterm_uds::spawnfd::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        aterm_pty::hangup_shell(&self.shell, self.master);
        aterm_pty::close_fd(self.master);
    }
}

/// This entry exists only in the original debug fixture artifact. Its task is
/// machine-MACed by the constructor-owned private world, never a raw image/path
/// admission. Child stdout is transferred as its actual descriptor.
pub fn run_native_source_proof_fixture(frame: String) -> Result<(), String> {
    let env = Env::from_process()?;
    let home = env.home.as_deref().ok_or("source fixture HOME missing")?;
    let lifetime = account_work::Lifetime::default();
    let scope = lifetime
        .enter()
        .ok_or("source fixture App admission closed")?;
    // The app's own constructor, as the native host fixture's: its registry is bound to the
    // fixture's lifetime and is the one its dispatches lend.
    let account_services = AccountServices::for_app(lifetime.clone());
    let registry = Arc::clone(&account_services.source_registry);
    let mut builder = crate::quiet_driver_event_loop_builder::<Wake>();
    builder.with_headless();
    let mut event_loop = builder
        .build()
        .map_err(|_| "source fixture event loop unavailable")?;
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let mut handle = super::super::control_redraw_conformance::registered_session(1, &term);
    let sid = handle.sid.as_str().to_owned();
    let launch = SourceProofLaunch::admit(&env, frame.trim_end(), &lifetime, &scope, &sid)?;
    let root = launch.directory().to_path_buf();
    let socket = root.join("control.sock");
    let socket_text = socket
        .to_str()
        .ok_or("source fixture socket path")?
        .to_owned();
    let token = control_auth::random_token_hex().ok_or("source fixture entropy unavailable")?;
    let token_names = aterm_types::control_socket::token_names_for_sock("control.sock");
    aterm_uds::private_file::write_private_new(
        &root,
        &token_names[0],
        format!("{token}\n").as_bytes(),
        |_| Ok(()),
    )
    .map_err(|_| "source fixture token unavailable")?
    .then_some(())
    .ok_or("source fixture token already exists")?;
    crate::proxy::set_self_sock(&root, &socket_text);
    let runtime = root.join("runtime");
    let graph = runtime.join("aterm");
    aterm_secrets::fsx::ensure_private_dir(&runtime)
        .map_err(|_| "source fixture runtime unavailable")?;
    aterm_secrets::fsx::ensure_private_dir(&graph)
        .map_err(|_| "source fixture runtime unavailable")?;
    crate::proxy::publish_graph_entry(&graph, &handle.sid, &socket_text, &handle.nonce);
    let mut environment = vec![
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        ("ATERM_SESSION_ID".into(), sid.clone()),
        ("ATERM_PARENT_SESSION_ID".into(), sid.clone()),
        (
            "XDG_RUNTIME_DIR".into(),
            runtime.to_string_lossy().into_owned(),
        ),
        (
            "XDG_CONFIG_HOME".into(),
            root.join("cfg").to_string_lossy().into_owned(),
        ),
        (
            "ATERM_HARNESS_STATE".into(),
            env.state.to_string_lossy().into_owned(),
        ),
        ("ATERM_CHILD".into(), "1".into()),
        ("TERM_PROGRAM".into(), "aterm".into()),
    ];
    if let Some(state) = &env.aterm_state {
        environment.push((
            "ATERM_STATE_HOME".into(),
            state.to_string_lossy().into_owned(),
        ));
    }
    // SAFETY: this explicit Rust debug host owns the isolated PTY and its child.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let shell_args = ["-f".into(), "-i".into()];
    let shell = aterm_pty::spawn_shell_with_pid(
        24,
        80,
        &spawn,
        &sandbox,
        &environment,
        Some("/bin/zsh"),
        Some(&shell_args),
        None,
        None,
        home.to_str(),
        None,
        aterm_sandbox::Limits::inherit(),
    )
    .map_err(|_| "source fixture PTY unavailable")?;
    let pty = Pty {
        master: shell.master,
        shell: aterm_pty::record_spawned_shell(shell.pid),
    };
    handle.master = pty.master;
    handle.shell_identity = pty.shell;
    Arc::get_mut(&mut handle.ctx)
        .ok_or("source fixture context already shared")?
        .sink = Arc::new(aterm_session::sink::SinkWriter::new(pty.master));
    let store = crate::session_store::new_store();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle.clone());
    let context = Arc::new(ControlWorkerContext {
        active: Arc::new(Mutex::new(Some(ActiveSession {
            term: Arc::clone(&term),
            master: pty.master,
            id: handle.local_id,
            ctx: Arc::clone(&handle.ctx),
        }))),
        store,
        proxy: event_loop.create_proxy(),
        account_services,
        token: Arc::new(token),
        sock_dir: root.clone(),
        recovery_count_admission: Arc::new(
            crate::keeper_link::RecoveryCountAdmission::from_retained(Vec::new()),
        ),
    });
    let listener = CtlListener::bind(&socket).map_err(|_| "source fixture listener unavailable")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "source fixture listener unavailable")?;
    let stopping = Arc::new(AtomicBool::new(false));
    let trace = Arc::new(Mutex::new(Trace::default()));
    let observed_trace = Arc::clone(&trace);
    let observed_registry = Arc::clone(&registry);
    let observe: Arc<SourceOutcomeObserver<'static>> =
        Arc::new(move |peer, connection, outcome| {
            let caller = observed_trace
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .caller
                .clone();
            let Some(caller) = caller else {
                return false;
            };
            let mut fault_candidate = None;
            // Completion evidence is real production dispatch, independent of a
            // census which may be Busy while another member changes its mapping.
            // Preserve it boundedly; evidence alone never proves Ready or Closed.
            if peer == Some(caller.pid)
                && let Some(mut outcome) = outcome
            {
                let mut trace = observed_trace.lock().unwrap_or_else(|p| p.into_inner());
                let action = outcome["action"].as_str().unwrap_or_default();
                trace.ready_evidence |= matches!(action, "member-ready" | "member-rebound-ready");
                if matches!(action, "member-retire" | "member-retire-owned")
                    && let Some(member) = outcome["member"].as_str()
                    && trace.closed_members.len() < 4096
                {
                    trace.closed_members.insert(member.to_owned());
                }
                let Some(sequence) = trace.outcome_sequence.checked_add(1) else {
                    return false;
                };
                trace.outcome_sequence = sequence;
                if let Some(object) = outcome.as_object_mut() {
                    object.insert("sequence".into(), aterm_json::json!(sequence));
                    object.insert("connection".into(), aterm_json::json!(connection));
                }
                if trace
                    .ready_loss
                    .as_ref()
                    .is_some_and(|loss| loss.candidate(&outcome))
                {
                    fault_candidate = Some(outcome.clone());
                }
                trace.outcomes.push_back(outcome);
                while trace.outcomes.len() > 32 {
                    trace.outcomes.pop_front();
                }
            }
            let until = Instant::now() + MUST_HAPPEN;
            loop {
                let observation = observed_registry.observe_source_proof(&caller);
                if let Err(error) = &observation {
                    // This explicit fixture records only the closed error enum,
                    // never identifiers, credential data or a readiness grant.
                    eprintln!("Source qualification native: observation failed {error:?}");
                }
                if let Ok(observation) = observation {
                    let current_fault = fault_candidate
                        .as_ref()
                        .is_some_and(|outcome| fault::ReadyLoss::current(outcome, &observation));
                    let mut trace = observed_trace.lock().unwrap_or_else(|p| p.into_inner());
                    trace.ready_seen |= trace.ready_evidence
                        && observation.managers_ready > 0
                        && observation.unresolved == 0;
                    let every_member_closed = all_members_closed(
                        &trace.closed_members,
                        &observation.member_epoch,
                        observation.member_highwater,
                    );
                    trace.closed_seen =
                        every_member_closed && trace.ready_seen && observation.closed();
                    trace.last = Some(observation);
                    if current_fault && let Some(candidate) = fault_candidate.as_ref() {
                        return trace
                            .ready_loss
                            .as_mut()
                            .is_some_and(|loss| loss.accept(candidate));
                    }
                }
                if fault_candidate.is_none() || Instant::now() >= until {
                    return false;
                }
                // No stale trace fallback can trigger the fault. The real reply
                // remains unsent while a briefly Busy coherent probe retries.
                std::thread::sleep(Duration::from_millis(1));
            }
        });
    let server_stop = Arc::clone(&stopping);
    let server_context = Arc::clone(&context);
    let server_observe = Arc::clone(&observe);
    let server = std::thread::Builder::new()
        .name("source-proof-native-control".into())
        .spawn(move || {
            let mut workers: Vec<(CtlStream, std::thread::JoinHandle<io::Result<bool>>)> =
                Vec::new();
            let completed = |worker: std::thread::JoinHandle<io::Result<bool>>| -> io::Result<()> {
                match worker
                    .join()
                    .map_err(|_| failure("source fixture control worker panicked"))?
                {
                    Ok(_) => Ok(()),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::BrokenPipe
                                | io::ErrorKind::ConnectionReset
                                | io::ErrorKind::ConnectionAborted
                                | io::ErrorKind::UnexpectedEof
                        ) =>
                    {
                        // The connection scope still performs actual native EOF
                        // cleanup. A lost reply is never a Closed receipt, and
                        // the listener must remain available for reconciliation.
                        eprintln!(
                            "Source qualification native: control channel disconnected {:?}",
                            error.kind()
                        );
                        Ok(())
                    }
                    Err(error) => {
                        eprintln!("Source qualification native: control worker failed: {error}");
                        Err(error)
                    }
                }
            };
            while !server_stop.load(Ordering::Acquire) {
                let mut index = 0;
                while index < workers.len() {
                    if workers[index].1.is_finished() {
                        let (_, worker) = workers.swap_remove(index);
                        completed(worker)?;
                    } else {
                        index += 1;
                    }
                }
                match aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Ready,
                    aterm_uds::AcceptedMode::Blocking,
                ) {
                    Ok((stream, _)) => {
                        if workers.len() >= 256 {
                            drop(stream);
                            continue;
                        }
                        let held = stream.try_clone()?;
                        let context = Arc::clone(&server_context);
                        let observe = Arc::clone(&server_observe);
                        workers.push((
                            held,
                            std::thread::spawn(move || {
                                serve_connection_observed(&context, stream, true, Some(&*observe))
                            }),
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => return Err(error),
                }
            }
            for (stream, worker) in workers {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                completed(worker)?;
            }
            Ok::<_, io::Error>(())
        })
        .map_err(|_| "source fixture control thread unavailable")?;
    let result = (|| -> Result<(), String> {
        let mut owned_child = ChildGuard(crate::task_launch::spawn_source_proof(
            &env,
            &sid,
            Arc::clone(&registry),
            scope,
            &launch,
            &environment,
        )?);
        let child = &mut owned_child.0;
        let process =
            Process::observe(child.id()).ok_or("source proof kernel child unavailable")?;
        let caller = Caller {
            sid: sid.clone(),
            pid: process.pid,
            birth: process.birth,
        };
        trace.lock().unwrap_or_else(|p| p.into_inner()).caller = Some(caller.clone());
        let input = child
            .stdin
            .take()
            .ok_or("source proof child stdin unavailable")?;
        let output = child
            .stdout
            .take()
            .ok_or("source proof child stdout unavailable")?;
        let guardian = CtlStream::connect(launch.guardian())
            .map_err(|_| "source proof guardian unavailable")?;
        let driver_pid = aterm_uds::fdpass::peer_pid(&guardian).map_err(|error| {
            format!(
                "source proof guardian kernel identity unavailable kind={:?} errno={:?}",
                error.kind(),
                error.raw_os_error()
            )
        })?;
        if !launch.guardian_peer_current(driver_pid) {
            return Err(format!(
                "source proof guardian kernel identity changed: {}",
                launch.guardian_peer_failure(driver_pid)
            ));
        }
        let header = aterm_json::to_vec(&aterm_json::json!({"schema":1,"sid":sid,
            "hostPid":std::process::id(),"pid":process.pid,"birth":process.birth}))
        .map_err(|_| "source proof metadata encoding failed")?;
        if aterm_uds::fdpass::send_with_fds(&guardian, &header, &[input.as_fd(), output.as_fd()])
            .map_err(|_| "source proof descriptor transfer failed")?
            != header.len()
        {
            return Err("source proof descriptor transfer incomplete".into());
        }
        drop((input, output));
        guardian
            .set_nonblocking(true)
            .map_err(|_| "source proof guardian unavailable")?;
        let mut guardian = guardian;
        let mut commands = Vec::new();
        let mut pending = None;
        let mut promoted_generation = None;
        let mut receipts = std::collections::VecDeque::<(u64, Command, Vec<u8>)>::new();
        let mut request_prefix = None;
        let mut highwater = 0u64;
        let mut next_retry = Instant::now();
        let mut pump = super::renewal::Pump::source_fixture(handle, term);
        let deadline = Instant::now() + Duration::from_secs(30 * 60);
        let status = loop {
            event_loop.pump_app_events(Some(Duration::from_millis(10)), &mut pump);
            let mut bytes = [0u8; 4096];
            match guardian.read(&mut bytes) {
                Ok(0) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("source proof guardian departed".into());
                }
                Ok(length) => commands.extend_from_slice(&bytes[..length]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => return Err("source proof guardian read failed".into()),
            }
            if commands.len() > 4096 {
                return Err("source proof guardian frame too large".into());
            }
            if let Some(end) = commands.iter().position(|byte| *byte == b'\n') {
                let command: Command = aterm_json::from_slice(&commands[..end])
                    .map_err(|_| "invalid source proof guardian command")?;
                commands.drain(..=end);
                if !launch.guardian_peer_current(driver_pid) {
                    return Err(format!(
                        "source proof guardian process changed: {}",
                        launch.guardian_peer_failure(driver_pid)
                    ));
                }
                if command.schema != 1
                    || !aterm_codec::hex::is_lower(&command.request, 32)
                    || !matches!(
                        command.action.as_str(),
                        "advance" | "observe" | "arm-ready-loss"
                    )
                    || (command.action == "arm-ready-loss")
                        != (command.member.is_some() && command.credential_generation.is_some())
                    || (command.action != "arm-ready-loss"
                        && (command.member.is_some() || command.credential_generation.is_some()))
                {
                    return Err("invalid source proof guardian command".into());
                }
                let prefix = &command.request[..16];
                let counter = u64::from_str_radix(&command.request[16..], 16)
                    .map_err(|_| "invalid source proof request counter")?;
                if request_prefix.as_ref().is_some_and(|old| old != prefix) || counter == 0 {
                    return Err("source proof request namespace changed".into());
                }
                if counter <= highwater {
                    let (_, original, reply) = receipts
                        .iter()
                        .find(|(old, _, _)| *old == counter)
                        .ok_or("source proof request is stale")?;
                    if original != &command {
                        return Err("source proof guardian request changed".into());
                    }
                    write_guardian(&mut guardian, reply, || {
                        event_loop.pump_app_events(Some(Duration::ZERO), &mut pump);
                    })?;
                    continue;
                }
                if highwater.checked_add(1) != Some(counter) {
                    return Err("source proof request sequence changed".into());
                }
                request_prefix = Some(prefix.to_owned());
                if command.action == "arm-ready-loss" {
                    let member = command
                        .member
                        .as_deref()
                        .ok_or("ready-loss member missing")?;
                    let generation = command
                        .credential_generation
                        .ok_or("ready-loss generation missing")?;
                    if !aterm_codec::hex::is_lower(member, 32) || generation == 0 {
                        return Err("invalid ready-loss plan".into());
                    }
                    let mut trace = trace.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(loss) = &trace.ready_loss {
                        if !loss.same_plan(member, generation) {
                            return Err("ready-loss plan changed".into());
                        }
                    } else {
                        // This plan is only a negative fault, never admission.
                        // Its baseline is the already coherent native snapshot
                        // checked by the driver after the preceding EOF leg.
                        let observation = trace
                            .last
                            .as_ref()
                            .ok_or("ready-loss native observation unavailable")?;
                        let plan = fault::ReadyLoss::arm(observation, member, generation)?;
                        trace.ready_loss = Some(plan);
                    }
                }
                if command.action == "advance" && pending.is_none() {
                    match launch.advance(&env) {
                        aterm_agent::harness::codex_enrollment::SourceProofAdvance::Current(
                            generation,
                        ) => promoted_generation = Some(generation),
                        aterm_agent::harness::codex_enrollment::SourceProofAdvance::Pending(
                            retained,
                        ) => pending = Some(retained),
                        aterm_agent::harness::codex_enrollment::SourceProofAdvance::Refused => {
                            return Err("source proof refresh fixture refused".into());
                        }
                    }
                }
                observe(None, None, None);
                let trace = trace.lock().unwrap_or_else(|p| p.into_inner());
                let outcomes = trace.outcomes.iter().collect::<Vec<_>>();
                let mut reply = aterm_json::to_vec(&aterm_json::json!({"schema":1,
                    "request":command.request,"action":command.action,"outcome":if pending.is_some() {"pending"} else {"ok"},
                    "generation":promoted_generation,"observation":trace.last.as_ref(),"readySeen":trace.ready_seen,"closedSeen":trace.closed_seen,"outcomes":outcomes,"outcomeSequence":trace.outcome_sequence,"readyLoss":trace.ready_loss}))
                    .map_err(|_| "source proof guardian encoding failed")?;
                if reply.len()
                    > aterm_agent::harness::codex_source_registry::SOURCE_PROOF_OBSERVATION_LIMIT
                {
                    return Err("source proof observation exceeds bounded reply".into());
                }
                reply.push(b'\n');
                drop(trace);
                write_guardian(&mut guardian, &reply, || {
                    event_loop.pump_app_events(Some(Duration::ZERO), &mut pump);
                })?;
                highwater = counter;
                receipts.push_back((counter, command, reply));
                while receipts.len() > 8 {
                    receipts.pop_front();
                }
            }
            if Instant::now() >= next_retry
                && let Some(retained) = pending.take()
            {
                match retained.retry(&env) {
                    aterm_agent::harness::codex_enrollment::SourceProofAdvance::Current(
                        generation,
                    ) => promoted_generation = Some(generation),
                    aterm_agent::harness::codex_enrollment::SourceProofAdvance::Pending(
                        retained,
                    ) => pending = Some(retained),
                    aterm_agent::harness::codex_enrollment::SourceProofAdvance::Refused => {
                        return Err("source proof retained fixture refused".into());
                    }
                }
                next_retry = Instant::now() + Duration::from_millis(200);
            }
            if server.is_finished() {
                return Err("source fixture control server ended while SDK was running".into());
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|_| "source proof child wait failed")?
            {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("source proof child hang detector expired".into());
            }
            observe(None, None, None);
        };
        if !status.success() {
            return Err("source proof SDK child failed".into());
        }
        let trace = trace.lock().unwrap_or_else(|p| p.into_inner());
        if !trace.ready_seen || !trace.closed_seen || pending.is_some() {
            eprintln!(
                "Source qualification native: final ready={} closed={} closed_receipts={} issued={:?} members={:?} highwater={:?} unresolved={:?} custody={:?} active={:?} pending={:?} ending={:?}",
                trace.ready_seen,
                trace.closed_seen,
                trace.closed_members.len(),
                trace.last.as_ref().map(|last| last.issued),
                trace.last.as_ref().map(|last| last.members),
                trace.last.as_ref().map(|last| last.member_highwater),
                trace.last.as_ref().map(|last| last.unresolved),
                trace.last.as_ref().map(|last| last.unsettled_custody),
                trace.last.as_ref().map(|last| last.active_uses),
                trace.last.as_ref().map(|last| last.pending_uses),
                trace.last.as_ref().map(|last| last.ending_uses),
            );
            return Err("source proof did not observe native Ready and explicit Closed".into());
        }
        Ok(())
    })();
    stopping.store(true, Ordering::Release);
    let served = server
        .join()
        .map_err(|_| "source fixture server panicked")?
        .map_err(|error| format!("source fixture server failed: {error}"));
    if let Err(error) = &served {
        eprintln!("Source qualification native: {error}");
    }
    drop((context, launch, pty));
    result?;
    served
}

#[cfg(test)]
#[path = "native_source_proof_fixture_source_closure_tests.rs"]
mod source_closure_tests;
