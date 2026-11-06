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
    time::{Duration, Instant},
};

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "retained custodian is not ready")
}
struct Connected {
    endpoint: PathBuf,
    lane: Arc<Forwarder>,
}
fn connected() -> &'static Mutex<Option<Connected>> {
    static CONNECTED: OnceLock<Mutex<Option<Connected>>> = OnceLock::new();
    CONNECTED.get_or_init(|| Mutex::new(None))
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
    let lane = Arc::new(match selected {
        Some(instance) => connect_existing(endpoint, &program, &instance)?,
        None => start(endpoint, &store, &program)?,
    });
    *slot = Some(Connected {
        endpoint: endpoint.to_owned(),
        lane: lane.clone(),
    });
    Ok(lane)
}
fn hello(endpoint: &Path, challenge: &Challenge) -> aterm_json::Value {
    aterm_json::json!({"version":3,"role":"gui",
        "endpoint_hex":aterm_codec::hex::encode_lower(endpoint.as_os_str().as_bytes()),
        "nonce":challenge.nonce()})
}
fn finish(
    endpoint: &Path,
    program: &Program,
    instance: &Instance,
    stream: CtlStream,
    deadline: Instant,
) -> io::Result<Forwarder> {
    instance.validate()?;
    let challenge = Challenge::begin(instance, program, endpoint)?;
    runtime::write_json(&stream, &hello(endpoint, &challenge), deadline)?;
    if wire::read_frame(&stream, deadline)?.as_slice() != [1] {
        return Err(refused());
    }
    instance.validate()?;
    Forwarder::new(
        stream,
        Peer {
            pid: instance.pid(),
            birth: instance.birth().0,
        },
    )
}
fn connect_existing(
    endpoint: &Path,
    program: &Program,
    instance: &Instance,
) -> io::Result<Forwarder> {
    let deadline = Instant::now() + Duration::from_secs(8);
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
fn start(endpoint: &Path, store: &Store, program: &Program) -> io::Result<Forwarder> {
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
    let deadline = Instant::now() + Duration::from_secs(10);
    let born = runtime::read_json(&status, deadline)?;
    if born["version"].as_u64() != Some(3) || born["pid"].as_u64() != Some(u64::from(child.id())) {
        return Err(refused());
    }
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
    finish(endpoint, program, &instance, control, deadline)
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
fn existing() -> io::Result<Arc<Forwarder>> {
    connected()
        .try_lock()
        .map_err(|_| refused())?
        .as_ref()
        .filter(|current| !current.lane.is_broken())
        .map(|current| current.lane.clone())
        .ok_or_else(refused)
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
