// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent native custodian. GUI connections are replaceable; original
//! unreaped children, durable debt and recovery stay in this process.
use aterm_capacity::host_executable::{Program, Store};
use aterm_ctl::resource_admission_transport::{self as wire};
use aterm_uds::{CtlListener, CtlStream, process::Birth};
use std::{
    ffi::{OsStr, OsString},
    io,
    os::unix::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::{OpenOptionsExt as _, PermissionsExt as _},
    },
    path::PathBuf,
    process::ExitCode,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) const VERB: &str = "__resource-host-v3";
/// Local native control observations have a hang detector, independently of target execution.
pub(crate) const CONTROL_HANG: Duration = Duration::from_secs(60);

pub(crate) fn control_deadline(started: Instant) -> Instant {
    started + CONTROL_HANG
}
const MAX_CONNECTIONS: usize = 128;
struct Endpoint {
    program: PathBuf,
    native: Option<Arc<trust_resource_provenance::PinnedProgram>>,
    telemetry: Option<Arc<crate::resource_telemetry::Telemetry>>,
    store: Option<std::fs::File>,
}
static ENDPOINT: OnceLock<Endpoint> = OnceLock::new();
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn test_endpoint() -> io::Result<()> {
    let program = std::env::current_exe()?.canonicalize()?;
    let existing = ENDPOINT.get_or_init(|| Endpoint {
        program: program.clone(),
        native: None,
        telemetry: None,
        store: None,
    });
    if existing.program == program {
        Ok(())
    } else {
        Err(refused())
    }
}
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn test_endpoint_with_store(path: &std::path::Path) -> io::Result<()> {
    let store = open_store_description(path)?;
    let program = std::env::current_exe()?.canonicalize()?;
    ENDPOINT
        .set(Endpoint {
            program,
            native: None,
            telemetry: None,
            store: Some(store),
        })
        .map_err(|_| refused())
}
fn open_store_description(path: &std::path::Path) -> io::Result<std::fs::File> {
    let store = trust_resource_provenance::Store::open_at(path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(store.path())?;
    use std::os::fd::AsFd as _;
    if !store.same_original_directory(file.as_fd())? {
        return Err(refused());
    }
    store.validate()?;
    Ok(file)
}
pub(crate) fn workload_store_description() -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsFd as _;
    aterm_uds::kept_out(|| {
        ENDPOINT
            .get()
            .and_then(|endpoint| endpoint.store.as_ref())
            .ok_or_else(refused)?
            .as_fd()
            .try_clone_to_owned()
    })
}
pub(crate) fn store_expectation_supported() -> bool {
    ENDPOINT
        .get()
        .is_some_and(|endpoint| endpoint.store.is_some())
}
/// Compare only the actual backend's retained original directory. No pathname,
/// record, caller label or reopened directory can select or establish it.
pub(crate) fn matches_original_store(expected: std::os::fd::BorrowedFd<'_>) -> io::Result<bool> {
    let original = ENDPOINT
        .get()
        .and_then(|endpoint| endpoint.store.as_ref())
        .ok_or_else(refused)?;
    same_store_description(original, expected)
}
fn same_store_description(
    original: &std::fs::File,
    expected: std::os::fd::BorrowedFd<'_>,
) -> io::Result<bool> {
    use std::os::{fd::AsRawFd as _, unix::fs::MetadataExt as _};
    let flags = unsafe { libc::fcntl(expected.as_raw_fd(), libc::F_GETFL) };
    let fdflags = unsafe { libc::fcntl(expected.as_raw_fd(), libc::F_GETFD) };
    if flags < 0
        || fdflags < 0
        || flags & libc::O_ACCMODE != libc::O_RDONLY
        || fdflags & libc::FD_CLOEXEC == 0
    {
        return Err(refused());
    }
    let expected = aterm_uds::spawnfd::descriptor_metadata(expected.as_raw_fd())?;
    let actual = original.metadata()?;
    if !expected.is_dir() || expected.uid() != unsafe { libc::geteuid() } {
        return Err(refused());
    }
    trust_resource_provenance::require_no_acl_grants(original)?;
    Ok(actual.is_dir()
        && actual.uid() == expected.uid()
        && (actual.dev(), actual.ino()) == (expected.dev(), expected.ino()))
}

