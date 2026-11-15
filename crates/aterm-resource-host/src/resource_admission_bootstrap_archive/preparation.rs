// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual retained inputs are supplied by the original operation, never opened
//! from report DATA. The request record is bounded DATA, not input authority.
use super::{Capture, alive, refused, sdk, spawnfd, transport::Socket, wire};
use crate::actor::owner::bootstrap_graph::ArchiveOperation;
use std::{
    fs::File,
    io::{self, Seek as _, Write as _},
    os::{
        fd::{AsRawFd as _, OwnedFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{FileExt as _, MetadataExt as _},
        },
    },
    sync::Arc,
    time::Instant,
};
use trust_resource_controller::{MetadataFile, MetadataPrepared, bootstrap_graph::GnuIdentity};
use trust_resource_provenance::program::{FileObservation, HeldProgram};

pub(in crate::actor::owner) fn observed_program(
    file: &FileObservation,
) -> io::Result<MetadataFile> {
    let mut sha256 = [0; 32];
    if !aterm_codec::hex::decode_into(file.sha256.as_bytes(), &mut sha256) {
        return Err(refused("archive original program digest malformed"));
    }
    Ok(MetadataFile {
        path: file.path.as_os_str().as_bytes().to_vec(),
        sha256,
        bytes: file.bytes,
        device: file.device,
        inode: file.inode,
    })
}
pub(super) fn check_gnu(gnu: &[OwnedFd; 2], expected: &GnuIdentity) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt as _;
    let GnuIdentity::NamedFifo { path, backing } = expected else {
        return Err(refused("archive requires actual original FIFO epoch"));
    };
    for (index, fd) in gnu.iter().enumerate() {
        let m = spawnfd::descriptor_metadata(fd.as_raw_fd())?;
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if !m.file_type().is_fifo()
            || m.uid() != aterm_uds::peer::our_uid()
            || (m.dev(), m.ino()) != (backing.device, backing.inode)
            || flags < 0
            || flags & libc::O_ACCMODE
                != if index == 0 {
                    libc::O_RDONLY
                } else {
                    libc::O_WRONLY
                }
            || flags & libc::O_NONBLOCK != 0
        {
            return Err(refused("archive original GNU description changed"));
        }
    }
    let m = std::fs::symlink_metadata(std::ffi::OsStr::from_bytes(path))?;
    if !m.file_type().is_fifo() || (m.dev(), m.ino()) != (backing.device, backing.inode) {
        return Err(refused("archive original GNU name changed"));
    }
    Ok(())
}
pub(super) struct Prepared {
    pub(super) held: Arc<HeldProgram>,
    pub(super) inventory: MetadataPrepared,
    pub(super) digest: [u8; 32],
    pub(super) command: Option<spawnfd::Command>,
    pub(super) prelude: crate::metadata_prelude::Owner,
    pub(super) handoff: jobserver_utils::ScopedHandoff,
    pub(super) stdout: Capture,
    pub(super) stderr: Capture,
    pub(super) control: Socket,
    pub(super) run_sent: bool,
    pub(super) control_eof: bool,
    request_record: File,
    request_bytes: Vec<u8>,
}
impl Prepared {
    pub(super) fn new(operation: &ArchiveOperation) -> io::Result<Self> {
        let request = &operation.request;
        let deadline = operation.deadline;
        request.validate()?;
        alive(deadline)?;
        if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native archive target unavailable",
            ));
        }
        operation.inputs.check_current(deadline)?;
        check_gnu(&operation.gnu, &request.gnu)?;
        let held = operation.sdk.bootstrap_archive_program(deadline)?;
        let observed = held.observation();
        let inventory = MetadataPrepared {
            root_attempt: request.key.graph.query.binding.root_attempt,
            member_attempt: request.key.operation,
            root: operation.sdk.execution().as_os_str().as_bytes().to_vec(),
            policy: wire::PREPARED_POLICY.into(),
            scope: wire::PREPARED_SCOPE.into(),
            executable: observed_program(&observed.executable)?,
            runtime: observed
                .runtime
                .iter()
                .map(observed_program)
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
        let request_bytes = request.encode()?;
        let digest = aterm_digest::Sha256::digest(request.binding_preimage()?);
        let mut request_record = aterm_tempfile::tempfile()?;
        request_record.write_all(&request_bytes)?;
        request_record.rewind()?;
        let handoff = jobserver_utils::ScopedHandoff::jobserver(
            [operation.gnu[0].try_clone()?, operation.gnu[1].try_clone()?],
            [3, 4],
        )?;
        let (stdout, out) = Capture::pipe()?;
        let (stderr, err) = Capture::pipe()?;
        let (parent, child) = aterm_uds::kept_out(aterm_uds::CtlStream::pair)?;
        child.set_nonblocking(true)?;
        let control = Socket::new(parent)?;
        let mut target = spawnfd::command(held.program());
        target
            .env_clear()
            .envs(held.runtime_environment()?)
            .args(request.arguments()?)
            .current_dir(std::ffi::OsStr::from_bytes(&request.source_root));
        let (mut prelude, mut command) =
            crate::metadata_prelude::Owner::new(&target, &[3, 4, 5, 6, 7, 8], deadline)?;
        command
            .stdin(spawnfd::Stdio::from(request_record.try_clone()?))
            .stdout(spawnfd::Stdio::from(out))
            .stderr(spawnfd::Stdio::from(err));
        for name in ["MAKEFLAGS", "MFLAGS", "CARGO_MAKEFLAGS"] {
            command.env(name, "-j --jobserver-fds=3,4 --jobserver-auth=3,4");
        }
        command
            .env(
                jobserver_utils::DESCENDANT_HANDOFF_ENV,
                handoff.advertisement(),
            )
            .env("ATERM_SDK_LEASE_V3", "5")
            .env("CARGO_BUILD_JOBS", "1");
        let extra = prelude.extra()?;
        // Retained originals supply both directory descriptions. The callback
        // runs all source/config/cache bookends even on duplication failure.
        operation
            .inputs
            .with_original_directories(deadline, |source, contents| {
                command.inherit_guards_at(vec![
                    (operation.gnu[0].try_clone()?, 3),
                    (operation.gnu[1].try_clone()?, 4),
                    (operation.sdk.descriptor()?, sdk::TARGET),
                    (aterm_uds::kept_out(|| source.try_clone_to_owned())?, 6),
                    (aterm_uds::kept_out(|| contents.try_clone_to_owned())?, 7),
                    (child.into(), 8),
                    extra,
                ])
            })?;
        prelude.configured(&mut command)?;
        let this = Self {
            held,
            inventory,
            digest,
            command: Some(command),
            prelude,
            handoff,
            stdout,
            stderr,
            control,
            run_sent: false,
            control_eof: false,
            request_record,
            request_bytes,
        };
        this.validate(operation)?;
        Ok(this)
    }
    pub(super) fn validate(&self, operation: &ArchiveOperation) -> io::Result<()> {
        alive(operation.deadline)?;
        operation.sdk.validate()?;
        self.held.validate(operation.deadline)?;
        operation.inputs.check_current(operation.deadline)?;
        check_gnu(&operation.gnu, &operation.request.gnu)?;
        let m = self.request_record.metadata()?;
        if !m.is_file() || m.mode() & 0o077 != 0 || m.len() != self.request_bytes.len() as u64 {
            return Err(refused("archive original request record changed"));
        }
        let mut actual = vec![0; self.request_bytes.len()];
        self.request_record.read_exact_at(&mut actual, 0)?;
        if actual != self.request_bytes {
            return Err(refused("archive original request bytes changed"));
        }
        alive(operation.deadline)
    }
    /// One Run, only after the actual retained child captured its earlier
    /// anchor. No source contents are read by this host handshake.
    pub(super) fn poll_control(
        &mut self,
        operation: &ArchiveOperation,
        exited: bool,
    ) -> io::Result<()> {
        use super::transport::Peek;
        if !self.run_sent {
            if let Some(frame) = self.control.receive(operation.deadline)? {
                wire::startup::validate_awaiting(&operation.request, self.digest, &frame)?;
                self.validate(operation)?;
                let ns = operation
                    .deadline
                    .checked_duration_since(Instant::now())
                    .filter(|d| !d.is_zero())
                    .ok_or(io::ErrorKind::TimedOut)?
                    .as_nanos();
                let ns = u64::try_from(ns)
                    .map_err(|_| refused("archive remaining time overflow"))?
                    .min(operation.request.timeout_ns);
                self.control
                    .push(wire::startup::run(&operation.request, self.digest, ns)?)?;
                self.run_sent = true;
            } else {
                return Ok(());
            }
        }
        self.control.flush(operation.deadline)?;
        match self.control.peek()? {
            Peek::Bytes => Err(refused("archive child repeated or trailed its AwaitWork")),
            Peek::Eof if !exited => Err(refused("archive child lost control before exit")),
            Peek::Eof => {
                self.control_eof = true;
                Ok(())
            }
            Peek::Waiting => Ok(()),
        }
    }
}
