// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GUI-side connector. It retains a lane, never an actual solver/compiler child.
use crate::{enrollment::Challenge, forward::Forwarder, runtime};
use aterm_capacity::host_executable::{Instance, Program, Store};
use aterm_ctl::resource_admission_transport::{self as wire, AuthorizedSubmission, Peer};
use aterm_types::resource_admission::Submission;
use aterm_uds::{CtlStream, spawnfd};
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "retained custodian is not ready")
}

fn startup_status(
    status: &CtlStream,
    expected_pid: u32,
    started: Instant,
) -> io::Result<(aterm_json::Value, Instant)> {
    let deadline = runtime::control_deadline(started);
    let born = runtime::read_json(status, deadline)?;
    if born["version"].as_u64() != Some(3) || born["pid"].as_u64() != Some(u64::from(expected_pid))
    {
        return Err(refused());
    }
    Ok((born, deadline))
}
struct Connected {
    endpoint: PathBuf,
    lane: Arc<Forwarder>,
    program: Program,
    instance: Instance,
    observer: Arc<Mutex<Option<Arc<Forwarder>>>>,
}

struct Enrolled {
    lane: Forwarder,
    permit: Option<Arc<aterm_uds::secret::Secret>>,
}
struct HandoffAuthority {
    instance: Instance,
    permit: Arc<aterm_uds::secret::Secret>,
}
fn handoff_authority() -> &'static Mutex<Option<HandoffAuthority>> {
    static AUTHORITY: OnceLock<Mutex<Option<HandoffAuthority>>> = OnceLock::new();
    AUTHORITY.get_or_init(|| Mutex::new(None))
}

struct RestoredObserver {
    source: Peer,
    instance: Option<Instance>,
    permit: Arc<aterm_uds::secret::Secret>,
    lane: Mutex<Option<Arc<Forwarder>>>,
}
fn restored_observer() -> &'static Mutex<Option<Arc<RestoredObserver>>> {
    static RESTORED: OnceLock<Mutex<Option<Arc<RestoredObserver>>>> = OnceLock::new();
    RESTORED.get_or_init(|| Mutex::new(None))
}
fn connected() -> &'static Mutex<Option<Connected>> {
    static CONNECTED: OnceLock<Mutex<Option<Connected>>> = OnceLock::new();
    CONNECTED.get_or_init(|| Mutex::new(None))
}

/// A source retirement is independent of whether its successor could be read this time.
#[must_use]
pub struct ObservationRead {
    /// A custodian's native lifetime was proved ended, so its cursor is obsolete.
    pub source_ended: bool,
    /// Current independently enrolled source, or a retryable observation failure.
    pub result: io::Result<crate::resource_observation::Observation>,
}

fn observation_source() -> &'static Mutex<Option<Peer>> {
    static SOURCE: OnceLock<Mutex<Option<Peer>>> = OnceLock::new();
    SOURCE.get_or_init(|| Mutex::new(None))
}

fn cursor_for_source(previous: Option<Peer>, current: Peer, since: u64) -> u64 {
    if previous == Some(current) { since } else { 0 }
}

fn observe_lane(
    lane: &Forwarder,
    source: Peer,
    since: u64,
) -> io::Result<crate::resource_observation::Observation> {
    let since = cursor_for_source(
        *observation_source()
            .lock()
            .unwrap_or_else(|p| p.into_inner()),
        source,
        since,
    );
    let observed = lane.resource_observation(since)?;
    *observation_source()
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(source);
    Ok(observed)
}

fn source_lifetime_ended(
    expected: Peer,
    before: Option<aterm_uds::process::Identity>,
    after: Option<aterm_uds::process::Identity>,
    presence: aterm_uds::process::ProcessPresence,
) -> bool {
    use aterm_uds::process::{Birth, ProcessPresence};
    matches!(presence, ProcessPresence::Absent | ProcessPresence::Exited)
        || before
            .filter(|identity| Some(*identity) == after)
            .is_some_and(|identity| {
                u32::try_from(identity.pid).ok() == Some(expected.pid)
                    && identity.birth != Birth(expected.birth)
            })
}