pub(super) fn inspector_program() -> io::Result<&'static std::path::Path> {
    ENDPOINT
        .get()
        .map(|endpoint| endpoint.program.as_path())
        .ok_or_else(refused)
}
fn native_program() -> io::Result<Arc<trust_resource_provenance::PinnedProgram>> {
    ENDPOINT
        .get()
        .and_then(|endpoint| endpoint.native.clone())
        .ok_or_else(refused)
}
fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "native resource custodian unavailable",
    )
}
pub(crate) fn path_field(value: &aterm_json::Value, field: &str) -> io::Result<PathBuf> {
    let encoded = value[field].as_str().ok_or_else(refused)?;
    if encoded.len() > 8192 {
        return Err(refused());
    }
    let path = PathBuf::from(OsString::from_vec(
        aterm_codec::hex::decode_vec(encoded.as_bytes()).ok_or_else(refused)?,
    ));
    if !path.is_absolute() {
        return Err(refused());
    }
    Ok(path)
}
pub(crate) fn write_json(
    stream: &CtlStream,
    value: &aterm_json::Value,
    deadline: Instant,
) -> io::Result<()> {
    let bytes = aterm_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > 16384 {
        return Err(refused());
    }
    wire::write_frame(stream, &bytes, deadline)
}
pub(crate) fn read_json(stream: &CtlStream, deadline: Instant) -> io::Result<aterm_json::Value> {
    let bytes = wire::read_frame(stream, deadline)?;
    if bytes.len() > 16384 {
        return Err(refused());
    }
    aterm_json::from_slice(&bytes).map_err(io::Error::other)
}

/// The one binary calls this before GUI initialization or bridge adoption.
/// The exact private role owns its two inherited socket slots once; no path,
/// budget or process identity is accepted as a command-line authority.
pub fn entry(args: &[OsString]) -> Option<ExitCode> {
    if let Some(result) = crate::metadata_prelude::entry(args) {
        return Some(result);
    }
    if args.first().is_none_or(|arg| arg != OsStr::new(VERB)) {
        return None;
    }
    if args.len() != 1 {
        return Some(ExitCode::from(75));
    }
    // The custodian retains status pipes, CPU/fence rights and original child
    // controls across all admitted workers. Preserve the OS hard ceiling while
    // raising this helper's usual256 soft default, as native verify already does.
    aterm_uds::spawnfd::raise_soft_nofile();
    // SAFETY: this exact early startup role receives slots3/4 exclusively from
    // detached_with_two_fds before any bridge/socket threads exist. Adoption
    // validates socket eligibility, consumes only those once-reserved lanes,
    // and refuses a hand invocation without an eligible inherited pair.
    let lanes = unsafe { aterm_uds::spawnfd::adopt_startup_bridge_lanes() };
    Some(
        match lanes
            .and_then(|(control, status)| run(CtlStream::from(control), CtlStream::from(status)))
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(75),
        },
    )
}

