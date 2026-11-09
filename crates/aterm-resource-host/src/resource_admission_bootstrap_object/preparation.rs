// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One Keeper and the actual accepted source/output descriptions. Input metadata
//! is an expectation only; none of these checks constructs a Manager or SDK.
use super::{alive, capture::Capture, output, refused, sdk, spawnfd, wire};
use crate::actor::owner::bootstrap_graph::OriginalOutput;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{File, Metadata},
    io,
    os::{
        fd::{AsFd, AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    },
    sync::Arc,
    time::Instant,
};
use trust_resource_controller::{MetadataFile, MetadataPrepared, bootstrap_graph::GnuIdentity};
use trust_resource_provenance::program::{FileObservation, HeldProgram};

#[derive(PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    bytes: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn of(m: &Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
            bytes: m.len(),
            mode: m.mode(),
            uid: m.uid(),
            gid: m.gid(),
            links: m.nlink(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
}
/// Actual readonly descriptions offered under the selected producer's unsafe
/// OriginalSourceRoot contract. They never become a new source-lock owner.
struct Source {
    descriptions: [OwnedFd; 2],
    stamps: Option<[Stamp; 2]>,
    parents: Vec<SourceParent>,
}
struct SourceParent {
    file: File,
    stamp: Option<Stamp>,
}
fn source_directory(parent: i32, name: &std::ffi::CStr) -> io::Result<File> {
    // SAFETY: fixed single component beneath an actual retained original;
    // no creation, symlink following, shared cursor or OFD-flag mutation.
    let raw = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(raw) })
    }
}
impl Source {
    fn capture_parents(&mut self) -> io::Result<()> {
        if !self.parents.is_empty() {
            return Err(refused("source parent originals already captured"));
        }
        let mut parent = self.descriptions[0].as_raw_fd();
        for name in [c"src", c"bootstrap"] {
            let file = source_directory(parent, name)?;
            self.parents.push(SourceParent { file, stamp: None });
            let original = self.parents.last_mut().expect("same source parent");
            original.stamp = Some(Stamp::of(&original.file.metadata()?));
            parent = original.file.as_raw_fd();
        }
        self.check_parents()
    }
    fn check_parents(&self) -> io::Result<()> {
        if self.parents.len() != 2 {
            return Err(refused(
                "complete original source parent population unavailable",
            ));
        }
        let mut parent = self.descriptions[0].as_raw_fd();
        for (original, name) in self.parents.iter().zip([c"src", c"bootstrap"]) {
            let expected = original
                .stamp
                .as_ref()
                .ok_or_else(|| refused("original source parent stamp unavailable"))?;
            if Stamp::of(&original.file.metadata()?) != *expected {
                return Err(refused("original source parent changed since capture"));
            }
            let named = source_directory(parent, name)?;
            if Stamp::of(&named.metadata()?) != *expected
                || Stamp::of(&original.file.metadata()?) != *expected
            {
                return Err(refused("original source parent name changed since capture"));
            }
            parent = original.file.as_raw_fd();
        }
        Ok(())
    }
    fn validate(&self, request: &wire::Request, deadline: Instant) -> io::Result<()> {
        self.check_parents()?;
        wire::transport::check_source(
            request,
            &self.descriptions.each_ref().map(AsFd::as_fd),
            deadline,
        )?;
        let named = std::fs::symlink_metadata(OsStr::from_bytes(&request.source_root))?;
        if !named.is_dir()
            || std::fs::canonicalize(OsStr::from_bytes(&request.source_root))?
                .as_os_str()
                .as_bytes()
                != request.source_root
            || (named.dev(), named.ino())
                != (
                    request.source_directory.device,
                    request.source_directory.inode,
                )
        {
            return Err(refused("object original source directory name changed"));
        }
        for (fd, stamp) in self.descriptions.iter().zip(
            self.stamps
                .as_ref()
                .ok_or_else(|| refused("object original source stamps unavailable"))?,
        ) {
            if Stamp::of(&spawnfd::descriptor_metadata(fd.as_raw_fd())?) != *stamp {
                return Err(refused(
                    "object original source changed since its actual capture",
                ));
            }
        }
        self.check_parents()?;
        alive(deadline)
    }
}

