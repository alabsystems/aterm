// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lifecycle of the optional aterm-owned Ollama daemon: reserving an ephemeral
//! loopback endpoint and a private managed home, launching and attesting the
//! child, and terminating the process aterm itself spawned.

use super::model_store::{AttestedManagedModel, attest_managed_model};
use super::transport::{RequestWriteAuthority, build_agent, loopback_socket};
use super::{
    EndpointOrigin, Job, MAX_RESPONSE_BYTES, TitleSummaryLocality, cancelled_error,
    job_is_authorized,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MANAGED_ENDPOINT_LAUNCH_ATTEMPTS: usize = 3;

#[derive(Clone)]
pub(super) struct ManagedOllamaController {
    state: Arc<Mutex<ManagedOllamaState>>,
}

struct ManagedOllamaState {
    /// Only this exact configuration epoch may install or reuse a daemon. `None`
    /// is a terminal revocation used by worker/application shutdown.
    allowed_authority_epoch: Option<u64>,
    owned: Option<OwnedOllamaChild>,
}

struct OwnedOllamaChild {
    child: std::process::Child,
    process: ManagedProcessIdentity,
    endpoint: String,
    authority_epoch: u64,
    private_home: Option<std::path::PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ManagedRuntimeExit {
    pub(super) endpoint: String,
    pub(super) authority_epoch: u64,
}

/// Stable identity of the process we actually spawned. A bare PID is not an
/// identity: it can be reused after exit. On macOS the birth time, parent, and
/// effective user are captured from libproc and rechecked at every connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ManagedProcessIdentity {
    pub(super) pid: u32,
    #[cfg(unix)]
    process_group: u32,
    #[cfg(unix)]
    session_id: u32,
    #[cfg(target_os = "macos")]
    parent_pid: u32,
    #[cfg(target_os = "macos")]
    uid: u32,
    #[cfg(target_os = "macos")]
    start_seconds: u64,
    #[cfg(target_os = "macos")]
    start_microseconds: u64,
}

impl ManagedOllamaController {
    pub(super) fn new(allowed_authority_epoch: Option<u64>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ManagedOllamaState {
                allowed_authority_epoch,
                owned: None,
            })),
        }
    }

    /// Atomically change launch authority and detach the old daemon. An install
    /// racing after this point either observes the new epoch or is killed/rejected.
    pub(super) fn transition_to(&self, allowed_authority_epoch: Option<u64>) {
        let child = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.allowed_authority_epoch = allowed_authority_epoch;
            state.owned.take()
        };
        if let Some(owned) = child {
            terminate_owned_ollama_nonblocking(owned);
        }
    }

    pub(super) fn stop(&self) {
        self.transition_to(None);
    }

    pub(super) fn reap(&self) -> Option<ManagedRuntimeExit> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let exited = state
            .owned
            .as_mut()
            .is_some_and(|owned| owned.child.try_wait().ok().flatten().is_some());
        let owned = exited.then(|| state.owned.take()).flatten();
        drop(state);
        owned.map(|owned| {
            let event = ManagedRuntimeExit {
                endpoint: owned.endpoint.clone(),
                authority_epoch: owned.authority_epoch,
            };
            // The direct child exited, but an accepted server may be its descendant.
            // Signal the whole dedicated group now, but emit lifecycle health
            // without waiting for descendant reaping/private-HOME cleanup.
            terminate_owned_ollama_nonblocking(owned);
            event
        })
    }

    #[cfg(test)]
    pub(super) fn owns_endpoint(&self, endpoint: &str, authority_epoch: u64) -> bool {
        self.endpoint_process(endpoint, authority_epoch).is_some()
    }

    /// Return the endpoint selected by this authority. Automatic managed mode
    /// intentionally cannot recompute it from configuration: the controller is
    /// the single owner of the per-process ephemeral selection.
    pub(super) fn owned_for_authority(
        &self,
        authority_epoch: u64,
    ) -> Option<(ManagedProcessIdentity, String)> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .owned
            .as_mut()
            .is_some_and(|owned| owned.child.try_wait().ok().flatten().is_some())
            && let Some(owned) = state.owned.take()
        {
            drop(state);
            terminate_owned_ollama(owned);
            return None;
        }
        (state.allowed_authority_epoch == Some(authority_epoch))
            .then(|| {
                state.owned.as_ref().and_then(|owned| {
                    (owned.authority_epoch == authority_epoch)
                        .then(|| (owned.process, owned.endpoint.clone()))
                })
            })
            .flatten()
    }

    pub(super) fn endpoint_process(
        &self,
        endpoint: &str,
        authority_epoch: u64,
    ) -> Option<ManagedProcessIdentity> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .owned
            .as_mut()
            .is_some_and(|owned| owned.child.try_wait().ok().flatten().is_some())
            && let Some(owned) = state.owned.take()
        {
            drop(state);
            terminate_owned_ollama(owned);
            return None;
        }
        (state.allowed_authority_epoch == Some(authority_epoch))
            .then(|| {
                state.owned.as_ref().and_then(|owned| {
                    (owned.endpoint == endpoint && owned.authority_epoch == authority_epoch)
                        .then_some(owned.process)
                })
            })
            .flatten()
    }

    pub(super) fn install(
        &self,
        child: std::process::Child,
        endpoint: String,
        authority_epoch: u64,
        private_home: Option<std::path::PathBuf>,
    ) -> Result<(), String> {
        let process = match managed_process_identity(child.id()) {
            Ok(process) => process,
            Err(error) => {
                terminate_unadmitted_managed_child(child, private_home);
                return Err(error);
            }
        };
        #[cfg(unix)]
        if process.process_group != process.pid
            || process.session_id != process.pid
            // SAFETY: getpgrp only reads caller process metadata.
            || process.process_group == u32::try_from(unsafe { libc::getpgrp() }).unwrap_or(0)
        {
            terminate_unadmitted_managed_child(child, private_home);
            return Err("managed Ollama did not enter its dedicated process session".to_string());
        }
        let displaced = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.allowed_authority_epoch != Some(authority_epoch) {
                drop(state);
                let owned = OwnedOllamaChild {
                    child,
                    process,
                    endpoint,
                    authority_epoch,
                    private_home,
                };
                terminate_owned_ollama(owned);
                return Err(cancelled_error());
            }
            state.owned.replace(OwnedOllamaChild {
                child,
                process,
                endpoint,
                authority_epoch,
                private_home,
            })
        };
        if let Some(owned) = displaced {
            terminate_owned_ollama(owned);
        }
        Ok(())
    }

    fn stop_if_owned(&self, endpoint: &str, authority_epoch: u64) {
        let child = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let matches = state.owned.as_ref().is_some_and(|owned| {
                owned.endpoint == endpoint && owned.authority_epoch == authority_epoch
            });
            matches.then(|| state.owned.take()).flatten()
        };
        if let Some(owned) = child {
            terminate_owned_ollama(owned);
        }
    }
}