fn run(control: CtlStream, status: CtlStream) -> io::Result<()> {
    let store_path = Store::account_path()?;
    let store = Store::open_at(&store_path)?;
    let program = store.cache_current()?;
    let pid = std::process::id();
    let birth = aterm_uds::process::read_birth(i32::try_from(pid).map_err(|_| refused())?)
        .ok_or_else(refused)?;
    if !aterm_uds::process::LoadedImage::read(i32::try_from(pid).map_err(|_| refused())?)
        .is_some_and(|image| image.matches_file(program.path()) == Some(true))
    {
        return Err(refused());
    }
    // Opening resources reads readiness only; it never retires legacy work,
    // changes account permissions, creates debt or repairs missing journals.
    crate::actor::prerequisites()?;
    let parent = wire::local_peer(&control)?;
    if wire::local_peer(&status)? != parent {
        return Err(refused());
    }
    program.validate_process(parent.pid, Birth(parent.birth))?;
    let path =
        Store::account_path()?.join(format!("h-{}.sock", aterm_uds::rand::hex_token::<8>()?));
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&path))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let instance = store.publish(&program, &listener)?;
    let deadline = control_deadline(Instant::now());
    let native = trust_resource_provenance::Store::open_at(&Store::account_path()?)?
        .pin_program(program.path(), deadline)?;
    native.validate_cached_process(
        trust_resource_provenance::process::read_identity(pid)?,
        deadline,
    )?;
    ENDPOINT
        .set(Endpoint {
            program: program.path().to_owned(),
            native: Some(Arc::new(native)),
            telemetry: Some(Arc::new(crate::resource_telemetry::Telemetry::new()?)),
            store: Some(open_store_description(&store_path)?),
        })
        .map_err(|_| refused())?;
    let deadline = control_deadline(Instant::now());
    write_json(
        &status,
        &aterm_json::json!({"version":3,"pid":pid,"birth":birth.0,
        "socket_hex":aterm_codec::hex::encode_lower(path.as_os_str().as_bytes())}),
        deadline,
    )?;
    let hello = read_json(&control, deadline)?;
    let authority = authenticate_gui(&control, &program, &hello)?;
    crate::actor::activate_process()?;
    let capabilities = crate::actor::capabilities();
    if capabilities["ready"].as_bool() != Some(true) {
        return Err(refused());
    }
    instance.validate()?;
    wire::write_frame(&control, &[1], deadline)?;
    drop(status);
    let active = Arc::new(AtomicUsize::new(1));
    let guis = Arc::new(AtomicUsize::new(1));
    let connection = Connection(active.clone());
    let gui = Connection(guis.clone());
    // The GUI lane owns no jobs. Its EOF/exec/death merely retires that lane;
    // the listener/main thread and actor keep actual custody and recovery live.
    std::thread::Builder::new()
        .name("resource-gui-owner".into())
        .spawn(move || {
            let (_connection, _gui) = (connection, gui);
            let _ = crate::forward::serve(&control, |peer| authority.check(peer));
        })?;
    serve(listener, program, active, guis)
}

fn authenticate_gui(
    stream: &CtlStream,
    program: &Program,
    hello: &aterm_json::Value,
) -> io::Result<crate::enrollment::GuiAuthority> {
    if hello["version"].as_u64() != Some(3) || hello["role"].as_str() != Some("gui") {
        return Err(refused());
    }
    let endpoint = path_field(hello, "endpoint_hex")?;
    let nonce = hello["nonce"].as_str().ok_or_else(refused)?;
    crate::enrollment::verify_gui(wire::local_peer(stream)?, program, &endpoint, nonce)
}

