// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private app-server startup by an actual foreground SDK owner.
use super::*;
use aterm_uds::spawnfd::BirthAttempt;
use std::os::fd::AsFd as _;
use std::path::Path;

#[path = "codex_source_backend_endpoint.rs"]
mod endpoint;

/// Configuration inputs, not command-line authority. The launcher supplies the
/// subcommand, private loopback listener and authentication itself. Root config
/// overrides are separate argv values, never a shell command or inherited task.
#[derive(Default)]
pub struct BackendConfiguration {
    overrides: Vec<String>,
}

impl BackendConfiguration {
    /// Retain bounded CLI configuration for this root's independent constructor.
    /// The SDK still selects and validates its actual credential implementation.
    ///
    /// # Errors
    /// Inputs contain NULs, exceed 64 KiB, or name an empty override.
    pub fn new(overrides: Vec<String>) -> Result<Self, Error> {
        let mut bytes = 0_usize;
        for value in &overrides {
            bytes = bytes.checked_add(value.len()).ok_or(Error::Denied)?;
            if value.is_empty() || value.contains('\0') || bytes > 64 * 1024 {
                return Err(Error::Denied);
            }
        }
        if overrides.len() > 128 {
            return Err(Error::Denied);
        }
        Ok(Self { overrides })
    }

    fn arguments(self, digest: String) -> Vec<String> {
        // Request early physical binding in the SDK before configuration consumes
        // its constructor. This marker supplies no native admission authority.
        let mut args = vec!["--alab-owned-backend".into()];
        for value in self.overrides {
            args.extend(["--config".into(), value]);
        }
        args.extend([
            "app-server".into(),
            "--listen".into(),
            "ws://127.0.0.1:0".into(),
            "--ws-auth".into(),
            "capability-token".into(),
            "--ws-token-sha256".into(),
            digest,
        ]);
        args
    }
}

/// Native inputs for one independently reserved backend birth. The actual
/// frontend/PTY and admitted executable cannot be reconstructed from DATA.
pub struct BackendStart<'a> {
    pub frontend: &'a FrontendMember<'a>,
    pub admitted: worker::Admitted,
    pub cli: &'a worker::WorkerCli,
    pub cwd: &'a Path,
    pub configuration: BackendConfiguration,
    pub attempt: BirthAttempt,
}

/// Startup custody only. A running service must still own root lifetime,
/// descendant resources, authenticated handoff and request/event receipts.
/// Drop retains OwnedWorker's original-cohort cleanup; use off the GUI thread.
#[must_use = "retain and supervise the actual backend child"]
pub struct OwnedBackend {
    worker: OwnedWorker,
    control: aterm_uds::CtlStream,
    frontend: frontend_control::FrontendIdentity,
    token: aterm_secrets::SecretBytes,
    stderr: Option<std::process::ChildStderr>,
    endpoint: endpoint::Observation,
}

impl OwnedLaunchReservation {
    /// Start the independently armed image as a private authenticated app-server.
    /// The plaintext bearer remains in native memory; argv contains its digest.
    /// No app-server endpoint or SDK readiness is inferred from successful spawn.
    ///
    /// # Errors
    /// Native frontend/constructor admission fails, entropy or preparation fails,
    /// or native spawn/bind fails. Every refusal carries the actual birth outcome.
    pub fn spawn_backend(self, start: BackendStart<'_>) -> Result<OwnedBackend, OwnedSpawnFailure> {
        if start.cli.agent != aterm_phase::Program::Codex {
            return Err(super::spawn::before_birth(Error::Denied, start.attempt));
        }
        let sid = match self.spawn_session() {
            Ok(sid) => sid,
            Err(error) => return Err(super::spawn::before_birth(error, start.attempt)),
        };
        if !start.frontend.admits(&self.inner.state, &sid) {
            return Err(super::spawn::before_birth(Error::Denied, start.attempt));
        }
        let token = aterm_uds::rand::hex_token::<32>()
            .map(|token| aterm_secrets::SecretBytes::new(token.into_bytes()));
        let token = match token {
            Ok(token) => token,
            Err(_) => {
                return Err(super::spawn::before_birth(
                    Error::Unavailable,
                    start.attempt,
                ));
            }
        };
        let digest =
            aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(token.as_slice()));
        let args = start.configuration.arguments(digest);
        let admitted = start.admitted.prepare(|mut starter| {
            starter.args = Ok(args);
            Ok((starter, ()))
        });
        let admitted = match admitted {
            Ok((admitted, ())) => admitted,
            Err(_) => return Err(super::spawn::before_birth(Error::Denied, start.attempt)),
        };
        if !start.frontend.admits(&self.inner.state, &sid) {
            return Err(super::spawn::before_birth(Error::Stale, start.attempt));
        }
        let frontend = start.frontend.identity();
        let (worker, control) =
            self.spawn_with_required_control(admitted, start.cwd, start.attempt)?;
        Ok(OwnedBackend {
            worker,
            control,
            frontend,
            token,
            stderr: None,
            endpoint: endpoint::Observation::default(),
        })
    }
}