impl Default for ManagedOllamaController {
    fn default() -> Self {
        Self::new(None)
    }
}

#[cfg(target_os = "macos")]
pub(super) fn managed_process_identity(pid: u32) -> Result<ManagedProcessIdentity, String> {
    let pid_i32 = i32::try_from(pid).map_err(|_| "managed Ollama PID is invalid".to_string())?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .map_err(|_| "managed Ollama process record is too large".to_string())?;
    // SAFETY: `info` points to `size` writable bytes of the exact structure
    // requested by PROC_PIDTBSDINFO. libproc returns the number initialized.
    let read = unsafe {
        libc::proc_pidinfo(
            pid_i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return Err(format!(
            "managed Ollama process {pid} disappeared before identity capture"
        ));
    }
    // SAFETY: the exact-size success above initialized the whole proc_bsdinfo.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid || info.pbi_uid != unsafe { libc::getuid() } {
        return Err("managed Ollama process identity or owner did not match".to_string());
    }
    let process_group = unix_process_group(pid)?;
    let session_id = unix_session_id(pid)?;
    Ok(ManagedProcessIdentity {
        pid,
        process_group,
        session_id,
        parent_pid: info.pbi_ppid,
        uid: info.pbi_uid,
        start_seconds: info.pbi_start_tvsec,
        start_microseconds: info.pbi_start_tvusec,
    })
}

#[cfg(not(target_os = "macos"))]
pub(super) fn managed_process_identity(pid: u32) -> Result<ManagedProcessIdentity, String> {
    // Managed auto-launch is disabled off macOS. Keeping a PID-only identity here
    // lets lifecycle tests exercise the controller without claiming attestation.
    Ok(ManagedProcessIdentity {
        pid,
        #[cfg(unix)]
        process_group: unix_process_group(pid)?,
        #[cfg(unix)]
        session_id: unix_session_id(pid)?,
    })
}

#[cfg(unix)]
pub(super) fn unix_process_group(pid: u32) -> Result<u32, String> {
    let pid = i32::try_from(pid).map_err(|_| "managed process ID is invalid".to_string())?;
    // SAFETY: getpgid only queries kernel process metadata.
    let group = unsafe { libc::getpgid(pid) };
    u32::try_from(group).map_err(|_| "could not resolve managed process group".to_string())
}

#[cfg(unix)]
fn unix_session_id(pid: u32) -> Result<u32, String> {
    let pid = i32::try_from(pid).map_err(|_| "managed process ID is invalid".to_string())?;
    // SAFETY: getsid only queries kernel process metadata.
    let session = unsafe { libc::getsid(pid) };
    u32::try_from(session).map_err(|_| "could not resolve managed process session".to_string())
}

fn finish_unadmitted_managed_child(
    mut child: std::process::Child,
    private_home: Option<std::path::PathBuf>,
    #[cfg(unix)] dedicated_group: Option<u32>,
) {
    let _ = child.wait();
    #[cfg(unix)]
    if let Some(group) = dedicated_group.and_then(|group| i32::try_from(group).ok()) {
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            // SAFETY: this is the exact child PID/group created by the managed
            // launch's `setsid` pre-exec contract, and it was checked against the
            // caller's process group before the first signal.
            if unsafe { libc::kill(-group, 0) } == -1 {
                break;
            }
            // A descendant may have forked concurrently with admission failure.
            // Repeat the group signal until the dedicated group is empty.
            let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    cleanup_private_managed_home(private_home.as_deref());
}

/// Tear down a just-spawned managed runtime that failed admission. Managed launches
/// always call `setsid` before exec, so a failed/fast-exiting leader can already have
/// forked the actual server. Signal the verified/expected dedicated group first;
/// never send a negative-PID signal when the child joined the caller's group.
pub(super) fn terminate_unadmitted_managed_child(
    mut child: std::process::Child,
    private_home: Option<std::path::PathBuf>,
) {
    #[cfg(unix)]
    let dedicated_group = {
        let root = child.id();
        // SAFETY: getpgrp/getpgid only query process metadata; kill(group, 0)
        // probes the expected post-setsid group without changing it.
        let caller_group = u32::try_from(unsafe { libc::getpgrp() }).unwrap_or(0);
        let observed_group = i32::try_from(root)
            .ok()
            .and_then(|pid| u32::try_from(unsafe { libc::getpgid(pid) }).ok());
        let expected_group_exists = i32::try_from(root)
            .ok()
            .is_some_and(|group| unsafe { libc::kill(-group, 0) } == 0);
        let dedicated = root != 0
            && root != caller_group
            && (observed_group == Some(root)
                || (observed_group.is_none() && expected_group_exists));
        if dedicated {
            if let Ok(group) = i32::try_from(root) {
                // SAFETY: root is the newly spawned child's expected private group,
                // observed directly or still alive after its leader exited.
                let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
            }
            Some(root)
        } else {
            let _ = child.kill();
            None
        }
    };
    #[cfg(not(unix))]
    let _ = child.kill();

    let pending = Arc::new(Mutex::new(Some((child, private_home))));
    let reaper_pending = pending.clone();
    if std::thread::Builder::new()
        .name("aterm-ollama-admission-reaper".to_string())
        .spawn(move || {
            if let Some((child, private_home)) = reaper_pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                finish_unadmitted_managed_child(
                    child,
                    private_home,
                    #[cfg(unix)]
                    dedicated_group,
                );
            }
        })
        .is_err()
        && let Some((child, private_home)) = pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    {
        finish_unadmitted_managed_child(
            child,
            private_home,
            #[cfg(unix)]
            dedicated_group,
        );
    }
}

#[cfg(unix)]
fn owned_has_dedicated_process_group(owned: &OwnedOllamaChild) -> bool {
    // SAFETY: getpgrp only reads caller process metadata.
    let caller_group = u32::try_from(unsafe { libc::getpgrp() }).unwrap_or(0);
    owned.process.process_group == owned.process.pid
        && owned.process.session_id == owned.process.pid
        && owned.process.process_group != 0
        && owned.process.process_group != caller_group
}

fn signal_owned_ollama(owned: &mut OwnedOllamaChild) {
    #[cfg(unix)]
    {
        // Installation admitted only a child that became both process-group and
        // session leader. Recheck before a negative-PID kill so a corrupted record
        // can never target aterm's own process group.
        if owned_has_dedicated_process_group(owned) {
            if let Ok(group) = i32::try_from(owned.process.process_group) {
                // SAFETY: the validated negative group targets only the dedicated
                // managed session; SIGKILL prevents a descendant listener surviving
                // revocation after its direct parent exits.
                let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
            }
        } else {
            // Fail safe: never issue a group kill from an invalid ownership record.
            let _ = owned.child.kill();
        }
    }
    #[cfg(not(unix))]
    let _ = owned.child.kill();
}

