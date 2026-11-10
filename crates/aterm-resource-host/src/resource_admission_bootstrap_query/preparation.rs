// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One selected Keeper supplies every executable/runtime input. This is not
//! an SDK selection API and creates no GNU credit or native launch authority.

use super::{alive, capture::Capture, refused, sdk, spawnfd, wire};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    os::{
        fd::{AsRawFd as _, OwnedFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{FileTypeExt as _, MetadataExt as _},
        },
    },
    sync::Arc,
    time::{Duration, Instant},
};
use trust_resource_controller::{MetadataFile, MetadataPrepared, bootstrap_graph::GnuIdentity};
use trust_resource_provenance::program::{FileObservation, HeldProgram};

/// SAME original selected inputs survive the Root. Exhausted query-only
/// descriptions are absent; this type has no constructor from observations.
pub(super) struct RetainedInputs {
    _sdk: Arc<sdk::Keeper>,
    _held: Arc<HeldProgram>,
    _gnu: Arc<[OwnedFd; 2]>,
}

pub(in crate::actor::owner) struct PreparedObservation {
    pub(in crate::actor::owner) inventory: MetadataPrepared,
    pub(in crate::actor::owner) request_sha256: [u8; 32],
    pub(in crate::actor::owner) deadline: Instant,
    pub(in crate::actor::owner) stdout: super::capture::CaptureObservation,
    pub(in crate::actor::owner) stderr: super::capture::CaptureObservation,
}

pub(super) struct Prepared {
    pub(super) request: wire::Request,
    pub(super) digest: [u8; 32],
    pub(super) deadline: Instant,
    pub(super) sdk: Arc<sdk::Keeper>,
    pub(super) held: Arc<HeldProgram>,
    pub(super) inventory: MetadataPrepared,
    pub(super) command: Option<spawnfd::Command>,
    pub(super) prelude: crate::metadata_prelude::Owner,
    pub(super) gnu: Arc<[OwnedFd; 2]>,
    pub(super) handoff: jobserver_utils::ScopedHandoff,
    pub(super) stdout: Capture,
    pub(super) stderr: Capture,
}

pub(super) fn observed_file(item: &FileObservation) -> io::Result<MetadataFile> {
    let mut sha256 = [0; 32];
    if !aterm_codec::hex::decode_into(item.sha256.as_bytes(), &mut sha256) {
        return Err(refused("original SDK query digest is malformed"));
    }
    Ok(MetadataFile {
        path: item.path.as_os_str().as_bytes().to_vec(),
        sha256,
        bytes: item.bytes,
        device: item.device,
        inode: item.inode,
    })
}

/// Equality observations of the SAME root-owned pair. These checks do not
/// establish funding: the separate actual Epoch borrower is mandatory at ARM.
pub(super) fn check_gnu(gnu: &[OwnedFd; 2], expected: &GnuIdentity) -> io::Result<()> {
    // The original paid native Epoch currently creates a named FIFO only.
    let GnuIdentity::NamedFifo { path, backing } = expected else {
        return Err(refused(
            "Bootstrap SDK queries require the original native FIFO epoch",
        ));
    };
    for (index, fd) in gnu.iter().enumerate() {
        let metadata = spawnfd::descriptor_metadata(fd.as_raw_fd())?;
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if !metadata.file_type().is_fifo()
            || metadata.uid() != aterm_uds::peer::our_uid()
            || metadata.dev() != backing.device
            || metadata.ino() != backing.inode
            || flags < 0
            || flags & libc::O_ACCMODE
                != if index == 0 {
                    libc::O_RDONLY
                } else {
                    libc::O_WRONLY
                }
            || flags & libc::O_NONBLOCK != 0
        {
            return Err(refused("Bootstrap original GNU description changed"));
        }
    }
    let named = std::fs::symlink_metadata(OsStr::from_bytes(path))?;
    if !named.file_type().is_fifo() || named.dev() != backing.device || named.ino() != backing.inode
    {
        return Err(refused("Bootstrap original GNU locator changed"));
    }
    Ok(())
}