fn restored_source_ended(observer: &RestoredObserver) -> bool {
    native_source_ended(observer.source)
}

fn native_source_ended(source: Peer) -> bool {
    let Ok(pid) = i32::try_from(source.pid) else {
        return false;
    };
    let before = aterm_uds::process::read_identity(pid);
    let presence = aterm_uds::process::process_presence(pid);
    let after = aterm_uds::process::read_identity(pid);
    source_lifetime_ended(source, before, after, presence)
}

fn retire_restored(observer: &Arc<RestoredObserver>) -> bool {
    let mut slot = restored_observer()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if slot
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, observer))
    {
        slot.take();
        true
    } else {
        false
    }
}

fn observe_restored<T>(
    source: &T,
    since: u64,
    mut ended: impl FnMut(&T) -> bool,
    mut retire: impl FnMut(&T) -> bool,
    mut read_source: impl FnMut(&T, u64) -> io::Result<crate::resource_observation::Observation>,
    read_successor: impl FnOnce(u64) -> io::Result<crate::resource_observation::Observation>,
) -> ObservationRead {
    if ended(source) && retire(source) {
        return ObservationRead {
            source_ended: true,
            result: read_successor(0),
        };
    }
    let result = read_source(source, since);
    if result.is_err() && ended(source) && retire(source) {
        ObservationRead {
            source_ended: true,
            result: read_successor(0),
        }
    } else {
        ObservationRead {
            source_ended: false,
            result,
        }
    }
}
fn open(endpoint: &Path) -> io::Result<Arc<Forwarder>> {
    let mut slot = connected().try_lock().map_err(|_| refused())?;
    if let Some(current) = slot.as_ref() {
        if current.endpoint != endpoint {
            return Err(refused());
        }
        if !current.lane.is_broken() {
            return Ok(current.lane.clone());
        }
    }
    // Read canonical readiness before any helper/cache creation. An observation
    // cannot create resource debt or implicitly adopt a live legacy namespace.
    crate::actor::prerequisites()?;
    let root = Store::account_path()?;
    let store = Store::prepare_at(&root)?;
    let program = store.cache_current()?;
    let mut selected = None;
    for instance in store.instances()? {
        if instance.validate().is_err() {
            continue;
        }
        if instance.program_path() != program.path() {
            // The prior maintained image may still control children. Do not
            // start a competing account owner across a GUI version upgrade.
            return Err(refused());
        }
        if selected.replace(instance).is_some() {
            return Err(refused());
        }
    }
    let (enrolled, instance) = match selected {
        Some(instance) => (connect_existing(endpoint, &program, &instance)?, instance),
        None => start(endpoint, &store, &program)?,
    };
    let lane = Arc::new(enrolled.lane);
    *handoff_authority()
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = enrolled.permit.map(|permit| HandoffAuthority {
        instance: instance.clone(),
        permit,
    });
    *slot = Some(Connected {
        endpoint: endpoint.to_owned(),
        lane: lane.clone(),
        program,
        instance,
        observer: Arc::new(Mutex::new(None)),
    });
    Ok(lane)
}
fn hello(endpoint: &Path, challenge: &Challenge) -> aterm_json::Value {
    aterm_json::json!({"version":3,"role":"gui",
        "endpoint_hex":aterm_codec::hex::encode_lower(endpoint.as_os_str().as_bytes()),
        "nonce":challenge.nonce(),"observer_reconnect":"permit-v1"})
}
fn finish(
    endpoint: &Path,
    program: &Program,
    instance: &Instance,
    stream: CtlStream,
    deadline: Instant,
) -> io::Result<Enrolled> {
    instance.validate()?;
    let challenge = Challenge::begin(instance, program, endpoint)?;
    runtime::write_json(&stream, &hello(endpoint, &challenge), deadline)?;
    let permit = gui_ack_permit(&wire::read_frame(&stream, deadline)?)?;
    instance.validate()?;
    Ok(Enrolled {
        lane: Forwarder::new(
            stream,
            Peer {
                pid: instance.pid(),
                birth: instance.birth().0,
            },
        )?,
        permit,
    })
}
fn connect_existing(
    endpoint: &Path,
    program: &Program,
    instance: &Instance,
) -> io::Result<Enrolled> {
    let deadline = runtime::control_deadline(Instant::now());
    instance.validate()?;
    let stream = aterm_uds::connect::until(instance.socket_path(), deadline)?;
    if wire::local_peer(&stream)?
        != (Peer {
            pid: instance.pid(),
            birth: instance.birth().0,
        })
    {
        return Err(refused());
    }
    finish(endpoint, program, instance, stream, deadline)
}
fn start(endpoint: &Path, store: &Store, program: &Program) -> io::Result<(Enrolled, Instance)> {
    let (control, child_control) = aterm_uds::kept_out(CtlStream::pair)?;
    let (status, child_status) = aterm_uds::kept_out(CtlStream::pair)?;
    let mut command = spawnfd::LaneCommand::new(program.path());
    command
        .arg(runtime::VERB)
        .stdin(spawnfd::ChildStdio::Fd(File::open("/dev/null")?.into()))
        .stdout(spawnfd::ChildStdio::Fd(
            OpenOptions::new().write(true).open("/dev/null")?.into(),
        ))
        .stderr(spawnfd::ChildStdio::Fd(
            OpenOptions::new().write(true).open("/dev/null")?.into(),
        ));
    program.validate()?;
    let child = spawnfd::detached_with_two_fds(command, child_control.into(), child_status.into())?;
    let (born, deadline) = startup_status(&status, child.id(), Instant::now())?;
    let birth = born["birth"].as_u64().ok_or_else(refused)?;
    let socket = runtime::path_field(&born, "socket_hex")?;
    let instance = store
        .instances()?
        .into_iter()
        .find(|instance| {
            instance.pid() == child.id()
                && instance.birth().0 == birth
                && instance.socket_path() == socket
                && instance.program_path() == program.path()
        })
        .ok_or_else(refused)?;
    // Never kill a published helper on a constructor timeout: another enrolled
    // GUI may already use the same account custodian. Actual LaneChild drop
    // preserves native reap custody without terminating that process.
    Ok((
        finish(endpoint, program, &instance, control, deadline)?,
        instance,
    ))
}