fn terminate_owned_ollama(mut owned: OwnedOllamaChild) {
    signal_owned_ollama(&mut owned);

    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if owned.child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if owned.child.try_wait().ok().flatten().is_none() {
        let mut child = owned.child;
        let home = owned.private_home;
        let _ = std::thread::Builder::new()
            .name("aterm-ollama-group-reaper".to_string())
            .spawn(move || {
                let _ = child.wait();
                cleanup_private_managed_home(home.as_deref());
            });
        return;
    }

    #[cfg(unix)]
    if owned_has_dedicated_process_group(&owned) {
        let group = i32::try_from(owned.process.process_group).unwrap_or(0);
        let verify_deadline = Instant::now() + Duration::from_millis(250);
        while group > 0 && Instant::now() < verify_deadline {
            // SAFETY: signal 0 probes the already validated managed group.
            if unsafe { libc::kill(-group, 0) } == -1 {
                break;
            }
            // A descendant may have been between fork and signal delivery. Repeat
            // SIGKILL and wait until the accepted server group is empty.
            // SAFETY: same validated dedicated group as above.
            let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    cleanup_private_managed_home(owned.private_home.as_deref());
}

/// Revocation's UI-thread portion is only an atomic state detach plus kill(2).
/// Reaping the process group and removing its private HOME happen on a bounded
/// lifecycle thread. If thread creation is unavailable, fail closed synchronously.
fn terminate_owned_ollama_nonblocking(mut owned: OwnedOllamaChild) {
    signal_owned_ollama(&mut owned);
    let pending = Arc::new(Mutex::new(Some(owned)));
    let reaper_pending = pending.clone();
    if std::thread::Builder::new()
        .name("aterm-ollama-revocation".to_string())
        .spawn(move || {
            let owned = reaper_pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(owned) = owned {
                terminate_owned_ollama(owned);
            }
        })
        .is_err()
        && let Some(owned) = pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    {
        terminate_owned_ollama(owned);
    }
}

pub(super) struct OllamaFacts {
    pub(super) locality: TitleSummaryLocality,
    pub(super) effective_endpoint: String,
    pub(super) managed_install_present: bool,
    /// Exact spawned process that is permitted to own the request's connected
    /// server socket. `None` is mandatory for explicitly trusted external peers.
    pub(super) managed_process: Option<ManagedProcessIdentity>,
    /// Holds every opened closure file through the HTTP request that carries
    /// terminal context, narrowing the post-check substitution window.
    pub(super) runtime_attestation: Option<AttestedManagedOllama>,
}

pub(super) struct ManagedOllama {
    pub(super) controller: ManagedOllamaController,
    pub(super) runtime_paths: Option<(std::path::PathBuf, std::path::PathBuf)>,
    /// The selected manifest and every referenced blob remain open for this exact
    /// owned daemon authority. Their expensive hashes are checked only at launch.
    pub(super) _model_attestation: Option<AttestedManagedModel>,
}

impl ManagedOllama {
    pub(super) fn new(controller: ManagedOllamaController) -> Self {
        Self {
            controller,
            runtime_paths: None,
            _model_attestation: None,
        }
    }

    pub(super) fn managed_install_present(&self) -> bool {
        managed_ollama_paths().is_some_and(|(binary, _)| binary.is_file())
    }

    pub(super) fn reap(&mut self) -> Option<ManagedRuntimeExit> {
        let exited = self.controller.reap();
        if exited.is_some() {
            self.runtime_paths = None;
            self._model_attestation = None;
        }
        exited
    }

    pub(super) fn stop(&mut self) {
        self.controller.stop();
        self.runtime_paths = None;
        self._model_attestation = None;
    }

    /// Strict local mode may use only the child this aterm worker launched with
    /// cloud access disabled. A listener that predates that child is untrusted.
    pub(super) fn ensure(
        &mut self,
        job: &Job,
        authority_epoch: &Arc<AtomicU64>,
    ) -> Result<OllamaFacts, String> {
        let _ = self.reap();
        let install_present = self.managed_install_present();
        let automatic = job.settings.endpoint_origin == EndpointOrigin::AutomaticManaged;
        let owned = if automatic {
            self.controller.owned_for_authority(job.authority_epoch)
        } else {
            self.controller
                .endpoint_process(&job.settings.endpoint, job.authority_epoch)
                .map(|process| (process, job.settings.endpoint.clone()))
        };
        if let Some((process, endpoint)) = owned {
            let (binary, models) = self
                .runtime_paths
                .as_ref()
                .cloned()
                .or_else(managed_ollama_paths)
                .ok_or_else(|| "managed Ollama runtime path was lost".to_string())?;
            // Helpers and libraries are user-owned files. Re-attest the complete
            // closure before every request that carries terminal context, not only
            // once when the front daemon started.
            let security_attestation = (|| {
                let runtime = attest_managed_ollama(&binary, &models)?;
                // The launch-time pinned hash is intentionally not repeated for
                // every 3.39 GiB refresh. Revalidate inode/ctime/mtime/size/mode
                // identities, including all ancestors inside the sealed tree.
                self._model_attestation
                    .as_ref()
                    .ok_or_else(|| "managed model integrity anchor was lost".to_string())?
                    .revalidate()?;
                Ok::<_, String>(runtime)
            })();
            let runtime_attestation = match security_attestation {
                Ok(attestation) => attestation,
                Err(error) => {
                    self.invalidate_owned(&endpoint, job.authority_epoch);
                    return Err(error);
                }
            };
            return Ok(OllamaFacts {
                locality: TitleSummaryLocality::ManagedLocal,
                effective_endpoint: endpoint,
                managed_install_present: install_present,
                managed_process: Some(process),
                runtime_attestation: Some(runtime_attestation),
            });
        }
        // No owned process remains for this exact authority. Release the prior
        // model guards before considering an external listener or a fresh launch.
        self._model_attestation = None;
        let explicit_target = if automatic {
            None
        } else {
            let (socket, bind) = loopback_socket(&job.settings.endpoint)
                .ok_or_else(|| "could not resolve the loopback Ollama endpoint".to_string())?;
            if std::net::TcpStream::connect_timeout(&socket, Duration::from_millis(100)).is_ok() {
                if job.settings.allow_remote {
                    return Ok(OllamaFacts {
                        locality: TitleSummaryLocality::UnattestedLoopback,
                        effective_endpoint: job.settings.endpoint.clone(),
                        managed_install_present: install_present,
                        managed_process: None,
                        runtime_attestation: None,
                    });
                }
                return Err(
                    "loopback Ollama is not owned by aterm; enable remote/trusted endpoint access to use it"
                        .to_string(),
                );
            }
            Some(ManagedEndpointTarget {
                endpoint: job.settings.endpoint.clone(),
                socket,
                bind,
            })
        };
        // Reserve before expensive closure/model attestation. The listener keeps
        // this process's ephemeral choice unavailable until immediately before
        // spawn; a post-drop collision is detected by process/socket attestation
        // and retried with a newly reserved port.
        let mut reservation = automatic.then(reserve_managed_endpoint).transpose()?;
        let Some((binary, models)) = managed_ollama_paths() else {
            return Err("Ollama is not running and no managed runtime is installed".to_string());
        };
        if !binary.is_file() {
            return Err(format!(
                "Ollama is not running; managed runtime not found at {}",
                binary.display()
            ));
        }
        let attested = attest_managed_ollama(&binary, &models)?;
        // Model weights consume terminal context and therefore need an independent
        // supply-chain anchor. This streams and checks the pinned manifest blobs
        // once per owned daemon launch, before any process or request is authorized.
        let model_attestation = attest_managed_model(&attested.models, &job.settings.model)?;
        if !job_is_authorized(job, authority_epoch) {
            return Err(cancelled_error());
        }
        let attempts = if automatic {
            MANAGED_ENDPOINT_LAUNCH_ATTEMPTS
        } else {
            1
        };
        let mut last_error = "managed Ollama could not claim its endpoint".to_string();
        for attempt in 0..attempts {
            if !job_is_authorized(job, authority_epoch) {
                return Err(cancelled_error());
            }
            let target = if automatic {
                let reserved = match reservation.take() {
                    Some(reserved) => reserved,
                    None => reserve_managed_endpoint()?,
                };
                reserved.into_target()
            } else {
                explicit_target.clone().expect("explicit target was built")
            };
            match self.launch_attested(job, authority_epoch, &attested, &model_attestation, &target)
            {
                Ok(process) => {
                    self.runtime_paths = Some((attested.binary.clone(), attested.models.clone()));
                    self._model_attestation = Some(model_attestation);
                    return Ok(OllamaFacts {
                        locality: TitleSummaryLocality::ManagedLocal,
                        effective_endpoint: target.endpoint,
                        managed_install_present: true,
                        managed_process: Some(process),
                        runtime_attestation: Some(attested),
                    });
                }
                Err(error) => {
                    last_error = error;
                    if attempt + 1 < attempts {
                        reservation = Some(reserve_managed_endpoint()?);
                    }
                }
            }
        }
        Err(last_error)
    }

    fn launch_attested(
        &mut self,
        job: &Job,
        authority_epoch: &Arc<AtomicU64>,
        attested: &AttestedManagedOllama,
        model_attestation: &AttestedManagedModel,
        target: &ManagedEndpointTarget,
    ) -> Result<ManagedProcessIdentity, String> {
        let private_home = create_private_managed_home()?;
        let child = managed_ollama_command(
            &attested.binary,
            &target.bind,
            &attested.models,
            &private_home,
        )
        .spawn()
        .map_err(|error| {
            cleanup_private_managed_home(Some(&private_home));
            format!("could not start managed Ollama: {error}")
        })?;
        let child_pid = child.id();
        self.controller.install(
            child,
            target.endpoint.clone(),
            job.authority_epoch,
            Some(private_home),
        )?;
        if !job_is_authorized(job, authority_epoch) {
            self.controller
                .stop_if_owned(&target.endpoint, job.authority_epoch);
            return Err(cancelled_error());
        }
        // Bind the dynamic-code check to the exact spawned child. No terminal
        // context is sent before this and the established-stream check below pass.
        if let Err(error) = attest_running_managed_ollama(child_pid) {
            self.controller
                .stop_if_owned(&target.endpoint, job.authority_epoch);
            return Err(error);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !job_is_authorized(job, authority_epoch) {
                self.controller
                    .stop_if_owned(&target.endpoint, job.authority_epoch);
                return Err(cancelled_error());
            }
            let _ = self.reap();
            let Some(process) = self
                .controller
                .endpoint_process(&target.endpoint, job.authority_epoch)
            else {
                return Err("managed Ollama exited before becoming ready".to_string());
            };
            if let Ok(stream) =
                std::net::TcpStream::connect_timeout(&target.socket, Duration::from_millis(100))
            {
                #[cfg(target_os = "macos")]
                if let Err(error) = attest_managed_server_stream(&stream, process) {
                    drop(stream);
                    self.controller
                        .stop_if_owned(&target.endpoint, job.authority_epoch);
                    return Err(format!(
                        "managed Ollama readiness stream failed ownership attestation: {error}"
                    ));
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = stream;
                    self.controller
                        .stop_if_owned(&target.endpoint, job.authority_epoch);
                    return Err(
                        "managed Ollama stream attestation is unavailable on this platform"
                            .to_string(),
                    );
                }
                drop(stream);
                // Ollama lazily maps model weights on its first request. Warm it
                // with fixed, non-terminal data and pin it in memory, then recheck
                // the disk anchor. Thus no terminal context is the trigger for
                // loading potentially substituted weights. Subsequent requests
                // cheaply revalidate that same anchor before their write boundary.
                if let Err(error) = warm_managed_model(
                    job,
                    authority_epoch,
                    process,
                    &target.endpoint,
                    model_attestation,
                ) {
                    self.controller
                        .stop_if_owned(&target.endpoint, job.authority_epoch);
                    return Err(error);
                }
                return Ok(process);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.controller
            .stop_if_owned(&target.endpoint, job.authority_epoch);
        Err("managed Ollama did not become ready within 5 seconds".to_string())
    }

    pub(super) fn invalidate_owned(&mut self, endpoint: &str, authority_epoch: u64) {
        self.controller.stop_if_owned(endpoint, authority_epoch);
        self.runtime_paths = None;
        self._model_attestation = None;
    }
}

fn warm_managed_model(
    job: &Job,
    authority_epoch: &Arc<AtomicU64>,
    process: ManagedProcessIdentity,
    endpoint: &str,
    model_attestation: &AttestedManagedModel,
) -> Result<(), String> {
    if !job_is_authorized(job, authority_epoch) {
        return Err(cancelled_error());
    }
    let agent = build_agent(
        &job.settings,
        endpoint,
        Some(process),
        RequestWriteAuthority::for_job(job, authority_epoch.clone()),
    )?;
    let body = serde_json::json!({
        "model": job.settings.model,
        "messages": [
            {"role": "system", "content": "Reply with READY."},
            {"role": "user", "content": "health check"}
        ],
        "stream": false,
        "think": false,
        // Keep the verified mapping resident for this owned daemon authority.
        "keep_alive": -1,
        "options": {"temperature": 0, "num_predict": 4, "num_ctx": 256}
    });
    let mut response = agent
        .post(endpoint)
        .header("Content-Type", "application/json")
        .send_json(&body)
        .map_err(|error| format!("managed model warm-up failed: {error}"))?;
    let _: serde_json::Value = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_json()
        .map_err(|error| format!("managed model warm-up returned invalid JSON: {error}"))?;
    if !job_is_authorized(job, authority_epoch) {
        return Err(cancelled_error());
    }
    model_attestation.revalidate()
}

#[derive(Clone)]
pub(super) struct ManagedEndpointTarget {
    pub(super) endpoint: String,
    pub(super) socket: std::net::SocketAddr,
    bind: String,
}

pub(super) struct ReservedManagedEndpoint {
    listener: std::net::TcpListener,
    pub(super) target: ManagedEndpointTarget,
}

impl ReservedManagedEndpoint {
    pub(super) fn into_target(self) -> ManagedEndpointTarget {
        // This is the narrow unavoidable bind-to-exec seam. Readiness attests the
        // exact accepted peer process before any terminal bytes use the endpoint.
        drop(self.listener);
        self.target
    }
}

pub(super) fn reserve_managed_endpoint() -> Result<ReservedManagedEndpoint, String> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| "could not reserve a private managed Ollama endpoint".to_string())?;
    let socket = listener
        .local_addr()
        .map_err(|_| "could not inspect the private managed Ollama endpoint".to_string())?;
    let port = socket.port();
    if port == 0 {
        return Err("the operating system returned an invalid managed endpoint".to_string());
    }
    Ok(ReservedManagedEndpoint {
        listener,
        target: ManagedEndpointTarget {
            endpoint: format!("http://127.0.0.1:{port}/api/chat"),
            socket,
            bind: socket.to_string(),
        },
    })
}

static MANAGED_HOME_NONCE: AtomicU64 = AtomicU64::new(1);

pub(super) fn create_private_managed_home() -> Result<std::path::PathBuf, String> {
    #[cfg(unix)]
    use std::os::unix::fs::DirBuilderExt as _;

    let parent = std::env::temp_dir();
    for _ in 0..16 {
        let nonce = MANAGED_HOME_NONCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!("aterm-ollama-home-{}-{nonce}", std::process::id()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(&path) {
            Ok(()) => {
                validate_private_managed_home(&path, true)?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("could not create private managed Ollama state".to_string()),
        }
    }
    Err("could not allocate unique private managed Ollama state".to_string())
}

pub(super) fn validate_private_managed_home(
    path: &std::path::Path,
    require_empty: bool,
) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "could not inspect private managed Ollama state".to_string())?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err("private managed Ollama state must be a real directory".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: getuid only reads caller credentials.
        if metadata.uid() != unsafe { libc::getuid() } || metadata.mode() & 0o077 != 0 {
            return Err("private managed Ollama state is not owner-only".to_string());
        }
    }
    if require_empty
        && std::fs::read_dir(path)
            .map_err(|_| "could not inspect private managed Ollama state".to_string())?
            .next()
            .is_some()
    {
        return Err("private managed Ollama state was not empty before launch".to_string());
    }
    Ok(())
}

pub(super) fn cleanup_private_managed_home(path: Option<&std::path::Path>) {
    let Some(path) = path else { return };
    // Only remove a directory carrying our exact per-process basename. Validation
    // prevents following a substituted symlink at the cleanup seam.
    let owned_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| {
            name.starts_with(&format!("aterm-ollama-home-{}-", std::process::id()))
        });
    if owned_name && validate_private_managed_home(path, false).is_ok() {
        let _ = std::fs::remove_dir_all(path);
    }
}

pub(super) fn managed_ollama_command(
    binary: &std::path::Path,
    bind: &str,
    models: &std::path::Path,
    home: &std::path::Path,
) -> std::process::Command {
    let mut command = std::process::Command::new(binary);
    command.arg("serve");
    configure_managed_ollama_environment(&mut command, bind, models, home);
    command.current_dir(home);
    #[cfg(unix)]
    configure_dedicated_process_session(&mut command);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

#[cfg(unix)]
pub(super) fn configure_dedicated_process_session(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;

    // SAFETY: setsid is async-signal-safe and is the only operation performed
    // in the post-fork/pre-exec callback. Failure aborts exec, fail-closed.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

pub(super) fn configure_managed_ollama_environment(
    command: &mut std::process::Command,
    bind: &str,
    models: &std::path::Path,
    home: &std::path::Path,
) {
    // Never pass terminal/session secrets, proxy overrides, DYLD injection, or
    // credential-helper variables into the model daemon. Ollama requires HOME; its
    // private managed root is sufficient and keeps any runtime files in scope.
    command
        .env_clear()
        .env("HOME", home)
        .env("OLLAMA_HOST", bind)
        .env("OLLAMA_MODELS", models)
        .env("OLLAMA_NO_CLOUD", "1")
        .env("OLLAMA_NOHISTORY", "1");
}

pub(super) struct AttestedManagedOllama {
    pub(super) binary: std::path::PathBuf,
    pub(super) models: std::path::PathBuf,
    #[cfg(target_os = "macos")]
    pub(super) _closure_guards: Vec<std::fs::File>,
}

#[cfg(target_os = "macos")]
pub(super) const OLLAMA_TEAM_ID: &str = "3MU9H2V9Y9";
#[cfg(target_os = "macos")]
pub(super) const OLLAMA_CODE_IDENTIFIER: &str = "ai.ollama.ollama";

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ManagedPathKind {
    Executable,
    CodeFile,
    Directory,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ManagedPathPolicyFacts {
    pub(super) within_root: bool,
    pub(super) kind_matches: bool,
    pub(super) owner_matches: bool,
    pub(super) mode: u32,
}

#[cfg(target_os = "macos")]
pub(super) fn validate_managed_path_policy(
    kind: ManagedPathKind,
    facts: ManagedPathPolicyFacts,
) -> Result<(), String> {
    if !facts.within_root {
        return Err("managed Ollama path resolves outside its managed root".to_string());
    }
    if !facts.kind_matches {
        return Err(match kind {
            ManagedPathKind::Executable => {
                "managed Ollama executable is not a regular file".to_string()
            }
            ManagedPathKind::CodeFile => {
                "managed Ollama code closure contains a non-regular file".to_string()
            }
            ManagedPathKind::Directory => {
                "managed Ollama path component is not a directory".to_string()
            }
        });
    }
    if !facts.owner_matches {
        return Err("managed Ollama files must be owned by the current user".to_string());
    }
    if facts.mode & 0o222 != 0 {
        return Err(
            "managed Ollama runtime must be sealed read-only before attestation".to_string(),
        );
    }
    match kind {
        ManagedPathKind::Executable if facts.mode & 0o111 == 0 => {
            Err("managed Ollama executable has no execute bit".to_string())
        }
        ManagedPathKind::Executable | ManagedPathKind::CodeFile if facts.mode & 0o6000 != 0 => {
            Err("managed Ollama code must not be set-id".to_string())
        }
        ManagedPathKind::Directory if facts.mode & 0o100 == 0 => {
            Err("managed Ollama directory is not owner-searchable".to_string())
        }
        _ => Ok(()),
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManagedFileIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    owner: u32,
    mode: u32,
}

#[cfg(target_os = "macos")]
fn managed_file_identity(metadata: &std::fs::Metadata) -> ManagedFileIdentity {
    use std::os::unix::fs::MetadataExt as _;
    ManagedFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        owner: metadata.uid(),
        mode: metadata.mode(),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn ollama_designated_requirement(
    team: &str,
    identifier: &str,
) -> Result<String, String> {
    let safe = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    };
    if !safe(team) || !safe(identifier) {
        return Err("invalid pinned Ollama code identity".to_string());
    }
    Ok(format!(
        "=anchor apple generic \
         and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{team}\" \
         and identifier \"{identifier}\""
    ))
}

#[cfg(target_os = "macos")]
fn ollama_codesign_command(all_architectures: bool) -> Result<std::process::Command, String> {
    let requirement = ollama_designated_requirement(OLLAMA_TEAM_ID, OLLAMA_CODE_IDENTIFIER)?;
    let mut command = std::process::Command::new("/usr/bin/codesign");
    command
        .env_clear()
        .args(["--verify", "--strict", "--verbose=2"]);
    if all_architectures {
        command.arg("--all-architectures");
    }
    command.arg("-R").arg(requirement);
    Ok(command)
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
pub(super) struct BoundedCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
}

/// Execute a security helper without allowing an inherited tool failure to hang
/// the title worker or allocate unbounded output. Unix helpers get a dedicated
/// process group, and timeout paths never wait for an inherited stdout pipe.
#[cfg(any(target_os = "macos", test))]
pub(super) fn run_command_bounded(
    command: &mut std::process::Command,
    timeout: Duration,
    stdout_limit: usize,
    label: &str,
) -> Result<BoundedCommandOutput, String> {
    use std::io::Read as _;

    command
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdout(if stdout_limit == 0 {
            std::process::Stdio::null()
        } else {
            std::process::Stdio::piped()
        });
    #[cfg(unix)]
    configure_dedicated_process_session(command);
    let mut child = command
        .spawn()
        .map_err(|_| format!("could not run {label}"))?;
    let (reader_tx, reader_rx) = mpsc::sync_channel(1);
    let reader = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            let mut bytes = Vec::with_capacity(stdout_limit.saturating_add(1).min(64 * 1024));
            let result = stdout
                .take(u64::try_from(stdout_limit.saturating_add(1)).unwrap_or(u64::MAX))
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = reader_tx.send(result);
        })
    });
    let deadline = Instant::now() + timeout;
    let terminate = |child: &mut std::process::Child| {
        #[cfg(unix)]
        if let Ok(group) = i32::try_from(child.id()) {
            // SAFETY: setsid above made the exact child PID its private group ID.
            let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        #[cfg(not(unix))]
        let _ = child.kill();
        let _ = child.wait();
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(None) => {
                terminate(&mut child);
                // The group kill closes ordinary inherited pipes. Deliberately do
                // not join here: even a hostile escaped descendant cannot extend
                // this function past its wall-clock deadline.
                drop(reader);
                return Err(format!("{label} timed out"));
            }
            Err(_) => {
                terminate(&mut child);
                drop(reader);
                return Err(format!("could not inspect {label} status"));
            }
        }
    };
    let stdout = match reader {
        Some(reader) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let result = reader_rx.recv_timeout(remaining).map_err(|_| {
                terminate(&mut child);
                format!("{label} timed out")
            })?;
            reader
                .join()
                .map_err(|_| format!("{label} output reader failed"))?;
            result.map_err(|_| format!("could not read {label} output"))?
        }
        None => Vec::new(),
    };
    if stdout.len() > stdout_limit {
        return Err(format!("{label} output exceeded its byte limit"));
    }
    Ok(BoundedCommandOutput { status, stdout })
}