impl Prepared {
    /// Called only with the original graph's selected Keeper, GNU pair and
    /// absolute queued deadline. No request extends any of those lifetimes.
    pub(super) fn new(
        request: wire::Request,
        sdk: Arc<sdk::Keeper>,
        gnu: Arc<[OwnedFd; 2]>,
        root_deadline: Instant,
        caller_deadline: Instant,
    ) -> io::Result<Self> {
        request.validate()?;
        let started = Instant::now();
        let deadline = started
            .checked_add(Duration::from_nanos(request.timeout_ns))
            .ok_or_else(|| refused("Bootstrap query clock bound is unrepresentable"))?
            .min(root_deadline)
            .min(caller_deadline);
        alive(deadline)?;
        if request.sdk_root != sdk.execution().as_os_str().as_bytes() {
            return Err(refused("Bootstrap query SDK differs from original Keeper"));
        }
        check_gnu(&gnu, &request.gnu)?;
        let held = sdk.bootstrap_query_program(request.tool, deadline)?;
        let observed = held.observation();
        let inventory = MetadataPrepared {
            root_attempt: request.key.graph.query.binding.root_attempt,
            member_attempt: request.key.operation,
            root: sdk.execution().as_os_str().as_bytes().to_vec(),
            policy: wire::PREPARED_POLICY.into(),
            scope: wire::PREPARED_SCOPE.into(),
            executable: observed_file(&observed.executable)?,
            runtime: observed
                .runtime
                .iter()
                .map(observed_file)
                .collect::<io::Result<_>>()?,
            loader_paths: observed
                .loader_paths
                .iter()
                .map(|p| p.as_os_str().as_bytes().to_vec())
                .collect(),
            system_dependencies: observed.system_dependencies.clone(),
            host: observed.host.clone(),
            boot: observed.boot.clone(),
        };
        request.validate_prepared(&inventory)?;
        let digest = aterm_digest::Sha256::digest(request.binding_preimage()?);
        let handoff = jobserver_utils::ScopedHandoff::jobserver(
            [gnu[0].try_clone()?, gnu[1].try_clone()?],
            [3, 4],
        )?;
        let (stdout, out) = Capture::pipe()?;
        let (stderr, err) = Capture::pipe()?;
        let mut target = spawnfd::command(held.program());
        target
            .env_clear()
            .envs(
                request
                    .environment
                    .iter()
                    .map(|(key, value)| (OsStr::from_bytes(key), OsStr::from_bytes(value))),
            )
            .envs(held.runtime_environment()?)
            .args(request.arguments.iter().map(|word| OsStr::from_bytes(word)))
            .current_dir(OsStr::from_bytes(&request.cwd));
        let (mut prelude, mut command) =
            crate::metadata_prelude::Owner::new(&target, &[3, 4, sdk::TARGET as u32], deadline)?;
        command
            .stdin(spawnfd::Stdio::from(File::open("/dev/null")?))
            .stdout(spawnfd::Stdio::from(out))
            .stderr(spawnfd::Stdio::from(err));
        command.inherit_guards_at(vec![
            (gnu[0].try_clone()?, 3),
            (gnu[1].try_clone()?, 4),
            (sdk.descriptor()?, sdk::TARGET),
            prelude.extra()?,
        ])?;
        for name in ["MAKEFLAGS", "MFLAGS", "CARGO_MAKEFLAGS"] {
            command.env(name, "-j --jobserver-fds=3,4 --jobserver-auth=3,4");
        }
        command.env(
            jobserver_utils::DESCENDANT_HANDOFF_ENV,
            handoff.advertisement(),
        );
        command.env("ATERM_SDK_LEASE_V3", sdk::TARGET.to_string());
        command.env("CARGO_BUILD_JOBS", "1");
        prelude.configured(&mut command)?;
        held.validate(deadline)?;
        sdk.validate()?;
        check_gnu(&gnu, &request.gnu)?;
        alive(deadline)?;
        Ok(Self {
            request,
            digest,
            deadline,
            sdk,
            held,
            inventory,
            command: Some(command),
            prelude,
            gnu,
            handoff,
            stdout,
            stderr,
        })
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        alive(self.deadline)?;
        self.sdk.validate()?;
        self.held.validate(self.deadline)?;
        check_gnu(&self.gnu, &self.request.gnu)?;
        alive(self.deadline)
    }

    /// Called only after the private Completed/ClosedFailed work closure.
    /// The SAME original inputs move into Root retention; no path is reopened,
    /// no saved inventory selects an owner, and no descriptor is exported.
    pub(super) fn into_retained(
        self,
        _closed: &super::execution::WorkClosed,
    ) -> (RetainedInputs, PreparedObservation) {
        let Self {
            request: _,
            digest,
            deadline,
            sdk,
            held,
            inventory,
            command,
            prelude,
            gnu,
            handoff,
            stdout,
            stderr,
        } = self;
        drop(command);
        drop(prelude);
        drop(handoff);
        let observation = PreparedObservation {
            inventory,
            request_sha256: digest,
            deadline,
            stdout: stdout.into_observation(),
            stderr: stderr.into_observation(),
        };
        (
            RetainedInputs {
                _sdk: sdk,
                _held: held,
                _gnu: gnu,
            },
            observation,
        )
    }
}