/// The exact Instance owner dispatcher supplies its real accepted listener path.
pub fn capabilities_at(endpoint: &Path) -> aterm_json::Value {
    open(endpoint)
        .and_then(|lane| lane.capabilities())
        .unwrap_or_else(|_| {
            aterm_types::resource_inspection::unavailable("retained custodian unavailable")
        })
}
/// Observe checked original custody through the enrolled GUI, without admission.
pub fn inspect_at(endpoint: &Path, peer: Peer, session: &str) -> io::Result<aterm_json::Value> {
    open(endpoint)?.inspect(peer, session)
}
/// Read origin and original-owning-wait facts over the already enrolled private custodian lane.
/// This never starts a backend or accepts a caller-provided process identity.
/// A failed read remains retryable. Only a kernel-proven ended restored source retires its
/// lane; its successor is then queried from the beginning of that source's own cursor.
pub fn resource_observation(since: u64) -> ObservationRead {
    let restored = restored_observer()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if let Some(restored) = restored {
        return observe_restored(
            &restored,
            since,
            |source| restored_source_ended(source),
            retire_restored,
            |source, since| {
                let lane = observer_connection(source)?;
                observe_lane(&lane, source.source, since)
            },
            |since| current_observation(since).result,
        );
    }
    current_observation(since)
}