#[cfg(target_os = "macos")]
pub(super) fn verify_ollama_code_paths(paths: &[std::path::PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("managed Ollama runtime contains no signed code files".to_string());
    }
    let output = run_command_bounded(
        ollama_codesign_command(true)?.args(paths),
        Duration::from_secs(10),
        0,
        "Ollama signature verification",
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err("managed Ollama failed its pinned Apple Developer-ID signature check".to_string())
    }
}

#[cfg(target_os = "macos")]
fn verify_running_ollama_code(pid: u32) -> Result<(), String> {
    let output = run_command_bounded(
        ollama_codesign_command(false)?.arg(pid.to_string()),
        Duration::from_secs(5),
        0,
        "dynamic Ollama verification",
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err("spawned Ollama failed its pinned dynamic code requirement".to_string())
    }
}

#[cfg(target_os = "macos")]
fn lsof_endpoint(address: std::net::SocketAddr) -> String {
    match address {
        std::net::SocketAddr::V4(address) => address.to_string(),
        std::net::SocketAddr::V6(address) => {
            format!("[{}]:{}", address.ip(), address.port())
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) const NO_ESTABLISHED_SERVER_OWNER: &str =
    "the connected Ollama socket had no identifiable server owner";
#[cfg(target_os = "macos")]
pub(super) const MULTIPLE_ESTABLISHED_SERVER_OWNERS: &str =
    "the connected Ollama socket had multiple possible server owners";

#[cfg(target_os = "macos")]
pub(super) fn socket_owner_observation_is_transient(error: &str) -> bool {
    matches!(
        error,
        NO_ESTABLISHED_SERVER_OWNER | MULTIPLE_ESTABLISHED_SERVER_OWNERS
    )
}

#[cfg(target_os = "macos")]
pub(super) fn parse_established_server_pid(
    output: &[u8],
    server: std::net::SocketAddr,
    client: std::net::SocketAddr,
) -> Result<u32, String> {
    const MAX_LSOF_OUTPUT_BYTES: usize = 64 * 1024;
    const MAX_LSOF_LINES: usize = 2048;
    if output.len() > MAX_LSOF_OUTPUT_BYTES {
        return Err("listener ownership output exceeded 64 KiB".to_string());
    }
    let text = std::str::from_utf8(output)
        .map_err(|_| "listener ownership output was not UTF-8".to_string())?;
    let expected_name = format!("{}->{}", lsof_endpoint(server), lsof_endpoint(client));
    let mut current_pid = None;
    let mut file_name: Option<&str> = None;
    let mut established = false;
    let mut candidates = HashSet::new();
    let mut finish_file = |pid: Option<u32>, name: Option<&str>, is_established: bool| {
        if is_established
            && name == Some(expected_name.as_str())
            && let Some(pid) = pid
        {
            candidates.insert(pid);
        }
    };
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_LSOF_LINES || line.len() > 1024 {
            return Err("listener ownership output exceeded its structural bound".to_string());
        }
        let Some((field, value)) = line.as_bytes().split_first() else {
            continue;
        };
        match *field {
            b'p' => {
                finish_file(current_pid, file_name, established);
                current_pid = std::str::from_utf8(value)
                    .ok()
                    .and_then(|pid| pid.parse().ok());
                file_name = None;
                established = false;
            }
            b'f' => {
                finish_file(current_pid, file_name, established);
                file_name = None;
                established = false;
            }
            b'n' => file_name = std::str::from_utf8(value).ok(),
            b'T' if value == b"ST=ESTABLISHED" => established = true,
            _ => {}
        }
    }
    finish_file(current_pid, file_name, established);
    match candidates.len() {
        1 => Ok(*candidates.iter().next().expect("one candidate")),
        0 => Err(NO_ESTABLISHED_SERVER_OWNER.to_string()),
        _ => Err(MULTIPLE_ESTABLISHED_SERVER_OWNERS.to_string()),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn established_server_pid(stream: &std::net::TcpStream) -> Result<u32, String> {
    let server = stream
        .peer_addr()
        .map_err(|error| format!("managed Ollama peer address: {error}"))?;
    let client = stream
        .local_addr()
        .map_err(|error| format!("managed Ollama client address: {error}"))?;
    let selector = match server {
        std::net::SocketAddr::V4(address) => {
            format!("-iTCP@{}:{}", address.ip(), address.port())
        }
        std::net::SocketAddr::V6(address) => {
            format!("-iTCP@[{}]:{}", address.ip(), address.port())
        }
    };
    // connect(2) can complete while the server-side socket is still queued for
    // accept(2), in which case lsof briefly sees only the client half. A process
    // concurrently between fork(2) and exec(2) can likewise expose a second,
    // inherited view of that exact FD for one snapshot. Retry both transient
    // shapes, but still require one unique server owner within the tight bound.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("could not identify the connected Ollama server process".to_string());
        }
        let output = run_command_bounded(
            std::process::Command::new("/usr/sbin/lsof")
                .env_clear()
                .args(["-nP", "-a"])
                .arg(&selector)
                .args(["-sTCP:ESTABLISHED", "-FpnT"]),
            remaining.min(Duration::from_millis(250)),
            64 * 1024,
            "managed Ollama listener inspection",
        )?;
        if output.status.success() {
            match parse_established_server_pid(&output.stdout, server, client) {
                Ok(pid) => return Ok(pid),
                Err(error)
                    if socket_owner_observation_is_transient(&error)
                        && Instant::now() < deadline => {}
                Err(error) => return Err(error),
            }
        } else if Instant::now() >= deadline {
            return Err("could not identify the connected Ollama server process".to_string());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "macos")]
pub(super) fn validate_managed_process_ancestry(
    candidate_pid: u32,
    root: ManagedProcessIdentity,
) -> Result<(), String> {
    const MAX_ANCESTRY_DEPTH: usize = 64;
    let root_now = managed_process_identity(root.pid)?;
    if root_now != root {
        return Err("managed Ollama root process identity changed".to_string());
    }
    let mut path = Vec::new();
    let mut cursor = candidate_pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if path
            .iter()
            .any(|identity: &ManagedProcessIdentity| identity.pid == cursor)
        {
            return Err("managed Ollama process ancestry contained a cycle".to_string());
        }
        let identity = managed_process_identity(cursor)?;
        if identity.uid != root.uid {
            return Err("connected Ollama server was owned by another user".to_string());
        }
        if identity.process_group != root.process_group || identity.session_id != root.session_id {
            return Err(
                "connected Ollama descendant escaped its dedicated process session".to_string(),
            );
        }
        path.push(identity);
        if identity.pid == root.pid {
            if identity != root {
                return Err("connected Ollama root PID was reused".to_string());
            }
            for expected in path {
                if managed_process_identity(expected.pid)? != expected {
                    return Err(
                        "connected Ollama process ancestry changed during verification".to_string(),
                    );
                }
            }
            return Ok(());
        }
        if identity.parent_pid <= 1 || identity.parent_pid == identity.pid {
            return Err("connected Ollama server is not a managed descendant".to_string());
        }
        cursor = identity.parent_pid;
    }
    Err("connected Ollama process ancestry exceeded 64 processes".to_string())
}

#[cfg(target_os = "macos")]
pub(super) fn attest_managed_server_stream(
    stream: &std::net::TcpStream,
    root: ManagedProcessIdentity,
) -> Result<(), String> {
    let listener_pid = established_server_pid(stream)?;
    validate_managed_process_ancestry(listener_pid, root)?;
    verify_running_ollama_code(listener_pid)?;
    // Re-read both the exact socket owner and its ancestry after codesign. The
    // stream remains open throughout; a rebind can never inherit this connection.
    if established_server_pid(stream)? != listener_pid {
        return Err("connected Ollama server owner changed during attestation".to_string());
    }
    validate_managed_process_ancestry(listener_pid, root)
}

#[cfg(target_os = "macos")]
pub(super) fn attest_managed_ollama(
    binary: &std::path::Path,
    models: &std::path::Path,
) -> Result<AttestedManagedOllama, String> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    const MAX_RUNTIME_CLOSURE_ENTRIES: usize = 256;

    let root = models
        .parent()
        .ok_or_else(|| "managed Ollama root is unavailable".to_string())?;
    let root_link = std::fs::symlink_metadata(root)
        .map_err(|error| format!("managed Ollama root {}: {error}", root.display()))?;
    if !root_link.file_type().is_dir() {
        return Err("managed Ollama root must be a real directory, not a link".to_string());
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("managed Ollama root {}: {error}", root.display()))?;
    let canonical_binary = binary
        .canonicalize()
        .map_err(|error| format!("managed Ollama executable {}: {error}", binary.display()))?;
    let canonical_models = models
        .canonicalize()
        .map_err(|error| format!("managed Ollama models {}: {error}", models.display()))?;
    let runtime_directory = canonical_binary
        .parent()
        .ok_or_else(|| "managed Ollama runtime directory is unavailable".to_string())?
        .to_path_buf();
    if !runtime_directory.starts_with(&canonical_root) || runtime_directory == canonical_root {
        return Err("managed Ollama runtime resolves outside its managed root".to_string());
    }
    let uid = {
        // SAFETY: getuid has no arguments and cannot fail.
        unsafe { libc::getuid() }
    };
    let check_directory = |path: &std::path::Path| -> Result<(), String> {
        let metadata = std::fs::metadata(path)
            .map_err(|error| format!("managed Ollama directory {}: {error}", path.display()))?;
        validate_managed_path_policy(
            ManagedPathKind::Directory,
            ManagedPathPolicyFacts {
                within_root: path.starts_with(&canonical_root),
                kind_matches: metadata.file_type().is_dir(),
                owner_matches: metadata.uid() == uid,
                mode: metadata.mode(),
            },
        )
    };
    check_directory(&canonical_root)?;
    check_directory(&canonical_models)?;
    check_directory(&runtime_directory)?;

    // Enumerate the complete extracted runtime closure, not only the front binary:
    // Ollama later executes llama-server and loads adjacent dylib/so/metallib files.
    // Every regular file in this bounded closure is signed today; treating an
    // unknown unsigned addition as data would silently reopen helper substitution.
    let mut pending_directories = vec![runtime_directory.clone()];
    let mut identities: Vec<(std::path::PathBuf, ManagedFileIdentity, bool)> = Vec::new();
    let mut code_paths = HashSet::new();
    let mut symlink_targets = Vec::new();
    let mut guards = Vec::new();
    let mut entry_count = 0usize;
    while let Some(directory) = pending_directories.pop() {
        let directory_metadata = std::fs::symlink_metadata(&directory).map_err(|error| {
            format!("managed Ollama directory {}: {error}", directory.display())
        })?;
        validate_managed_path_policy(
            ManagedPathKind::Directory,
            ManagedPathPolicyFacts {
                within_root: directory.starts_with(&runtime_directory),
                kind_matches: directory_metadata.file_type().is_dir(),
                owner_matches: directory_metadata.uid() == uid,
                mode: directory_metadata.mode(),
            },
        )?;
        identities.push((
            directory.clone(),
            managed_file_identity(&directory_metadata),
            false,
        ));
        let entries = std::fs::read_dir(&directory).map_err(|error| {
            format!("managed Ollama directory {}: {error}", directory.display())
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("managed Ollama runtime entry: {error}"))?;
            entry_count = entry_count.saturating_add(1);
            if entry_count > MAX_RUNTIME_CLOSURE_ENTRIES {
                return Err("managed Ollama runtime closure exceeds 256 entries".to_string());
            }
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                format!("managed Ollama runtime entry {}: {error}", path.display())
            })?;
            // Symlink permission bits are not access-control bits on macOS (lstat
            // commonly reports 0777). The sealed containing directory prevents
            // replacement; the target must still stay inside the closure and its
            // real file is independently opened, sealed, signed, and identity-bound.
            if metadata.uid() != uid
                || (!metadata.file_type().is_symlink() && metadata.mode() & 0o222 != 0)
            {
                return Err(format!(
                    "managed Ollama runtime entry {} is not owner-matched and sealed read-only",
                    path.display()
                ));
            }
            identities.push((
                path.clone(),
                managed_file_identity(&metadata),
                metadata.file_type().is_symlink(),
            ));
            if metadata.file_type().is_symlink() {
                let target = path
                    .canonicalize()
                    .map_err(|error| format!("managed Ollama link {}: {error}", path.display()))?;
                if !target.starts_with(&runtime_directory)
                    || !std::fs::metadata(&target).is_ok_and(|target| target.is_file())
                {
                    return Err(format!(
                        "managed Ollama link {} escapes or does not resolve to code",
                        path.display()
                    ));
                }
                symlink_targets.push(target);
            } else if metadata.file_type().is_dir() {
                pending_directories.push(path);
            } else if metadata.file_type().is_file() {
                let guard = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)
                    .map_err(|error| {
                        format!("managed Ollama code file {}: {error}", path.display())
                    })?;
                let opened = guard.metadata().map_err(|error| {
                    format!("managed Ollama code metadata {}: {error}", path.display())
                })?;
                validate_managed_path_policy(
                    if path == canonical_binary {
                        ManagedPathKind::Executable
                    } else {
                        ManagedPathKind::CodeFile
                    },
                    ManagedPathPolicyFacts {
                        within_root: path.starts_with(&runtime_directory),
                        kind_matches: opened.file_type().is_file(),
                        owner_matches: opened.uid() == uid,
                        mode: opened.mode(),
                    },
                )?;
                if managed_file_identity(&opened) != managed_file_identity(&metadata) {
                    return Err(format!(
                        "managed Ollama code file {} changed while opening",
                        path.display()
                    ));
                }
                code_paths.insert(path);
                guards.push(guard);
            } else {
                return Err(format!(
                    "managed Ollama runtime contains unsupported entry {}",
                    path.display()
                ));
            }
        }
    }
    if !code_paths.contains(&canonical_binary) {
        return Err("managed Ollama executable is not in its runtime closure".to_string());
    }
    if symlink_targets
        .iter()
        .any(|target| !code_paths.contains(target))
    {
        return Err("managed Ollama link target is missing from the signed closure".to_string());
    }
    let mut code_paths: Vec<_> = code_paths.into_iter().collect();
    code_paths.sort();
    verify_ollama_code_paths(&code_paths)?;

    // Bind directory membership, symlink targets, and each open regular-file
    // identity across verification. Dynamic PID verification below then binds the
    // actually spawned main executable; hardened-runtime library validation retains
    // the Apple Team boundary for code loaded by that process.
    for (path, before, symlink) in identities {
        let after = if symlink {
            std::fs::symlink_metadata(&path)
        } else {
            std::fs::metadata(&path)
        }
        .map_err(|error| {
            format!(
                "managed Ollama closure changed at {}: {error}",
                path.display()
            )
        })?;
        if managed_file_identity(&after) != before {
            return Err(format!(
                "managed Ollama runtime closure changed during attestation at {}",
                path.display()
            ));
        }
    }
    if canonical_binary
        != binary
            .canonicalize()
            .map_err(|error| format!("managed Ollama executable changed: {error}"))?
    {
        return Err("managed Ollama current runtime changed during attestation".to_string());
    }
    Ok(AttestedManagedOllama {
        binary: canonical_binary,
        models: canonical_models,
        _closure_guards: guards,
    })
}

