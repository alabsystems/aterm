// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-session native backend startup. The actor, not a frontend SDK waiter,
//! owns birth and settlement. Startup never waits for SDK readiness in a source
//! dispatch worker; endpoint delivery must still use OwnedService's real checks.
use aterm_agent::harness::{
    account_work, codex_source_registry as registry, native_act::Process, worker,
};
use aterm_uds::spawnfd::{BirthAttempt, SpawnFailure, Spawned};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

#[path = "codex_retained_settings.rs"]
mod settings;
pub(crate) use settings::Settings;

#[path = "codex_retained_delivery.rs"]
mod frontend_delivery;

/// An original start intent on an actual native-issued controller. The opaque
/// identity holds no request scope and grants no constructor or attachment.
pub(crate) struct StartOwner {
    controller: registry::ServiceIdentity,
    operation: String,
}
impl StartOwner {
    pub(crate) fn new(
        frontend: &registry::ServiceFrontend,
        operation: &str,
    ) -> Result<Self, registry::Error> {
        frontend.current()?;
        if operation.len() != 32
            || !operation
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(registry::Error::Denied);
        }
        Ok(Self {
            controller: frontend.identity(),
            operation: operation.into(),
        })
    }
    fn same(&self, other: &Self) -> bool {
        self.controller.same(&other.controller) && self.operation == other.operation
    }
}

pub(crate) struct Controller {
    namespace: Weak<registry::Registry>,
    id: String,
    owner: StartOwner,
    image: aterm_agent::harness::native_act::Image,
    arguments: Vec<OsString>,
    cwd: PathBuf,
    environment: registry::OwnedLaunchEnvironment,
    state: Arc<Mutex<&'static str>>,
    delivery: Delivery,
    _diagnostics: Arc<Mutex<VecDeque<u8>>>,
}

struct Attachment {
    frontend: registry::ServiceFrontend,
    operation: String,
    callback: PathBuf,
    owner: Process,
}
struct Delivered {
    frontend: registry::ServiceFrontend,
    operation: String,
    receipt: registry::PreparedServiceAttachment,
    transport: Option<frontend_delivery::FrontendDelivery>,
    result: Result<(), registry::Error>,
}
struct Running {
    service: Option<registry::OwnedService>,
    delivered: Vec<Delivered>,
}
#[derive(Clone)]
pub(crate) struct Delivery {
    inner: Arc<Mutex<Running>>,
    ctx: Weak<crate::SessionCtx>,
    cancelled: account_work::Cancellation,
}
impl Delivery {
    fn live(&self) -> bool {
        !self.cancelled.cancelled()
            && self
                .ctx
                .upgrade()
                .is_some_and(|ctx| !ctx.fabric.endpoint_closed())
    }
    pub(crate) fn attach(
        &self,
        frontend: registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        operation: &str,
        callback: &std::path::Path,
    ) -> Result<(), registry::Error> {
        if !self.live() {
            return Err(registry::Error::Stale);
        }
        foreground.check_current()?;
        let owner =
            Process::observe(foreground.caller().pid).ok_or(registry::Error::Unavailable)?;
        if owner.birth != foreground.caller().birth {
            return Err(registry::Error::Stale);
        }
        let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
        let Running { service, delivered } = &mut *running;
        let service = service.as_mut().ok_or(registry::Error::Busy)?;
        attach(
            service,
            delivered,
            Attachment {
                frontend,
                operation: operation.into(),
                callback: callback.into(),
                owner,
            },
            foreground,
            self,
        )
    }
}

/// A callback address is DATA. The admitted member and original connection are
/// checked independently by OwnedService; this kernel check only prevents sending
/// its private descriptor to another process at the caller's supplied address.
fn connect_callback(
    path: &std::path::Path,
    owner: &Process,
    deadline: Instant,
) -> Result<aterm_uds::CtlStream, registry::Error> {
    if Process::observe(owner.pid).as_ref() != Some(owner) {
        return Err(registry::Error::Stale);
    }
    let stream = aterm_uds::connect::until(path, deadline).map_err(|_error| {
        #[cfg(debug_assertions)]
        eprintln!("Retained native callback: connect-kind={:?}", _error.kind());
        registry::Error::Unavailable
    })?;
    if aterm_uds::fdpass::peer_pid(&stream).map_err(|_error| {
        #[cfg(debug_assertions)]
        eprintln!("Retained native callback: peer-kind={:?}", _error.kind());
        registry::Error::Unavailable
    })? != owner.pid
        || Process::observe(owner.pid).as_ref() != Some(owner)
    {
        return Err(registry::Error::Denied);
    }
    Ok(stream)
}

