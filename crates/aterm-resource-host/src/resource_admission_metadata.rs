// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original fixed-one metadata custody and sealed, held input preparation.
use super::*;
use crate::subgroup_delegation::Prepared;
#[cfg(target_os = "macos")]
use std::os::fd::{AsFd, AsRawFd};
use std::{
    ffi::OsStr,
    os::{fd::OwnedFd, unix::ffi::OsStrExt},
};
use trust_resource_controller::{MetadataFile, MetadataPrepared, SealedMetadataRequest};
use trust_resource_provenance::program::HeldProgram;

#[path = "resource_admission_metadata/linker_lifeline.rs"]
mod linker_lifeline;

/// Keep the separately negotiated purpose through paid preparation. Sharing
/// native input custody never converts a resolver query into metadata role 3.
#[derive(Clone)]
pub(super) enum Purpose {
    Metadata(SealedMetadataRequest),
    Resolver(trust_resource_controller::resolver::SealedResolverQueryRequest),
    WorkloadQuery(trust_resource_controller::workload::host::CompilerQuery),
}
impl Purpose {
    pub(super) fn authorize(&self, check: &mut members::Authorization) {
        check.metadata = matches!(self, Self::Metadata(_));
        check.resolver = matches!(self, Self::Resolver(_));
        check.workload_query = match self {
            Self::WorkloadQuery(query) => Some(query.clone()),
            _ => None,
        };
    }
    pub(super) fn member(&self) -> &trust_resource_controller::Request {
        match self {
            Self::Metadata(request) => &request.member,
            Self::Resolver(request) => &request.member,
            Self::WorkloadQuery(request) => &request.member,
        }
    }
    fn root(&self) -> &[u8] {
        match self {
            Self::Metadata(request) => &request.root,
            Self::Resolver(request) => &request.root,
            Self::WorkloadQuery(request) => &request.root,
        }
    }
    fn digest(&self) -> &[u8; 32] {
        match self {
            Self::Metadata(request) => &request.program_sha256,
            Self::Resolver(request) => &request.program_sha256,
            Self::WorkloadQuery(request) => &request.program_sha256,
        }
    }
    fn validate(&self) -> io::Result<()> {
        match self {
            Self::Metadata(request) => request.validate(),
            Self::Resolver(request) => request.validate(),
            Self::WorkloadQuery(request) => request.validate(),
        }
    }
    fn validate_prepared(&self, prepared: &MetadataPrepared) -> io::Result<()> {
        match self {
            Self::Metadata(request) => prepared.validate_for(request),
            Self::Resolver(request) => request.validate_prepared(prepared),
            Self::WorkloadQuery(request) => request.validate_prepared(prepared),
        }
    }
}