#[cfg(not(target_os = "macos"))]
pub(super) fn attest_managed_ollama(
    _binary: &std::path::Path,
    _models: &std::path::Path,
) -> Result<AttestedManagedOllama, String> {
    Err("automatic managed Ollama launch is disabled on this platform because no runtime attestation anchor is implemented; start Ollama separately and explicitly trust the endpoint".to_string())
}

#[cfg(target_os = "macos")]
pub(super) fn attest_running_managed_ollama(pid: u32) -> Result<(), String> {
    verify_running_ollama_code(pid)
        .map_err(|error| format!("spawned Ollama failed dynamic code attestation: {error}"))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn attest_running_managed_ollama(_pid: u32) -> Result<(), String> {
    Err("managed Ollama dynamic code attestation is unavailable on this platform".to_string())
}

pub(super) fn managed_ollama_paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    #[cfg(target_os = "macos")]
    {
        let root = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)?
            .join("Library/Application Support/aterm/llm");
        return Some((root.join("current/ollama"), root.join("models")));
    }
    #[cfg(target_os = "linux")]
    {
        let data = std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|home| home.join(".local/share"))
            })?;
        let root = data.join("aterm/llm");
        return Some((root.join("current/ollama"), root.join("models")));
    }
    #[cfg(windows)]
    {
        let root = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)?
            .join("aterm/llm");
        return Some((root.join("current/ollama.exe"), root.join("models")));
    }
    #[allow(unreachable_code)]
    None
}
