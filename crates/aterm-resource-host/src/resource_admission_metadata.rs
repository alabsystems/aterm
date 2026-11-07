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

/// Keep the separately negotiated purpose through paid preparation. Sharing
/// native input custody never converts a resolver query into metadata role 3.
#[derive(Clone)]
pub(super) enum Purpose {
    Metadata(SealedMetadataRequest),
    Resolver(trust_resource_controller::resolver::SealedResolverQueryRequest),
}
impl Purpose {
    pub(super) fn authorize(&self, check: &mut members::Authorization) {
        check.metadata = matches!(self, Self::Metadata(_));
        check.resolver = matches!(self, Self::Resolver(_));
    }
    pub(super) fn member(&self) -> &trust_resource_controller::Request {
        match self {
            Self::Metadata(request) => &request.member,
            Self::Resolver(request) => &request.member,
        }
    }
    fn root(&self) -> &[u8] {
        match self {
            Self::Metadata(request) => &request.root,
            Self::Resolver(request) => &request.root,
        }
    }
    fn digest(&self) -> &[u8; 32] {
        match self {
            Self::Metadata(request) => &request.program_sha256,
            Self::Resolver(request) => &request.program_sha256,
        }
    }
    fn validate(&self) -> io::Result<()> {
        match self {
            Self::Metadata(request) => request.validate(),
            Self::Resolver(request) => request.validate(),
        }
    }
    fn validate_prepared(&self, prepared: &MetadataPrepared) -> io::Result<()> {
        match self {
            Self::Metadata(request) => prepared.validate_for(request),
            Self::Resolver(request) => request.validate_prepared(prepared),
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
    held: HeldProgram,
    pub(super) inventory: MetadataPrepared,
    prelude: crate::metadata_prelude::Owner,
}
pub(super) struct Command {
    pub(super) command: spawnfd::Command,
    pub(super) inputs: Inputs,
}
pub(super) fn prepare(purpose: Purpose, deadline: Instant) -> io::Result<Command> {
    purpose.validate()?;
    let request = purpose.member();
    let program = PathBuf::from(OsStr::from_bytes(&request.command.program));
    let root = PathBuf::from(OsStr::from_bytes(purpose.root()));
    let held = HeldProgram::open(&root, &program, purpose.digest(), deadline)?;
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
    let mut command = spawnfd::command(&program);
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
            _ => return Err(refused("metadata requires its original numeric GNU pair")),
        })
        .collect::<Vec<_>>();
    let (prelude, command) = crate::metadata_prelude::Owner::new(&command, &targets, deadline)?;
    Ok(Command {
        command,
        inputs: Inputs {
            held,
            inventory,
            prelude,
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
}
impl member::MemberResources for Resources {
    fn configure(&mut self, command: &mut spawnfd::Command) -> io::Result<()> {
        if let Some(inputs) = &mut self.inputs {
            self.scheduling
                .configure_with_extra(command, vec![inputs.prelude.extra()?])?;
            inputs.prelude.configured(command)
        } else {
            self.scheduling.configure(command)
        }
    }
    fn prepare_target(&mut self, child: &mut spawnfd::Child) -> io::Result<bool> {
        if let Some(inputs) = &mut self.inputs {
            let held = &inputs.held;
            inputs.prelude.poll(child, |pid| {
                held.validate_loaded(
                    trust_resource_provenance::process::read_identity(pid)?,
                    Instant::now() + Duration::from_secs(2),
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
            inputs
                .held
                .validate_loaded(child, Instant::now() + Duration::from_secs(2))?;
        }
        Ok(())
    }
    fn finish_inputs(&mut self) -> Option<Result<(), String>> {
        self.inputs.as_ref().map(|inputs| {
            inputs
                .held
                .validate(Instant::now() + Duration::from_secs(2))
                .map_err(|error| error.to_string())
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