struct Connection(Arc<AtomicUsize>);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn serve(
    listener: CtlListener,
    program: Program,
    active: Arc<AtomicUsize>,
    guis: Arc<AtomicUsize>,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    loop {
        match aterm_uds::ready::until(
            std::os::fd::AsFd::as_fd(&listener),
            aterm_uds::ready::Interest::Read,
            Instant::now() + Duration::from_secs(1),
        ) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                if active.load(Ordering::Acquire) == 0
                    && guis.load(Ordering::Acquire) == 0
                    && crate::actor::idle()
                {
                    // No controls or original actual children remain. A later
                    // GUI version may now create the next account custodian.
                    return Ok(());
                }
                continue;
            }
            Err(_) => {
                std::thread::park_timeout(Duration::from_secs(1));
                continue;
            }
        }
        let (stream, _) = match aterm_uds::accept(&listener) {
            Ok(connection) => connection,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(_) => {
                // Listener failure does not retire actual child control. Keep
                // the owner/recovery actor alive and its existing private
                // lifecycle channels usable; never drop custody on an I/O error.
                std::thread::park_timeout(Duration::from_secs(1));
                continue;
            }
        };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let guard = Connection(active.clone());
        let program = program.clone();
        let guis = guis.clone();
        // A failed thread spawn drops the guard and this connection only. It
        // must never unwind/drop the account's original owned child controls.
        let _ = std::thread::Builder::new()
            .name("resource-host-control".into())
            .spawn(move || {
                let _guard = guard;
                let _ = serve_one(&stream, &program, guis);
            });
    }
}
fn serve_one(stream: &CtlStream, program: &Program, guis: Arc<AtomicUsize>) -> io::Result<()> {
    let deadline = control_deadline(Instant::now());
    let peer = wire::local_peer(stream)?;
    let hello = read_json(stream, deadline)?;
    match hello["role"].as_str() {
        Some("gui") => {
            let authority = authenticate_gui(stream, program, &hello)?;
            guis.fetch_add(1, Ordering::AcqRel);
            let _gui = Connection(guis);
            wire::write_frame(stream, &[1], deadline)?;
            crate::forward::serve(stream, |peer| authority.check(peer))
        }
        Some("native-telemetry") if hello["version"].as_u64() == Some(3) => {
            let telemetry = ENDPOINT
                .get()
                .and_then(|endpoint| endpoint.telemetry.as_ref())
                .ok_or_else(refused)?;
            crate::resource_telemetry::serve(
                stream,
                peer,
                &hello,
                deadline,
                telemetry,
                crate::actor::context,
            )
        }
        Some("native-context") if hello["version"].as_u64() == Some(3) => {
            crate::resource_context::serve(stream, peer, &hello, deadline, crate::actor::context)
        }
        Some("native-context-sdk-v1") if hello["version"].as_u64() == Some(3) => {
            crate::resource_context::serve_sdk(
                stream,
                peer,
                &hello,
                deadline,
                crate::actor::context,
            )
        }
        Some("native-metadata-ready-v1") if hello["version"].as_u64() == Some(3) => {
            crate::resource_context::serve_metadata_ready(
                stream,
                peer,
                &hello,
                deadline,
                crate::actor::metadata_ready,
            )
        }
        Some("native-resolver-ready-v1") if hello["version"].as_u64() == Some(3) => {
            crate::resource_context::serve_resolver_ready(
                stream,
                peer,
                &hello,
                deadline,
                crate::actor::resolver_ready,
            )
        }
        Some("native-workload-ready-v1") if hello["version"].as_u64() == Some(3) => {
            crate::workload_transport::ready(
                stream,
                peer,
                &hello,
                deadline,
                crate::actor::workload_ready,
            )
        }
        Some("native-workload-member-original-v1") if hello["version"].as_u64() == Some(3) => {
            crate::workload_transport::member_original(
                stream,
                peer,
                &hello,
                deadline,
                crate::actor::workload_member_original,
            )
        }
        Some("native-workload-worker-v1") if hello["version"].as_u64() == Some(3) => {
            crate::workload_transport::worker(stream, peer, &hello, deadline)
        }
        Some("native-workload-worker-return-v1") if hello["version"].as_u64() == Some(3) => {
            crate::workload_transport::worker_return(stream, peer, &hello, deadline)
        }
        Some("inspect") if hello["version"].as_u64() == Some(3) => {
            let session = hello["session"].as_str().ok_or_else(refused)?;
            let observed = crate::actor::inspect(peer, session)?.json()?;
            if wire::local_peer(stream)? != peer {
                return Err(refused());
            }
            write_json(stream, &observed, deadline)?;
            // Keep the authenticated server alive until the native caller has
            // validated its post-response peer and immutable endpoint. Darwin
            // may lose LOCAL_PEERPID immediately after the server closes.
            if wire::read_frame(stream, deadline)?.as_slice() != [1] {
                return Err(refused());
            }
            Ok(())
        }
        Some("sealed-metadata-v1") => {
            let job = crate::actor::metadata_binding(&hello)?;
            if wire::local_peer(stream)? != peer {
                return Err(refused());
            }
            wire::write_frame(stream, aterm_types::resource_subgroup::PREPARE, deadline)?;
            let member = crate::metadata_transport::receive(stream, &job, deadline)?;
            crate::actor::submit_metadata(member)?;
            aterm_ctl::resource_subgroup_client::acknowledge(stream, deadline)
        }
        Some("sealed-resolver-query-v1") => {
            let job = crate::actor::resolver_binding(&hello)?;
            if wire::local_peer(stream)? != peer {
                return Err(refused());
            }
            wire::write_frame(stream, aterm_types::resource_subgroup::PREPARE, deadline)?;
            let member = crate::resolver_transport::receive(stream, &job, deadline)?;
            crate::actor::submit_resolver_query(member)?;
            aterm_ctl::resource_subgroup_client::acknowledge(stream, deadline)
        }
        Some("sealed-workload-query-v1") => {
            let job = crate::actor::workload_binding(&hello)?;
            if wire::local_peer(stream)? != peer {
                return Err(refused());
            }
            wire::write_frame(stream, &[0x50], deadline)?;
            let incoming = crate::workload_transport::receive(stream, &job, deadline)?;
            crate::actor::submit_workload_query(incoming)?;
            aterm_ctl::resource_subgroup_client::acknowledge(stream, deadline)
        }
        Some("native-linker-loan-v1") => {
            let fields = ["version", "role", "session"];
            if hello.as_object().is_none_or(|map| {
                map.len() != fields.len() || fields.iter().any(|name| !map.contains_key(*name))
            }) || hello["version"].as_u64() != Some(3)
                || wire::local_peer(stream)? != peer
            {
                return Err(refused());
            }
            let session = hello["session"].as_str().ok_or_else(refused)?;
            wire::write_frame(stream, &[0x50], deadline)?;
            let incoming = crate::linker_transport::receive(stream, session, deadline)?;
            crate::actor::submit_linker(incoming)?;
            aterm_ctl::resource_subgroup_client::acknowledge(stream, deadline)
        }
        Some("sealed-workload-member-v1") => {
            let job = crate::actor::member_binding_role(
                &hello,
                trust_resource_controller::workload::host::produced::PURPOSE,
            )?;
            if wire::local_peer(stream)? != peer {
                return Err(refused());
            }
            wire::write_frame(stream, &[0x50], deadline)?;
            let incoming = crate::workload_transport::receive_produced(stream, &job, deadline)?;
            crate::actor::submit_workload_produced(incoming)?;
            aterm_ctl::resource_subgroup_client::acknowledge(stream, deadline)
        }
        Some("sealed-workload-contained-produced-v1") => {
            let job = crate::actor::member_binding_role(
                &hello,
                trust_resource_controller::workload::host::contained::PURPOSE,
            )?;
            if wire::local_peer(stream)? != peer {
                return Err(refused());
            }
            wire::write_frame(stream, &[0x50], deadline)?;
            let incoming =
                crate::workload_transport::receive_contained_produced(stream, &job, deadline)?;
            crate::actor::submit_workload_produced(incoming)?;
            aterm_ctl::resource_subgroup_client::acknowledge(stream, deadline)?;
            stream.shutdown(std::net::Shutdown::Write)
        }
        Some(role)
            if hello["version"].as_u64() == Some(3)
                && matches!(
                    role,
                    "proxy-member" | trust_resource_controller::accounting::PROXY_ROLE
                ) =>
        {
            let accounted = role == trust_resource_controller::accounting::PROXY_ROLE;
            let job = if accounted {
                crate::actor::proxy_binding_accounted(&hello)?
            } else {
                crate::actor::proxy_binding(&hello)?
            };
            let native = native_program()?;
            native.validate_cached_process(
                trust_resource_provenance::process::read_identity(peer.pid)?,
                deadline,
            )?;
            wire::write_frame(stream, aterm_types::resource_subgroup::PREPARE, deadline)?;
            let incoming = if accounted {
                crate::resource_proxy_intake::receive_accounted(
                    stream,
                    &job,
                    &native,
                    deadline,
                    crate::actor::provisional_proxy,
                )?
            } else {
                crate::resource_proxy_intake::receive(
                    stream,
                    &job,
                    &native,
                    deadline,
                    crate::actor::provisional_proxy,
                )?
            };
            crate::actor::submit_proxy(incoming)?;
            aterm_ctl::resource_subgroup_client::acknowledge(stream, deadline)
        }
        Some("member") => {
            let job = crate::actor::member_binding(&hello)?;
            if wire::local_peer(stream)? != peer {
                return Err(refused());
            }
            wire::write_frame(stream, aterm_types::resource_subgroup::PREPARE, deadline)?;
            let member = aterm_ctl::resource_subgroup_client::receive(
                stream,
                &job,
                deadline,
                crate::actor::authorize_member,
            )?;
            crate::actor::submit_member(member)?;
            // Attachment only: actual Started/Finished remain on the original
            // private lifecycle channel, including when this ACK is lost.
            aterm_ctl::resource_subgroup_client::acknowledge(stream, deadline)
        }
        _ => Err(refused()),
    }
}
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn serve_test(stream: &CtlStream, program: &Program) -> io::Result<()> {
    serve_one(stream, program, Arc::new(AtomicUsize::new(0)))
}
#[cfg(all(test, unix))]
#[path = "runtime_store_tests.rs"]
mod original_store_tests;