/// This zero-byte pipe is the original GNU server, not a numeric capacity
/// assertion. The original fixed CPU already pays for its one implicit unit.
#[cfg(target_os = "macos")]
pub(super) struct RootGnu {
    pipe: [OwnedFd; 2],
    handoff: jobserver_utils::ScopedHandoff,
}
#[cfg(target_os = "macos")]
impl RootGnu {
    pub(super) fn new() -> io::Result<Self> {
        let (read, write) = aterm_uds::kept_out(io::pipe)?;
        let pipe = [OwnedFd::from(read), OwnedFd::from(write)];
        let handoff = jobserver_utils::ScopedHandoff::jobserver(
            [pipe[0].try_clone()?, pipe[1].try_clone()?],
            [3, 4],
        )?;
        Ok(Self { pipe, handoff })
    }
    pub(super) fn configure(
        &mut self,
        command: &mut spawnfd::Command,
        mut extra: Vec<(OwnedFd, i32)>,
    ) -> io::Result<()> {
        if command.guard_targets().len() != 0 {
            return Err(refused("metadata GNU would replace inherited custody"));
        }
        extra.extend([
            (self.pipe[0].try_clone()?, 3),
            (self.pipe[1].try_clone()?, 4),
        ]);
        command.inherit_guards_at(extra)?;
        for key in ["MAKEFLAGS", "MFLAGS", "CARGO_MAKEFLAGS"] {
            command.env(key, "-j --jobserver-fds=3,4 --jobserver-auth=3,4");
        }
        command.env(
            jobserver_utils::DESCENDANT_HANDOFF_ENV,
            self.handoff.advertisement(),
        );
        Ok(())
    }
    pub(super) fn bind_child(&mut self, pid: u32) -> io::Result<()> {
        self.handoff.bind_child(pid)
    }
    pub(super) fn matches(&self, incoming: &[OwnedFd]) -> io::Result<bool> {
        if incoming.len() != 2 {
            return Ok(false);
        }
        let expected = [pipe(self.pipe[0].as_fd())?, pipe(self.pipe[1].as_fd())?];
        let observed = [pipe(incoming[0].as_fd())?, pipe(incoming[1].as_fd())?];
        Ok(expected == observed
            && expected[0].0 != 0
            && expected[0].1 == expected[1].0
            && expected[1].1 == expected[0].0
            && expected[0].2 == 1
            && expected[1].2 == 2)
    }
}
#[cfg(target_os = "macos")]
fn pipe(fd: std::os::fd::BorrowedFd<'_>) -> io::Result<(u64, u64, u32)> {
    use aterm_sysprobe::jobs::darwin;
    let mut bytes = [0_u8; darwin::PIPE_SIZE];
    // SAFETY: read-only native query of our original owned descriptor into a
    // correctly bounded buffer; it confers no process control authority.
    let got = unsafe {
        libc::proc_pidfdinfo(
            std::process::id() as i32,
            fd.as_raw_fd(),
            darwin::PROC_PIDFDPIPEINFO,
            bytes.as_mut_ptr().cast(),
            bytes.len() as i32,
        )
    };
    if got != bytes.len() as i32 {
        return Err(io::Error::last_os_error());
    }
    let observed = darwin::pipe(&bytes).ok_or_else(|| refused("GNU pipe identity unavailable"))?;
    Ok((observed.handle, observed.peer, observed.openflags & 3))
}
#[cfg(not(target_os = "macos"))]
pub(super) struct RootGnu;
#[cfg(not(target_os = "macos"))]
impl RootGnu {
    pub(super) fn new() -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn configure(
        &mut self,
        _: &mut spawnfd::Command,
        _: Vec<(OwnedFd, i32)>,
    ) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn bind_child(&mut self, _: u32) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn matches(&self, _: &[OwnedFd]) -> io::Result<bool> {
        Ok(false)
    }
}