struct Start {
    reservation: registry::OwnedLaunchReservation,
    admitted: worker::Admitted,
    cwd: PathBuf,
}

pub(crate) struct Selection {
    process: Process,
    arguments: Vec<OsString>,
    cwd: PathBuf,
    environment: registry::OwnedLaunchEnvironment,
}
impl Selection {
    pub(crate) fn capture(
        peer: &registry::Peer,
        arguments: &[OsString],
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> Result<Self, registry::Error> {
        if !peer.current() || arguments.iter().any(|arg| arg.as_bytes().contains(&0)) {
            return Err(registry::Error::Stale);
        }
        let process = Process::observe(peer.caller().pid).ok_or(registry::Error::Unavailable)?;
        let cwd = aterm_sysprobe::process_cwd(process.pid).ok_or(registry::Error::Unavailable)?;
        if !cwd.is_absolute() || Process::observe(process.pid).as_ref() != Some(&process) {
            return Err(registry::Error::Stale);
        }
        if process.birth != peer.caller().birth || !peer.current() {
            return Err(registry::Error::Stale);
        }
        Ok(Self {
            environment: registry::OwnedLaunchEnvironment::from_snapshot(&process, environment)?,
            process,
            arguments: arguments.to_vec(),
            cwd,
        })
    }
}

impl Controller {
    #[cfg(debug_assertions)]
    pub(crate) fn is_resource_qualification(&self) -> bool {
        self.arguments == [OsString::from("--alab-app-resources")]
    }
    #[cfg(debug_assertions)]
    pub(crate) fn observe_owned_backend(&self) -> Result<Process, registry::Error> {
        let mut running = self
            .delivery
            .inner
            .try_lock()
            .map_err(|_| registry::Error::Busy)?;
        running
            .service
            .as_mut()
            .ok_or(registry::Error::Busy)?
            .observe_owned_backend()
    }
    /// Read fixed debug stage DATA only, never arbitrary backend output.
    #[cfg(debug_assertions)]
    pub(crate) fn qualification_stages(&self) -> Vec<&'static str> {
        const STAGES: &[&str] = &[
            "configured-start",
            "config-loaded",
            "cloud-policy-loaded",
            "login-policy-checked",
            "environment-ready",
            "embedded-client-start",
            "embedded-client-ready",
            "attachment-wait",
            "attachment-received",
            "service-error",
            "bridge-session-start",
            "bridge-session-ended",
            "bridge-read-eof",
            "bridge-greeting",
            "bridge-attached",
        ];
        let Ok(bytes) = self._diagnostics.lock() else {
            return Vec::new();
        };
        let bytes: Vec<u8> = bytes.iter().copied().collect();
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|line| {
                let stage = line.strip_prefix("Retained backend startup: stage=")?;
                STAGES.iter().copied().find(|known| *known == stage)
            })
            .collect()
    }

    pub(crate) fn in_namespace(&self, sources: &Arc<registry::Registry>) -> bool {
        Weak::ptr_eq(&self.namespace, &Arc::downgrade(sources))
    }
    pub(crate) fn delivery(&self) -> Delivery {
        self.delivery.clone()
    }
    pub(crate) fn matches(&self, selection: &Selection, sources: &Arc<registry::Registry>) -> bool {
        Weak::ptr_eq(&self.namespace, &Arc::downgrade(sources))
            && self.image == selection.process.image
            && self.arguments == selection.arguments
            && self.cwd == selection.cwd
            && self.environment.same_settings(&selection.environment)
    }
    pub(crate) fn same_start(&self, owner: &StartOwner, sources: &Arc<registry::Registry>) -> bool {
        self.in_namespace(sources) && self.owner.same(owner)
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(crate) fn state(&self) -> Result<&'static str, registry::Error> {
        self.state
            .lock()
            .map(|state| *state)
            .map_err(|_| registry::Error::Unavailable)
    }

    pub(crate) fn start(
        ctx: &Arc<crate::SessionCtx>,
        sources: &Arc<registry::Registry>,
        lifetime: &account_work::Lifetime,
        selection: Selection,
        owner: StartOwner,
    ) -> Result<Self, registry::Error> {
        if ctx.fabric.endpoint_closed() {
            return Err(registry::Error::Stale);
        }
        // This is deliberately a fresh operation, not the frontend request's
        // scope. Its cancellation/EOF cannot kill an accepted backend operation.
        let scope = lifetime.enter().ok_or(registry::Error::Unavailable)?;
        let start = prepare(ctx, sources, &scope, &selection).inspect_err(|_error| {
            #[cfg(debug_assertions)]
            eprintln!("Retained native startup: preparation={_error:?}");
        })?;
        let id = aterm_uds::rand::hex_token::<16>().map_err(|_| registry::Error::Unavailable)?;
        let state = Arc::new(Mutex::new("starting"));
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
        let delivery = Delivery {
            inner: Arc::new(Mutex::new(Running {
                service: None,
                delivered: Vec::new(),
            })),
            ctx: Arc::downgrade(ctx),
            cancelled: scope.cancellation(),
        };
        let result = Self {
            namespace: Arc::downgrade(sources),
            id,
            owner,
            image: selection.process.image.clone(),
            arguments: selection.arguments,
            cwd: selection.cwd,
            environment: selection.environment,
            state: state.clone(),
            delivery: delivery.clone(),
            _diagnostics: diagnostics.clone(),
        };
        let ctx = Arc::downgrade(ctx);
        let actor_scope = scope.clone();
        let sources = sources.clone();
        std::thread::Builder::new()
            .name("codex-retained-service".into())
            .spawn(scope.wrap(move || {
                run(ctx, actor_scope, start, state, diagnostics, delivery);
                drop(sources);
            }))
            .map_err(|_| registry::Error::Unavailable)?;
        Ok(result)
    }
}