fn current_observation(since: u64) -> ObservationRead {
    let mut source_ended = false;
    let result = current_observation_at(since, &mut source_ended);
    ObservationRead {
        source_ended,
        result,
    }
}

fn current_observation_at(
    since: u64,
    source_ended: &mut bool,
) -> io::Result<crate::resource_observation::Observation> {
    let (endpoint, program, instance, observer) = {
        let connected = connected().try_lock().map_err(|_| refused())?;
        let connected = connected.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "native custodian is not enrolled",
            )
        })?;
        (
            connected.endpoint.clone(),
            connected.program.clone(),
            connected.instance.clone(),
            connected.observer.clone(),
        )
    };
    let source = Peer {
        pid: instance.pid(),
        birth: instance.birth().0,
    };
    if native_source_ended(source) {
        *source_ended = true;
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "native observation source ended",
        ));
    }
    let lane = {
        // This lock belongs only to the read-only connection. Sampling cannot take the
        // execution lane's try_lock and spuriously refuse authorize/submit requests.
        let mut observer = observer.try_lock().map_err(|_| refused())?;
        if observer.as_ref().is_none_or(|lane| lane.is_broken()) {
            *observer = Some(Arc::new(
                connect_existing(&endpoint, &program, &instance)?.lane,
            ));
        }
        observer.as_ref().unwrap().clone()
    };
    observe_lane(&lane, source, since)
}

fn observer_connection(observer: &RestoredObserver) -> io::Result<Arc<Forwarder>> {
    let mut lane = observer.lane.try_lock().map_err(|_| refused())?;
    if lane.as_ref().is_none_or(|lane| lane.is_broken()) {
        let instance = observer
            .instance
            .clone()
            .map_or_else(|| observation_instance(observer.source), Ok)?;
        instance.validate()?;
        let deadline = runtime::control_deadline(Instant::now());
        let stream = aterm_uds::connect::until(instance.socket_path(), deadline)?;
        let expected = observer.source;
        if wire::local_peer(&stream)? != expected {
            return Err(refused());
        }
        let permit = std::str::from_utf8(observer.permit.expose()).map_err(|_| refused())?;
        runtime::write_json(
            &stream,
            &aterm_json::json!({"version":3,"role":"resource-observer-v1","permit":permit}),
            deadline,
        )?;
        if wire::read_frame(&stream, deadline)?.as_slice() != [1] {
            return Err(refused());
        }
        instance.validate()?;
        *lane = Some(Arc::new(Forwarder::new(stream, expected)?));
    }
    Ok(lane.as_ref().unwrap().clone())
}

/// Encode only cached read-only observer authority for the private GUI handoff. This does not
/// enroll a backend, issue an admission request or make the UI wait on a telemetry read.
pub fn carry_resource_observer() -> Option<aterm_json::Value> {
    if let Some(observer) = restored_observer()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
    {
        return Some(observer_value(observer.source, &observer.permit));
    }
    let authority = handoff_authority()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let authority = authority.as_ref()?;
    Some(observer_value(
        Peer {
            pid: authority.instance.pid(),
            birth: authority.instance.birth().0,
        },
        &authority.permit,
    ))
}

fn gui_ack_permit(ack: &[u8]) -> io::Result<Option<Arc<aterm_uds::secret::Secret>>> {
    if ack.first() != Some(&1) {
        return Err(refused());
    }
    // Enrollment success is independent of optional observer metadata. A missing or malformed
    // read-only grant never turns an authenticated execution lane into a refused admission.
    Ok(std::str::from_utf8(&ack[1..])
        .ok()
        .filter(|text| aterm_codec::hex::is_lower(text, 64))
        .map(|text| Arc::new(aterm_uds::secret::Secret::new(text.as_bytes()))))
}

fn observer_value(source: Peer, permit: &aterm_uds::secret::Secret) -> aterm_json::Value {
    aterm_json::json!({"version":1,"pid":source.pid,"birth":source.birth,
        "permit":std::str::from_utf8(permit.expose()).unwrap_or("")})
}