pub(super) struct Inputs {
    held: std::sync::Arc<HeldProgram>,
    pub(super) inventory: Option<MetadataPrepared>,
    prelude: crate::metadata_prelude::Owner,
    host_store: bool,
    linker: Option<trust_resource_controller::linker::Request>,
    linker_lifeline: Option<linker_lifeline::Lifeline>,
    linker_sdk: Option<OwnedFd>,
}
pub(super) struct Command {
    pub(super) command: spawnfd::Command,
    pub(super) inputs: Inputs,
}
pub(super) enum Preparation {
    Metadata {
        purpose: Purpose,
        held: Option<std::sync::Arc<HeldProgram>>,
    },
    Native {
        command: spawnfd::Command,
        held: std::sync::Arc<HeldProgram>,
        targets: Vec<u32>,
    },
    Linker {
        request: trust_resource_controller::linker::Request,
        held: std::sync::Arc<HeldProgram>,
        sdk: OwnedFd,
    },
}
impl Preparation {
    pub(super) fn run(self, deadline: Instant) -> io::Result<Command> {
        match self {
            Self::Metadata {
                purpose,
                held: Some(held),
            } => prepare_with_held(purpose, held, deadline),
            Self::Metadata {
                purpose,
                held: None,
            } => prepare(purpose, deadline),
            Self::Native {
                command,
                held,
                targets,
            } => prepare_native(command, held, targets, deadline),
            Self::Linker { request, held, sdk } => prepare_linker(request, held, sdk, deadline),
        }
    }
}
pub(super) fn prepare(purpose: Purpose, deadline: Instant) -> io::Result<Command> {
    purpose.validate()?;
    let request = purpose.member();
    let program = PathBuf::from(OsStr::from_bytes(&request.command.program));
    let root = PathBuf::from(OsStr::from_bytes(purpose.root()));
    let held = HeldProgram::open(&root, &program, purpose.digest(), deadline)?;
    prepare_with_held(purpose, std::sync::Arc::new(held), deadline)
}
pub(super) fn prepare_with_held(
    purpose: Purpose,
    held: std::sync::Arc<HeldProgram>,
    deadline: Instant,
) -> io::Result<Command> {
    purpose.validate()?;
    let request = purpose.member();
    if held.program().as_os_str().as_bytes() != request.command.program
        || held.root().as_os_str().as_bytes() != purpose.root()
    {
        return Err(refused(
            "metadata request differs from its original held SDK inputs",
        ));
    }
    let mut digest = [0; 32];
    if !aterm_codec::hex::decode_into(held.observation().executable.sha256.as_bytes(), &mut digest)
        || &digest != purpose.digest()
    {
        return Err(refused(
            "metadata digest differs from its original held SDK inputs",
        ));
    }
    held.validate(deadline)?;
    let program = held.program();
    let observed = held.observation();
    let file =
        |item: &trust_resource_provenance::program::FileObservation| -> io::Result<MetadataFile> {
            let mut sha256 = [0; 32];
            if !aterm_codec::hex::decode_into(item.sha256.as_bytes(), &mut sha256) {
                return Err(refused("invalid held digest"));
            }
            Ok(MetadataFile {
                path: item.path.as_os_str().as_bytes().to_vec(),
                sha256,
                bytes: item.bytes,
                device: item.device,
                inode: item.inode,
            })
        };
    let inventory = MetadataPrepared {
        root_attempt: request.binding.root_attempt,
        member_attempt: request.member_attempt,
        root: purpose.root().to_vec(),
        policy: observed.policy.into(),
        scope: observed.scope.into(),
        executable: file(&observed.executable)?,
        runtime: observed
            .runtime
            .iter()
            .map(file)
            .collect::<io::Result<_>>()?,
        loader_paths: observed
            .loader_paths
            .iter()
            .map(|path| path.as_os_str().as_bytes().to_vec())
            .collect(),
        system_dependencies: observed.system_dependencies.clone(),
        host: observed.host.clone(),
        boot: observed.boot.clone(),
    };
    purpose.validate_prepared(&inventory)?;
    let mut command = spawnfd::command(program);
    command
        .env_clear()
        .envs(
            request
                .command
                .environment
                .iter()
                .map(|(key, value)| (OsStr::from_bytes(key), OsStr::from_bytes(value))),
        )
        .envs(held.runtime_environment()?)
        .args(
            request
                .command
                .args
                .iter()
                .map(|word| OsStr::from_bytes(word)),
        )
        .current_dir(OsStr::from_bytes(&request.command.cwd))
        .start_suspended(true);
    let targets = request
        .delegations
        .cpu
        .iter()
        .map(|fd| fd.target)
        .chain(match request.delegations.gnu {
            trust_resource_controller::GnuTransport::NumericPipe { read, write } => {
                vec![read.target, write.target]
            }
            trust_resource_controller::GnuTransport::None
                if matches!(purpose, Purpose::WorkloadQuery(_)) =>
            {
                Vec::new()
            }
            _ => return Err(refused("metadata requires its original numeric GNU pair")),
        })
        .collect::<Vec<_>>();
    let (prelude, command) = crate::metadata_prelude::Owner::new(&command, &targets, deadline)?;
    Ok(Command {
        command,
        inputs: Inputs {
            held,
            inventory: Some(inventory),
            prelude,
            host_store: false,
            linker: None,
            linker_lifeline: None,
            linker_sdk: None,
        },
    })
}
/// Original compiler input custody. The marker was checked on this SAME held
/// image in paid root preparation; current validation reuses that fact. This
/// emits no metadata Prepared frame on the unchanged compiler/linker protocol.
pub(super) fn prepare_native(
    mut command: spawnfd::Command,
    held: std::sync::Arc<HeldProgram>,
    mut targets: Vec<u32>,
    deadline: Instant,
) -> io::Result<Command> {
    if command.get_program() != held.program().as_os_str() {
        return Err(refused("native command differs from original SDK image"));
    }
    held.validate(deadline)?;
    command.envs(held.runtime_environment()?);
    targets.extend([sdk::TARGET as u32, 7]);
    let (prelude, command) = crate::metadata_prelude::Owner::new(&command, &targets, deadline)?;
    Ok(Command {
        command,
        inputs: Inputs {
            held,
            inventory: None,
            prelude,
            host_store: true,
            linker: None,
            linker_lifeline: None,
            linker_sdk: None,
        },
    })
}
fn prepare_linker(
    request: trust_resource_controller::linker::Request,
    held: std::sync::Arc<HeldProgram>,
    sdk: OwnedFd,
    deadline: Instant,
) -> io::Result<Command> {
    request.validate()?;
    if request.root != held.root().as_os_str().as_bytes()
        || request.member.command.program != held.program().as_os_str().as_bytes()
        || request.member.command.environment.iter().any(|(key, _)| {
            trust_resource_controller::workload::host::produced::protected_environment_name(key)
                || trust_resource_controller::workload::host::produced::loader_override_name(key)
        })
    {
        return Err(refused(
            "linker inputs or environment differ from nondelegating SDK profile",
        ));
    }
    held.validate(deadline)?;
    // The caller cannot choose or duplicate the Host-only writer. Reject every
    // reserved spelling before injecting our own single final-exec reader.
    if request
        .member
        .command
        .args
        .iter()
        .any(|arg| arg.starts_with(b"--resource-lifeline"))
    {
        return Err(refused("caller-supplied linker lifeline is not authorized"));
    }
    let lifeline = linker_lifeline::Lifeline::new()?;
    let mut arguments = request.member.command.args.clone();
    // Keep the recipe's final three worker caps as actual final options.
    arguments.insert(
        arguments.len() - 3,
        linker_lifeline::ARGUMENT.as_bytes().to_vec(),
    );
    let mut command = spawnfd::command(held.program());
    command
        .env_clear()
        .envs(
            request
                .member
                .command
                .environment
                .iter()
                .map(|(key, value)| (OsStr::from_bytes(key), OsStr::from_bytes(value))),
        )
        .envs(held.runtime_environment()?)
        .args(arguments.iter().map(|arg| OsStr::from_bytes(arg)))
        .current_dir(OsStr::from_bytes(&request.member.command.cwd));
    let cpu = request
        .member
        .delegations
        .cpu
        .ok_or(io::ErrorKind::InvalidData)?
        .target;
    let (prelude, command) = crate::metadata_prelude::Owner::new(
        &command,
        &[cpu, linker_lifeline::TARGET, linker_lifeline::SDK_TARGET],
        deadline,
    )?;
    Ok(Command {
        command,
        inputs: Inputs {
            held,
            inventory: None,
            prelude,
            host_store: false,
            linker: Some(request),
            linker_lifeline: Some(lifeline),
            linker_sdk: Some(sdk),
        },
    })
}
pub(super) struct Resources {
    pub(super) scheduling: Prepared,
    pub(super) inputs: Option<Inputs>,
    // These original owners also survive OwnedAttempt's conservative retention
    // of Resources when actual native retirement cannot be established.
    pub(super) _sdk: Option<std::sync::Arc<sdk::Keeper>>,
    pub(super) _compiler: Option<compiler::Selection>,
    pub(super) _primary: Option<std::sync::Arc<workload::Primary>>,
    pub(super) _output: Option<std::sync::Arc<workload::OutputLocks>>,
}
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
impl Resources {
    pub(super) fn close_linker_lifeline(&mut self) -> bool {
        let Some(lifeline) = self
            .inputs
            .as_mut()
            .and_then(|inputs| inputs.linker_lifeline.as_mut())
        else {
            return false;
        };
        lifeline.close();
        true
    }
}
impl member::MemberResources for Resources {
    fn take_producer_inputs(&mut self) -> io::Result<Option<member_custody::ProducerInputs>> {
        let Some(inputs) = &mut self.inputs else {
            return Ok(None);
        };
        let gnu = self.scheduling.retirement_gnu()?;
        let target = inputs
            .prelude
            .take_retirement()
            .ok_or_else(|| refused("native target retirement witness missing"))?;
        Ok(Some(member_custody::ProducerInputs::new(target, gnu)?))
    }
    fn configure(&mut self, command: &mut spawnfd::Command) -> io::Result<()> {
        if let Some(inputs) = &mut self.inputs {
            let mut extra = vec![inputs.prelude.extra()?];
            if inputs.host_store {
                extra.push((crate::runtime::workload_store_description()?, 7));
            }
            if inputs.linker.is_some() {
                extra.push(
                    inputs
                        .linker_lifeline
                        .as_mut()
                        .ok_or_else(|| refused("linker host lifeline missing"))?
                        .extra()?,
                );
                // Close-only SAME original Host lease, retained by the payload
                // after Host/parent loss. No SDK locator or grant is exported.
                extra.push((
                    inputs
                        .linker_sdk
                        .take()
                        .ok_or_else(|| refused("linker original SDK guard missing"))?,
                    linker_lifeline::SDK_TARGET as i32,
                ));
                self.scheduling.configure_linker(command, extra)?;
            } else {
                self.scheduling.configure_with_extra(command, extra)?;
            }
            if inputs.host_store {
                command.env(
                    trust_resource_controller::workload::host::HOST_STORE_FD_ENV,
                    "7",
                );
            }
            inputs.prelude.configured(command)
        } else {
            self.scheduling.configure(command)
        }
    }
    fn prepare_target(&mut self, child: &mut spawnfd::Child) -> io::Result<bool> {
        if let Some(inputs) = &mut self.inputs {
            let held = &inputs.held;
            inputs.prelude.poll(child, |pid| {
                native_inputs::validate_loaded(
                    held,
                    trust_resource_provenance::process::read_identity(pid)?,
                    Instant::now(),
                )
            })
        } else {
            Ok(true)
        }
    }
    fn bind_child(&mut self, pid: u32) -> io::Result<()> {
        self.scheduling.bind_child(pid)?;
        if let Some(inputs) = &self.inputs {
            let child = trust_resource_provenance::process::read_identity(pid)?;
            native_inputs::validate_loaded(&inputs.held, child, Instant::now())?;
        }
        Ok(())
    }
    fn publish_prepared(
        &mut self,
        (pid, birth): (u32, u64),
        events: &mut job::Events,
    ) -> io::Result<()> {
        let Some(inputs) = &self.inputs else {
            return Ok(());
        };
        let Some(request) = &inputs.linker else {
            return Ok(());
        };
        inputs
            .held
            .validate(native_inputs::deadline(Instant::now()))?;
        let observed = inputs.held.observation();
        let file=|file:&trust_resource_provenance::program::FileObservation|->io::Result<MetadataFile>{
            let mut sha256=[0;32];
            if !aterm_codec::hex::decode_into(file.sha256.as_bytes(),&mut sha256){return Err(refused("linker input digest malformed"));}
            Ok(MetadataFile{path:file.path.as_os_str().as_bytes().to_vec(),sha256,bytes:file.bytes,device:file.device,inode:file.inode})
        };
        let prepared = trust_resource_controller::linker::Prepared {
            root_attempt: request.member.binding.root_attempt,
            member_attempt: request.member.member_attempt,
            loan: request.loan()?,
            identity: trust_resource_controller::Identity {
                pid,
                birth,
                birth_kind: if cfg!(target_os = "macos") {
                    trust_resource_controller::BirthKind::MacosWallMicroseconds
                } else {
                    trust_resource_controller::BirthKind::LinuxStartTicks
                },
            },
            root: request.root.clone(),
            policy: observed.policy.into(),
            scope: observed.scope.into(),
            executable: file(&observed.executable)?,
            runtime: observed
                .runtime
                .iter()
                .map(file)
                .collect::<io::Result<_>>()?,
            loader_paths: observed
                .loader_paths
                .iter()
                .map(|path| path.as_os_str().as_bytes().to_vec())
                .collect(),
            system_dependencies: observed.system_dependencies.clone(),
            host: observed.host.clone(),
            boot: observed.boot.clone(),
        };
        prepared.validate_for(request)?;
        events.linker_prepared(&prepared)
    }
    fn finish_inputs(&mut self) -> Option<Result<(), String>> {
        self.inputs.as_ref().map(|inputs| {
            native_inputs::validate(&inputs.held, Instant::now()).map_err(|error| error.to_string())
        })
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn original_gnu_has_zero_supplement_and_refuses_foreign_or_swapped_rights() {
        let original = RootGnu::new().unwrap();
        let copied = original
            .pipe
            .iter()
            .map(|fd| fd.try_clone().unwrap())
            .collect::<Vec<_>>();
        assert!(original.matches(&copied).unwrap());
        assert!(
            !original
                .matches(&[
                    copied[1].try_clone().unwrap(),
                    copied[0].try_clone().unwrap()
                ])
                .unwrap()
        );
        let foreign = RootGnu::new().unwrap();
        assert!(!original.matches(&foreign.pipe).unwrap());
        let mut queued: libc::c_int = -1;
        // SAFETY: FIONREAD only observes the held original GNU read end.
        assert_eq!(
            unsafe { libc::ioctl(original.pipe[0].as_raw_fd(), libc::FIONREAD, &mut queued) },
            0
        );
        assert_eq!(
            queued, 0,
            "one original implicit CPU never creates a supplemental GNU token"
        );
    }
}