impl OwnedBackend {
    /// The original native-owned bootstrap channel, separate from app-server
    /// RPC and diagnostics. Possession alone grants neither SDK readiness nor
    /// frontend attachment; the child must bind its challenge through the SDK.
    pub fn control(&mut self) -> &mut aterm_uds::CtlStream {
        &mut self.control
    }

    /// Drain diagnostics after listener observation. Startup owns this actual
    /// pipe until its bounded observation completes; no other reader may race it.
    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.endpoint.address()?;
        self.stderr.take()
    }

    /// Observe this actual child's private listener from its retained pipe.
    /// This supplies an address only, not SDK readiness or attachment authority.
    /// On refusal the actual child remains owned and must still be settled.
    ///
    /// # Errors
    /// Ownership changes, the deadline expires, diagnostics exceed their bound,
    /// the child closes the pipe, or the announced endpoint is not private.
    pub fn observe_endpoint(
        &mut self,
        frontend: &FrontendMember<'_>,
        deadline: std::time::Instant,
    ) -> std::io::Result<std::net::SocketAddr> {
        self.frontend.check_foreground(frontend).map_err(|error| {
            std::io::Error::new(
                if error == Error::Busy {
                    std::io::ErrorKind::WouldBlock
                } else {
                    std::io::ErrorKind::PermissionDenied
                },
                "backend startup owner unavailable",
            )
        })?;
        if self.worker.process().is_err() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "backend startup owner changed",
            ));
        }
        if let Some(address) = self.endpoint.address() {
            return Ok(address);
        }
        let deadline = deadline.min(std::time::Instant::now() + std::time::Duration::from_secs(60));
        if self.stderr.is_none() && self.endpoint.address().is_none() {
            self.stderr = self.worker.take_stderr();
        }
        let stderr = self.stderr.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "backend diagnostics are unavailable",
            )
        })?;
        let descriptor = stderr.as_fd().try_clone_to_owned()?;
        self.endpoint
            .read(stderr, descriptor.as_fd(), deadline, || {
                self.frontend.check_foreground(frontend).is_ok() && self.worker.process().is_ok()
            })
    }

    /// Drain this actual child's output so startup cannot block on a full pipe.
    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.worker.take_stdout()
    }

    /// Observe the current original child, not a PID supplied by the frontend.
    ///
    /// # Errors
    /// Original custody is stale or the kernel cannot observe the child.
    pub fn process(&mut self) -> Result<Process, Error> {
        self.worker.process()
    }

    /// Borrow the private transport credential only for the original actual
    /// frontend after this child completed SDK construction. This is not a root
    /// attachment grant or an exec handoff: a replacement image needs separate
    /// native handoff admission even when PID and birth are unchanged.
    ///
    /// # Errors
    /// The foreground owner changed or SDK construction is not currently known.
    pub fn transport_credential(
        &mut self,
        frontend: &FrontendMember<'_>,
    ) -> Result<&aterm_secrets::SecretBytes, Error> {
        self.frontend.check_foreground(frontend)?;
        if self.endpoint.address().is_none() {
            return Err(Error::Busy);
        }
        self.worker.check_sdk_construction()?;
        self.worker.check_control_binding()?;
        self.frontend.check_foreground(frontend)?;
        Ok(&self.token)
    }

    /// Settle this original cohort. Separate tool groups remain the running
    /// supervisor's resources; this startup owner cannot assert their closure.
    ///
    /// # Errors
    /// Original custody or native termination/wait fails. WouldBlock means
    /// original-group retirement is still pending, even after the leader's reap.
    /// The owner is retained; no result covers separately owned tool groups.
    pub fn stop(&mut self) -> std::io::Result<()> {
        self.worker.stop()
    }
}

impl std::fmt::Debug for OwnedBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OwnedNativeBackendStartup(redacted)")
    }
}