fn prepare(
    ctx: &crate::SessionCtx,
    sources: &registry::Registry,
    scope: &account_work::Scope,
    selection: &Selection,
) -> Result<Start, registry::Error> {
    use std::os::unix::ffi::OsStringExt;
    let cli =
        worker::worker_cli(aterm_phase::Program::Codex).ok_or(registry::Error::Unavailable)?;
    let cwd = selection.cwd.clone();
    // This is the already observed caller image and explicit startup snapshot.
    // Shell worker posture/pool transformations must not select another argv.
    let starter = worker::Starter {
        exe: std::ffi::OsString::from_vec(selection.process.image.path.clone()).into(),
        env: Vec::new(),
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    let arguments = std::iter::once(OsString::from("--alab-retained-service"))
        .chain(selection.arguments.iter().cloned())
        .collect();
    let starter = selection.environment.prepare(starter)?;
    if !starter
        .env
        .iter()
        .any(|(key, value)| key == "HOME" && std::path::Path::new(value).is_absolute())
    {
        return Err(registry::Error::Unsupported);
    }
    let expected = selection.process.image.clone();
    let executable = starter.exe.clone();
    let admitted = worker::admit(
        cli,
        starter,
        |_| aterm_agent::harness::native_act::Image::at(&executable).as_ref() == Some(&expected),
        |_| None,
        |_| Err("retained startup never installs another program".into()),
    )
    .map_err(|_| registry::Error::Denied)?;
    let environment = selection.environment.clone();
    let env = crate::native_act::codex_account_env().map_err(|_| {
        #[cfg(debug_assertions)]
        eprintln!("Retained native startup: context=Unavailable");
        registry::Error::Unavailable
    })?;
    let reservation = sources.reserve_owned_worker_with_arguments(
        &env,
        ctx.self_id.as_str(),
        &admitted,
        scope,
        (environment, arguments),
    )?;
    let admitted = reservation.arm(admitted)?;
    Ok(Start {
        reservation,
        admitted,
        cwd,
    })
}

fn live(ctx: &Weak<crate::SessionCtx>, scope: &account_work::Scope) -> bool {
    !scope.cancelled()
        && ctx
            .upgrade()
            .is_some_and(|ctx| !ctx.fabric.endpoint_closed())
}
fn state(state: &Mutex<&'static str>, value: &'static str) {
    if let Ok(mut state) = state.lock() {
        *state = value;
    }
}

fn run(
    ctx: Weak<crate::SessionCtx>,
    scope: account_work::Scope,
    start: Start,
    status: Arc<Mutex<&'static str>>,
    diagnostics: Arc<Mutex<VecDeque<u8>>>,
    delivery: Delivery,
) {
    if !live(&ctx, &scope) {
        state(&status, "stopping");
        return;
    }
    let Ok(attempt) = BirthAttempt::new() else {
        state(&status, "failed");
        return;
    };
    let (mut worker, control) =
        match start
            .reservation
            .spawn_with_required_control(start.admitted, &start.cwd, attempt)
        {
            Ok(started) => started,
            Err(mut failure) => {
                state(&status, "failed");
                // A refused attempt is not an authoritative no-child result. Keep
                // its actual outcome and scope instead of inventing a cancellation.
                while settle_failure(&mut failure.outcome).is_err() {
                    std::thread::park_timeout(Duration::from_millis(100));
                }
                return;
            }
        };
    drop(control); // OwnedService obtains the original constructor's own endpoint.
    let mut drains_ok = true;
    if let Some(output) = worker.take_stdout() {
        drains_ok &= drain(output, scope.clone(), diagnostics.clone()).is_ok();
    }
    if let Some(output) = worker.take_stderr() {
        drains_ok &= drain(output, scope.clone(), diagnostics).is_ok();
    }
    if !drains_ok {
        state(&status, "failed");
        while worker.stop().is_err() {
            std::thread::park_timeout(Duration::from_millis(100));
        }
        return;
    }
    let service = match worker.into_service() {
        Ok(service) => service,
        Err(mut failure) => {
            state(&status, "failed");
            while failure.worker.stop().is_err() {
                std::thread::park_timeout(Duration::from_millis(100));
            }
            return;
        }
    };
    // This reports actual birth only. It is never SDK readiness or permission
    // to attach; delivery must independently call OwnedService's admission API.
    state(&status, "spawned");
    {
        let mut running = delivery
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        running.service = Some(service);
    }
    while live(&ctx, &scope) {
        if let Ok(mut running) = delivery.inner.try_lock()
            && let Some(service) = &mut running.service
        {
            let _ = service.reconcile_frontend();
        }
        std::thread::park_timeout(Duration::from_millis(100));
    }
    #[cfg(debug_assertions)]
    eprintln!(
        "Retained native owner: loop-ended cancelled={} session-open={}",
        scope.cancelled(),
        ctx.upgrade()
            .is_some_and(|ctx| !ctx.fabric.endpoint_closed())
    );
    state(&status, "stopping");
    delivery.cancelled.cancel();
    let mut service = {
        let mut running = delivery
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        running.delivered.clear();
        running.service.take()
    };
    if let Some(service) = &mut service {
        while service.stop().is_err() {
            std::thread::park_timeout(Duration::from_millis(100));
        }
    }
}

fn attach(
    service: &mut registry::OwnedService,
    delivered: &mut Vec<Delivered>,
    request: Attachment,
    foreground: &registry::FrontendMember<'_>,
    delivery: &Delivery,
) -> Result<(), registry::Error> {
    request.frontend.current()?;
    delivered.retain(|entry| !entry.frontend.retired());
    if let Some(entry) = delivered
        .iter()
        .find(|entry| entry.frontend.same_requester(&request.frontend))
    {
        // One delivery per actual frontend incarnation. Lost response waiters
        // cannot replace its callback and fence a successfully attached stream.
        return if entry.operation == request.operation {
            entry.result
        } else {
            Err(registry::Error::Denied)
        };
    }
    if delivered.len() >= 256 {
        return Err(registry::Error::Busy);
    }
    let receipt = service.prepare_attachment(request.frontend.clone(), foreground)?;
    delivered.push(Delivered {
        frontend: request.frontend,
        operation: request.operation,
        receipt,
        transport: None,
        result: Err(registry::Error::Stale),
    });
    let entry = delivered.last_mut().ok_or(registry::Error::Unavailable)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    entry.result = (|| {
        // Do not connect until actual backend construction passed preparation.
        // Busy SDK readiness must not produce an empty callback connection.
        let callback = connect_callback(&request.callback, &request.owner, deadline).inspect_err(
            |_error| {
                #[cfg(debug_assertions)]
                eprintln!("Retained native delivery: callback={_error:?}");
            },
        )?;
        retry_delivery_metadata(deadline, delivery, || {
            entry.frontend.current().inspect_err(|_error| {
                #[cfg(debug_assertions)]
                eprintln!("Retained native delivery: frontend={_error:?}");
            })?;
            foreground.check_current().inspect_err(|_error| {
                #[cfg(debug_assertions)]
                eprintln!("Retained native delivery: foreground={_error:?}");
            })
        })?;
        deliver(
            service,
            &mut entry.receipt,
            &mut entry.transport,
            callback,
            foreground,
            delivery,
            deadline,
        )
    })();
    entry.result
}

fn deliver(
    service: &mut registry::OwnedService,
    attachment: &mut registry::PreparedServiceAttachment,
    transport: &mut Option<frontend_delivery::FrontendDelivery>,
    callback: aterm_uds::CtlStream,
    foreground: &registry::FrontendMember<'_>,
    delivery: &Delivery,
    deadline: Instant,
) -> Result<(), registry::Error> {
    // The accepted native actor owns this exact receipt throughout synchronous
    // commit and callback IO. No await or response cancellation can replay it.
    let result = (|| {
        retry_delivery_metadata(deadline, delivery, || {
            service.commit_attachment(attachment, foreground)
        })
        .inspect_err(|_error| {
            #[cfg(debug_assertions)]
            eprintln!("Retained native delivery: commit={_error:?}");
        })?;
        let endpoint = retry_delivery_metadata(deadline, delivery, || {
            service.take_frontend(attachment, foreground)
        })
        .inspect_err(|_error| {
            #[cfg(debug_assertions)]
            eprintln!("Retained native delivery: endpoint={_error:?}");
        })?;
        *transport = Some(frontend_delivery::FrontendDelivery::new(
            callback, endpoint,
        )?);
        transport
            .as_mut()
            .ok_or(registry::Error::Unavailable)?
            .exchange(deadline, || {
                retry_delivery_metadata(deadline, delivery, || {
                    service.commit_attachment(attachment, foreground)
                })
            })
    })();
    if result.is_err() {
        service.detach_frontend()?;
    }
    result
}

fn retry_delivery_metadata<T>(
    deadline: Instant,
    delivery: &Delivery,
    mut attempt: impl FnMut() -> Result<T, registry::Error>,
) -> Result<T, registry::Error> {
    loop {
        if !delivery.live() {
            return Err(registry::Error::Stale);
        }
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or(registry::Error::Unavailable)?;
        match attempt() {
            Err(registry::Error::Busy) => {
                // Registry try-locks have no notification. Preserve the same
                // delivery, callback and deadline while waiting for metadata.
                std::thread::sleep(left.min(Duration::from_millis(1)));
            }
            result => return result,
        }
    }
}

fn drain(
    mut input: impl Read + Send + 'static,
    scope: account_work::Scope,
    diagnostics: Arc<Mutex<VecDeque<u8>>>,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("codex-service-output".into())
        .spawn(scope.wrap(move || {
            let mut bytes = [0; 4096];
            loop {
                let count = match input.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => count,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                if let Ok(mut held) = diagnostics.lock() {
                    for byte in &bytes[..count] {
                        if held.len() == 32 * 1024 {
                            held.pop_front();
                        }
                        held.push_back(*byte);
                    }
                }
            }
        }))
        .map(|_| ())
}

fn settle_failure(failure: &mut SpawnFailure) -> io::Result<()> {
    match failure {
        SpawnFailure::NotSpawned(_) => Ok(()),
        SpawnFailure::Refused(_) => Err(io::ErrorKind::WouldBlock.into()),
        SpawnFailure::Unbound { spawned, .. } => settle_cohort(spawned),
    }
}
fn settle_cohort(spawned: &mut Spawned) -> io::Result<()> {
    if !spawned.key().is_current() || !spawned.is_original_child() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let group = spawned.child_mut().id();
    if !spawned.child_mut().is_reaped() {
        if !spawned.child_mut().owns_unreaped() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        aterm_uds::process::kill_group_until_empty(
            i32::try_from(group).map_err(io::Error::other)?,
        )?;
        if !spawned.group_members_ended()? {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        spawned.child_mut().wait()?;
    }
    if aterm_uds::process::group_presence(group) != aterm_uds::process::ProcessPresence::Absent {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "codex_retained_service_tests.rs"]
mod tests;

pub(crate) fn receive_settings(
    foreground: &registry::FrontendMember<'_>,
    callback: &std::path::Path,
) -> Result<Settings, registry::Error> {
    foreground.check_current()?;
    let owner = Process::observe(foreground.caller().pid).ok_or(registry::Error::Unavailable)?;
    if owner.birth != foreground.caller().birth {
        return Err(registry::Error::Stale);
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let stream = connect_callback(callback, &owner, deadline)?;
    let settings =
        Settings::read_socket(&stream, deadline).map_err(|error| match error.kind() {
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => registry::Error::Denied,
            _ => registry::Error::Unavailable,
        })?;
    foreground.check_current()?;
    if Process::observe(owner.pid).as_ref() != Some(&owner) {
        return Err(registry::Error::Stale);
    }
    Ok(settings)
}