pub(super) struct RetainedInputs {
    _sdk: Arc<sdk::Keeper>,
    _held: Arc<HeldProgram>,
    _gnu: Arc<[OwnedFd; 2]>,
    _source: Option<Source>,
    _pending: Option<output::Pending>,
    _artifact: Option<output::Published>,
}
pub(in crate::actor::owner) struct PreparedObservation {
    pub(in crate::actor::owner) inventory: MetadataPrepared,
    pub(in crate::actor::owner) request_sha256: [u8; 32],
    pub(in crate::actor::owner) deadline: Instant,
    pub(in crate::actor::owner) stdout: super::capture::CaptureObservation,
    pub(in crate::actor::owner) stderr: super::capture::CaptureObservation,
    pub(in crate::actor::owner) artifact: Option<MetadataFile>,
    pub(in crate::actor::owner) retained_output_name: Option<Vec<u8>>,
}
pub(super) struct Prepared {
    pub(super) request: wire::Request,
    pub(super) digest: [u8; 32],
    pub(super) deadline: Instant,
    pub(super) sdk: Arc<sdk::Keeper>,
    pub(super) held: Arc<HeldProgram>,
    pub(super) inventory: wire::Prepared,
    pub(super) command: Option<spawnfd::Command>,
    pub(super) prelude: crate::metadata_prelude::Owner,
    control: Option<(OwnedFd, i32)>,
    pub(super) gnu: Arc<[OwnedFd; 2]>,
    pub(super) handoff: jobserver_utils::ScopedHandoff,
    pub(super) stdout: Capture,
    pub(super) stderr: Capture,
    source: Option<Source>,
    pub(super) pending: Option<output::Pending>,
    pub(super) artifact: Option<output::Published>,
}
pub(in crate::actor::owner) fn observed_file(item: &FileObservation) -> io::Result<MetadataFile> {
    let mut sha256 = [0; 32];
    if !aterm_codec::hex::decode_into(item.sha256.as_bytes(), &mut sha256) {
        return Err(refused("original object tool digest malformed"));
    }
    Ok(MetadataFile {
        path: item.path.as_os_str().as_bytes().to_vec(),
        sha256,
        bytes: item.bytes,
        device: item.device,
        inode: item.inode,
    })
}
fn check_gnu(gnu: &[OwnedFd; 2], expected: &GnuIdentity) -> io::Result<()> {
    let GnuIdentity::NamedFifo { path, backing } = expected else {
        return Err(refused("object needs original native FIFO epoch"));
    };
    for (index, fd) in gnu.iter().enumerate() {
        let m = spawnfd::descriptor_metadata(fd.as_raw_fd())?;
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        use std::os::unix::fs::FileTypeExt as _;
        if !m.file_type().is_fifo()
            || m.uid() != aterm_uds::peer::our_uid()
            || m.dev() != backing.device
            || m.ino() != backing.inode
            || flags < 0
            || flags & libc::O_ACCMODE
                != if index == 0 {
                    libc::O_RDONLY
                } else {
                    libc::O_WRONLY
                }
            || flags & libc::O_NONBLOCK != 0
        {
            return Err(refused("object original GNU descriptions changed"));
        }
    }
    use std::os::unix::fs::FileTypeExt as _;
    let m = std::fs::symlink_metadata(OsStr::from_bytes(path))?;
    if !m.file_type().is_fifo() || m.dev() != backing.device || m.ino() != backing.inode {
        return Err(refused("object original GNU name changed"));
    }
    Ok(())
}
impl Prepared {
    pub(super) fn new(
        request: wire::Request,
        sdk: Arc<sdk::Keeper>,
        gnu: Arc<[OwnedFd; 2]>,
        output: OriginalOutput,
        environment: &BTreeMap<Vec<u8>, Vec<u8>>,
        deadline: Instant,
    ) -> io::Result<Self> {
        request.validate()?;
        alive(deadline)?;
        // Only this measured native object route has an existing qualified image.
        if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Bootstrap native object target unavailable",
            ));
        }
        if request.sdk_root != sdk.execution().as_os_str().as_bytes() {
            return Err(refused("object SDK differs from original Keeper"));
        }
        check_gnu(&gnu, &request.gnu)?;
        let held = sdk.bootstrap_object_program(deadline)?;
        let compiler = sdk.bootstrap_query_program(
            trust_resource_controller::bootstrap_graph::query::Tool::Compiler,
            deadline,
        )?;
        if observed_file(&held.observation().executable)? != request.driver
            || observed_file(&compiler.observation().executable)? != request.compiler
        {
            return Err(refused(
                "object tools differ from original captured Keeper inputs",
            ));
        }
        let pending = output::Pending::new(
            output,
            request.key.operation,
            &request.destination,
            deadline,
        )?;
        let observed = held.observation();
        let inventory = wire::Prepared {
            inventory: MetadataPrepared {
                root_attempt: request.key.graph.query.binding.root_attempt,
                member_attempt: request.key.operation,
                root: sdk.execution().as_os_str().as_bytes().to_vec(),
                policy: wire::PREPARED_POLICY.into(),
                scope: wire::PREPARED_SCOPE.into(),
                executable: observed_file(&observed.executable)?,
                runtime: {
                    let mut files = BTreeMap::new();
                    for item in observed
                        .runtime
                        .iter()
                        .chain(std::iter::once(&compiler.observation().executable))
                        .chain(&compiler.observation().runtime)
                    {
                        let actual = observed_file(item)?;
                        if let Some(previous) = files.insert(actual.path.clone(), actual.clone())
                            && previous != actual
                        {
                            return Err(refused("original object input population conflicts"));
                        }
                    }
                    files.into_values().collect()
                },
                loader_paths: observed
                    .loader_paths
                    .iter()
                    .map(|p| p.as_os_str().as_bytes().to_vec())
                    .collect(),
                system_dependencies: observed.system_dependencies.clone(),
                host: observed.host.clone(),
                boot: observed.boot.clone(),
            },
            staging: pending.staging().as_bytes().to_vec(),
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
                environment
                    .iter()
                    .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
            )
            .envs(held.runtime_environment()?)
            .args(
                request
                    .arguments(&inventory.staging)?
                    .iter()
                    .map(|arg| OsStr::from_bytes(arg)),
            )
            .current_dir(OsStr::from_bytes(&request.source_root));
        let (mut prelude, mut command) = crate::metadata_prelude::Owner::new(
            &target,
            &[3, 4, sdk::TARGET as u32, 6, 7, 8, 9],
            deadline,
        )?;
        command
            .stdin(spawnfd::Stdio::from(File::open("/dev/null")?))
            .stdout(spawnfd::Stdio::from(out))
            .stderr(spawnfd::Stdio::from(err));
        let control = Some(prelude.extra()?);
        for name in ["MAKEFLAGS", "MFLAGS", "CARGO_MAKEFLAGS"] {
            command.env(name, "-j --jobserver-fds=3,4 --jobserver-auth=3,4");
        }
        command.env(
            jobserver_utils::DESCENDANT_HANDOFF_ENV,
            handoff.advertisement(),
        );
        command.env("ATERM_SDK_LEASE_V3", sdk::TARGET.to_string());
        command.env("CARGO_BUILD_JOBS", "1");
        let this = Self {
            request,
            digest,
            deadline,
            sdk,
            held,
            inventory,
            command: Some(command),
            prelude,
            control,
            gnu,
            handoff,
            stdout,
            stderr,
            source: None,
            pending: Some(pending),
            artifact: None,
        };
        this.validate_selected()?;
        Ok(this)
    }
    fn validate_selected(&self) -> io::Result<()> {
        alive(self.deadline)?;
        self.sdk.validate()?;
        self.held.validate(self.deadline)?;
        check_gnu(&self.gnu, &self.request.gnu)?;
        if let Some(pending) = &self.pending {
            pending.validate()?;
        }
        if let Some(artifact) = &self.artifact {
            artifact.validate(self.deadline)?;
        }
        alive(self.deadline)
    }
    pub(super) fn attach_source(&mut self, descriptions: [OwnedFd; 2]) -> io::Result<()> {
        if self.source.is_some() {
            return Err(refused("object source already attached"));
        }
        // Store actual originals BEFORE any fallible validation. Failed attach
        // keeps the entire supplied source population in the same owner.
        self.source = Some(Source {
            descriptions,
            stamps: None,
            parents: Vec::new(),
        });
        let source = self.source.as_mut().expect("same attached actual source");
        let [a, b] = source
            .descriptions
            .each_ref()
            .map(|fd| spawnfd::descriptor_metadata(fd.as_raw_fd()).map(|m| Stamp::of(&m)));
        source.stamps = Some([a?, b?]);
        source.capture_parents()?;
        self.validate()?;
        let source = self.source.as_ref().expect("same attached actual source");
        let [out_dir, out_lock] = self
            .pending
            .as_ref()
            .ok_or_else(|| refused("object output owner unavailable"))?
            .inherited()?;
        let control = self
            .control
            .take()
            .ok_or_else(|| refused("object prelude control already configured"))?;
        let command = self
            .command
            .as_mut()
            .ok_or_else(|| refused("object launch already attempted"))?;
        // These are close-only duplicates of actual originals. They survive
        // Host/Root owner death through BOTH native execs; no lock is reopened,
        // acquired, unlocked, or reconstructed from the request's identities.
        command.inherit_guards_at(vec![
            (self.gnu[0].try_clone()?, 3),
            (self.gnu[1].try_clone()?, 4),
            (self.sdk.descriptor()?, sdk::TARGET),
            (source.descriptions[0].try_clone()?, 6),
            (source.descriptions[1].try_clone()?, 7),
            (out_dir, 8),
            (out_lock, 9),
            control,
        ])?;
        self.prelude.configured(command)?;
        self.validate()
    }
    pub(super) fn check_unlaunched(&self) -> io::Result<()> {
        self.validate()?;
        self.pending
            .as_ref()
            .ok_or_else(|| refused("object staging owner missing before GO"))?
            .check_unlaunched()
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        self.validate_selected()?;
        self.source
            .as_ref()
            .ok_or_else(|| refused("object has no actual source descriptions"))?
            .validate(&self.request, self.deadline)
    }
    pub(super) fn into_retained(
        self,
        _closed: &super::execution::WorkClosed,
    ) -> (RetainedInputs, PreparedObservation) {
        let Self {
            sdk,
            held,
            gnu,
            source,
            pending,
            artifact,
            inventory,
            digest,
            deadline,
            stdout,
            stderr,
            command,
            prelude,
            handoff,
            control,
            ..
        } = self;
        let observation = PreparedObservation {
            inventory: inventory.inventory,
            request_sha256: digest,
            deadline,
            stdout: stdout.into_observation(),
            stderr: stderr.into_observation(),
            artifact: artifact.as_ref().map(|a| a.observation().clone()),
            retained_output_name: pending.as_ref().map(|p| p.retained_name().to_vec()),
        };
        drop(command);
        drop(prelude);
        drop(handoff);
        drop(control);
        (
            RetainedInputs {
                _sdk: sdk,
                _held: held,
                _gnu: gnu,
                _source: source,
                _pending: pending,
                _artifact: artifact,
            },
            observation,
        )
    }
}

#[cfg(test)]
#[path = "preparation_tests.rs"]
mod tests;