fn observation_instance(source: Peer) -> io::Result<Instance> {
    Store::open_at(&Store::account_path()?)?
        .instances()?
        .into_iter()
        .find(|instance| instance.pid() == source.pid && instance.birth().0 == source.birth)
        .ok_or_else(refused)
}

fn observer_context(value: &aterm_json::Value) -> io::Result<RestoredObserver> {
    if value["version"].as_u64() != Some(1) {
        return Err(refused());
    }
    let pid = value["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 1 && i32::try_from(*pid).is_ok())
        .ok_or_else(refused)?;
    let birth = value["birth"].as_u64().ok_or_else(refused)?;
    let permit = value["permit"]
        .as_str()
        .filter(|text| aterm_codec::hex::is_lower(text, 64))
        .ok_or_else(refused)?;
    let source = Peer { pid, birth };
    Ok(RestoredObserver {
        source,
        instance: None,
        permit: Arc::new(aterm_uds::secret::Secret::new(permit.as_bytes())),
        lane: Mutex::new(None),
    })
}

/// Restore read-only authority handed off by an enrolled predecessor. Before any connection,
/// the source must match an immutable store instance and native incarnation. A failed read
/// retains pending handoff context for retry or a kernel-proven retirement; a PID or pathname
/// alone never enrolls it. The restored capability cannot authorize or submit execution.
/// # Errors
/// Malformed authority, unavailable provenance, a changed native source or a refused permit.
pub fn restore_resource_observer(value: &aterm_json::Value) -> io::Result<()> {
    let mut context = observer_context(value)?;
    context.instance = observation_instance(context.source).ok();
    let observer = Arc::new(context);
    // Intake establishes the successor's observation connection before the predecessor's
    // normal GUI connection is released; a later poll is not the handoff boundary.
    *restored_observer()
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(observer.clone());
    observer_connection(&observer).map(|_| ())
}

/// Release a handed-off source only after its work and final observations are adopted. An old
/// idle custodian can then retire so the current GUI image can enroll its own execution owner.
/// Returns whether a handed-off connection was released.
pub fn finish_resource_observer() -> bool {
    restored_observer()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .is_some()
}
fn existing() -> io::Result<Arc<Forwarder>> {
    connected()
        .try_lock()
        .map_err(|_| refused())?
        .as_ref()
        .filter(|current| !current.lane.is_broken())
        .map(|current| current.lane.clone())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "native custodian is not enrolled",
            )
        })
}
/// Forward only after the GUI has independently authenticated the live session.
pub fn authorize(request: &Submission, peer: Peer) -> bool {
    request.peer_pid == peer.pid
        && request.peer_birth == peer.birth
        && request.validate().is_ok()
        && existing()
            .and_then(|lane| lane.authorize(request))
            .unwrap_or(false)
}
/// Delivery remains ambiguous on a lost reply. No new attempt is retried here.
pub fn submit(submission: AuthorizedSubmission) -> io::Result<()> {
    existing()?.submit(submission)
}
/// Submit only through the explicitly negotiated owning-wait accounting route.
/// Delivery failure is ambiguous and never permits a second attempt.
pub fn submit_accounted(submission: AuthorizedSubmission) -> io::Result<()> {
    existing()?.submit_accounted(submission)
}
/// The GUI selects/enrolls its backend independently. The offered directory is
/// only an expectation on that actual backend, never a discovery locator.
pub fn matches_original_store_at(
    endpoint: &Path,
    expected: &std::os::fd::OwnedFd,
) -> io::Result<bool> {
    open(endpoint)?.matches_original_store(expected)
}
/// Recheck the original expectation on the SAME enrolled lane before its
/// actual root admission. A broken lane is never rediscovered or retried.
pub fn submit_accounted_expected(
    submission: AuthorizedSubmission,
    expected: std::os::fd::OwnedFd,
) -> io::Result<()> {
    existing()?.submit_accounted_expected(submission, expected)
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
