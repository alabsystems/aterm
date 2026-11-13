// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trusted preparation inside the already owned process, followed by a second
//! stopped-at-exec hold. The selected metadata image cannot fork descendants.
use aterm_ctl::resource_admission_transport as transport;
use aterm_uds::{CtlStream, process, spawnfd};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Read},
    os::{fd::OwnedFd, unix::ffi::OsStrExt},
    time::{Duration, Instant},
};

#[path = "metadata_prelude/plan.rs"]
mod plan;
use plan::{ExecPlan, Seal};

const VERB: &str = "__resource-metadata-exec-v1";
const MAX_PLAN: usize = 65536;

fn loader_environment(key: &OsStr) -> bool {
    atpkg::shim_env::NEVER_SET_PREFIXES
        .iter()
        .any(|prefix| key.as_bytes().starts_with(prefix.as_bytes()))
}

#[cfg(all(test, target_os = "macos"))]
#[path = "metadata_prelude/footprint_tests.rs"]
mod footprint_tests;
fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "native metadata preparation refused",
    )
}
fn refused_at(stage: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, stage)
}
fn alive(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}
#[cfg(target_os = "macos")]
pub(super) fn no_fork(pid: u32) -> io::Result<bool> {
    unsafe extern "C" {
        fn sandbox_check(pid: i32, operation: *const std::ffi::c_char, kind: i32, ...) -> i32;
    }
    let pid = i32::try_from(pid).map_err(|_| refused())?;
    // SAFETY: read-only query, a static operation and SANDBOX_FILTER_NONE.
    match unsafe { sandbox_check(pid, c"process-fork".as_ptr(), 0) } {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(refused()),
    }
}
#[cfg(not(target_os = "macos"))]
pub(super) fn no_fork(_: u32) -> io::Result<bool> {
    Err(io::ErrorKind::Unsupported.into())
}
#[cfg(target_os = "macos")]
fn install() -> io::Result<()> {
    unsafe extern "C" {
        fn sandbox_init(
            profile: *const std::ffi::c_char,
            flags: u64,
            error: *mut *mut std::ffi::c_char,
        ) -> i32;
        fn sandbox_free_error(error: *mut std::ffi::c_char);
    }
    let mut error = std::ptr::null_mut();
    // SAFETY: this private early single-process role installs one static fixed
    // policy on itself. There is no caller-supplied profile or parent fork.
    let result = unsafe {
        sandbox_init(
            c"(version 1)(allow default)(deny process-fork)".as_ptr(),
            0,
            &mut error,
        )
    };
    if !error.is_null() {
        unsafe {
            sandbox_free_error(error);
        }
    }
    if result != 0 || !no_fork(std::process::id())? {
        return Err(refused_at(
            "fixed native no-fork policy installation or observation failed",
        ));
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn install() -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(super) struct Owner {
    seal: Seal,
    channel: CtlStream,
    child_channel: Option<OwnedFd>,
    target: i32,
    program: OsString,
    arg0: Option<OsString>,
    arguments: Vec<OsString>,
    cwd: OsString,
    final_loader_environment: Vec<(Vec<u8>, Vec<u8>)>,
    footprint_mib: Option<u32>,
    payload: Vec<u8>,
    written: usize,
    acknowledgement: bool,
    started: bool,
    ready: bool,
    original: Option<process::Lineage>,
    retirement: Option<NoForkTarget>,
    #[cfg(all(test, target_os = "macos"))]
    prepared_target: Option<PreparedTarget>,
    deadline: Instant,
}

/// A native pre-GO observation owned by the two-stop preparation that enforced
/// it. There is no constructor from a PID, acknowledgement or wire observation.
pub(crate) struct NoForkTarget {
    original: process::Lineage,
}
impl NoForkTarget {
    pub(crate) fn matches(&self, pid: u32, birth: u64, group: i32) -> bool {
        self.original.identity.pid == pid as i32
            && self.original.identity.birth.0 == birth
            && self.original.pgid == pid
            && group == pid as i32
    }
}

// Qualification-only transfer of the SAME acknowledged final-exec hold. It
// owns no Child and cannot reconstruct one, create a grant, or survive restart.
#[cfg(all(test, target_os = "macos"))]
pub(super) struct PreparedTarget {
    original: process::Lineage,
    footprint_mib: u32,
    state: TransferState,
    deadline: Instant,
}
#[cfg(all(test, target_os = "macos"))]
enum TransferState {
    Held,
    ResumeAttempted,
    Running,
}
#[cfg(all(test, target_os = "macos"))]
impl PreparedTarget {
    pub(super) fn lineage(&self) -> process::Lineage {
        self.original
    }
    pub(super) fn configured_footprint_mib(&self) -> u32 {
        self.footprint_mib
    }
    fn validate(&self, child: &mut spawnfd::Child) -> io::Result<()> {
        alive(self.deadline)?;
        if child.exited()?
            || child.id() != self.original.identity.pid as u32
            || process::read_lineage(child.id() as i32) != Some(self.original)
        {
            return Err(refused());
        }
        Ok(())
    }
    pub(super) fn resume(&mut self, child: &mut spawnfd::Child) -> io::Result<()> {
        self.validate(child)?;
        if !matches!(self.state, TransferState::Held) {
            return Err(refused());
        }
        // An unknown native delivery cannot be offered again.
        self.state = TransferState::ResumeAttempted;
        child.resume()?;
        self.state = TransferState::Running;
        Ok(())
    }
    pub(super) fn sealed(self, child: &mut spawnfd::Child) -> io::Result<NoForkTarget> {
        self.validate(child)?;
        if !matches!(self.state, TransferState::Running) || !no_fork(child.id())? {
            return Err(refused());
        }
        Ok(NoForkTarget {
            original: self.original,
        })
    }
}
impl Owner {
    pub(super) fn new(
        command: &spawnfd::Command,
        targets: &[u32],
        deadline: Instant,
    ) -> io::Result<(Self, spawnfd::Command)> {
        Self::new_with_seal(command, targets, deadline, Seal::NoFork)
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn new_for_child_seal(
        command: &spawnfd::Command,
        targets: &[u32],
        deadline: Instant,
    ) -> io::Result<(Self, spawnfd::Command)> {
        Self::new_with_seal(command, targets, deadline, Seal::Child)
    }
    fn new_with_seal(
        command: &spawnfd::Command,
        targets: &[u32],
        deadline: Instant,
        seal: Seal,
    ) -> io::Result<(Self, spawnfd::Command)> {
        #[cfg(target_os = "macos")]
        let footprint_mib = command
            .get_footprint_limit()
            .map(spawnfd::FootprintLimit::mib);
        #[cfg(not(target_os = "macos"))]
        let footprint_mib = None;
        let target = (3..=130)
            .find(|fd| !targets.contains(fd))
            .ok_or_else(refused)? as i32;
        let (channel, child_channel) = aterm_uds::kept_out(CtlStream::pair)?;
        channel.set_nonblocking(true)?;
        let mut prelude = spawnfd::command(crate::runtime::inspector_program()?);
        let final_loader_environment = command
            .get_envs()
            .filter(|(key, _)| loader_environment(key))
            .filter_map(|(key, value)| {
                value.map(|value| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
            })
            .collect();
        prelude
            .arg(VERB)
            .arg(format!("--control-fd={target}"))
            .env_clear()
            .envs(
                command
                    .get_envs()
                    .filter(|(key, _)| !loader_environment(key))
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .current_dir(command.get_current_dir().ok_or_else(refused)?)
            .setsid(true)
            .start_suspended(true);
        Ok((
            Self {
                seal,
                channel,
                child_channel: Some(child_channel.into()),
                target,
                program: command.get_program().to_owned(),
                arg0: command.get_arg0().map(OsStr::to_owned),
                arguments: command.get_args().map(OsStr::to_owned).collect(),
                cwd: command
                    .get_current_dir()
                    .ok_or_else(refused)?
                    .as_os_str()
                    .to_owned(),
                final_loader_environment,
                footprint_mib,
                payload: Vec::new(),
                written: 0,
                acknowledgement: false,
                started: false,
                ready: false,
                original: None,
                retirement: None,
                #[cfg(all(test, target_os = "macos"))]
                prepared_target: None,
                deadline,
            },
            prelude,
        ))
    }
    pub(super) fn extra(&mut self) -> io::Result<(OwnedFd, i32)> {
        Ok((self.child_channel.take().ok_or_else(refused)?, self.target))
    }
    pub(super) fn configured(&mut self, command: &mut spawnfd::Command) -> io::Result<()> {
        if !self.payload.is_empty()
            || command
                .get_envs()
                .any(|(key, value)| loader_environment(key) && value.is_some())
        {
            return Err(refused());
        }
        // Resource configuration is complete. A later requested policy replaces
        // the selected command's initial one, but must never cap the prelude.
        // Absence of a new request preserves the selected initial policy.
        // Read it first; encoding failure leaves both command and owner intact.
        #[cfg(target_os = "macos")]
        let footprint_mib = command
            .get_footprint_limit()
            .map(spawnfd::FootprintLimit::mib)
            .or(self.footprint_mib);
        #[cfg(not(target_os = "macos"))]
        let footprint_mib = self.footprint_mib;
        let peer = transport::Peer {
            pid: std::process::id(),
            birth: process::read_birth(std::process::id() as i32)
                .ok_or_else(refused)?
                .0,
        };
        let plan = ExecPlan {
            seal: self.seal,
            parent: peer,
            footprint_mib,
            command: trust_resource_controller::CommandSpec {
                program: self.program.as_bytes().to_vec(),
                arg0: command
                    .get_arg0()
                    .or(self.arg0.as_deref())
                    .map(|word| word.as_bytes().to_vec()),
                args: self
                    .arguments
                    .iter()
                    .map(|argument| argument.as_bytes().to_vec())
                    .collect(),
                cwd: self.cwd.as_bytes().to_vec(),
                environment: command
                    .get_envs()
                    .filter_map(|(key, value)| {
                        value.map(|value| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
                    })
                    .chain(self.final_loader_environment.iter().cloned())
                    .collect(),
            },
            targets: command
                .guard_targets()
                .filter(|fd| *fd != self.target)
                .collect(),
        };
        let bytes = plan.encode()?;
        let mut payload = Vec::with_capacity(bytes.len() + 4);
        payload.extend((bytes.len() as u32).to_be_bytes());
        payload.extend(bytes);
        // Commit the fully encoded plan before the first ARM. No fallible work
        // follows the extraction; the actual intermediate spawn remains uncapped.
        #[cfg(target_os = "macos")]
        let _ = command.take_footprint_limit();
        self.footprint_mib = footprint_mib;
        self.payload = payload;
        Ok(())
    }
    pub(super) fn poll(
        &mut self,
        child: &mut spawnfd::Child,
        validate: impl FnOnce(u32) -> io::Result<()>,
    ) -> io::Result<bool> {
        if self.ready {
            return Ok(true);
        }
        alive(self.deadline)?;
        if !self.started {
            let lineage = process::read_lineage(child.id() as i32).ok_or_else(refused)?;
            // A session leader cannot move itself to another group, and the
            // installed no-fork policy prevents a nonleader escape child.
            if unsafe { libc::getsid(child.id() as i32) } != child.id() as i32 {
                return Err(refused_at(
                    "metadata preparation is not the original dedicated session leader",
                ));
            }
            let image = process::LoadedImage::read(child.id() as i32).ok_or_else(refused)?;
            if image.matches_file(crate::runtime::inspector_program()?) != Some(true)
                || lineage.parent != std::process::id()
            {
                return Err(refused_at(
                    "original trusted prelude image or parent changed",
                ));
            }
            let Some(stop) = child.try_job_event()? else {
                return Ok(false);
            };
            if !matches!(
                stop.notice(),
                spawnfd::ChildNotice::Stopped(spawnfd::ChildStop::SuspendedStart)
            ) || !matches!(
                stop.acknowledge()?,
                Some(spawnfd::ChildEvent::Stopped(
                    spawnfd::ChildStop::SuspendedStart
                ))
            ) {
                return Err(refused_at("original prelude stop was not acknowledged"));
            }
            self.original = Some(lineage);
            child.resume()?;
            self.started = true;
        }
        while self.written < self.payload.len() {
            match aterm_uds::fdpass::send_vectored(
                &self.channel,
                &[io::IoSlice::new(&self.payload[self.written..])],
            ) {
                Ok(0) => return Err(refused()),
                Ok(count) => self.written += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        if !self.acknowledgement {
            let mut bytes = [0; 2];
            match (&self.channel).read(&mut bytes) {
                Ok(1) if bytes[0] == 0x51 => self.acknowledgement = true,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                _ => {
                    return Err(refused_at(
                        "prelude closed or policy acknowledgement was malformed",
                    ));
                }
            }
        }
        let Some(stop) = child.try_job_event()? else {
            return Ok(false);
        };
        if !matches!(
            stop.notice(),
            spawnfd::ChildNotice::Stopped(spawnfd::ChildStop::SuspendedStart)
        ) {
            return Err(refused_at(
                "selected image did not supply a second suspended-at-exec stop",
            ));
        }
        let pid = stop.id();
        let current = process::read_lineage(pid as i32).ok_or_else(refused)?;
        let original = self.original.as_ref().ok_or_else(refused)?;
        if current.identity.pid != original.identity.pid
            || current.identity.birth != original.identity.birth
            || current.uniqueid != original.uniqueid
            || current.parent != original.parent
            || current.pgid != original.pgid
            || current.pgid != pid
            || unsafe { libc::getsid(pid as i32) } != pid as i32
            || match self.seal {
                Seal::NoFork => !no_fork(pid)?,
                #[cfg(all(test, target_os = "macos"))]
                Seal::Child => no_fork(pid)?,
            }
        {
            return Err(refused_at(
                "selected image birth, group or affirmative no-fork policy changed",
            ));
        }
        validate(pid)?;
        if !matches!(
            stop.acknowledge()?,
            Some(spawnfd::ChildEvent::Stopped(
                spawnfd::ChildStop::SuspendedStart
            ))
        ) {
            return Err(refused_at("selected image stop was not acknowledged"));
        }
        match self.seal {
            Seal::NoFork => self.retirement = Some(NoForkTarget { original: current }),
            #[cfg(all(test, target_os = "macos"))]
            Seal::Child => {
                self.prepared_target = Some(PreparedTarget {
                    original: current,
                    footprint_mib: self.footprint_mib.ok_or_else(refused)?,
                    state: TransferState::Held,
                    deadline: self.deadline,
                });
            }
        }
        self.ready = true;
        Ok(true)
    }

    pub(super) fn take_retirement(&mut self) -> Option<NoForkTarget> {
        self.retirement.take()
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn take_prepared_target(&mut self) -> io::Result<PreparedTarget> {
        self.prepared_target.take().ok_or_else(refused)
    }
}

pub(super) fn entry(args: &[OsString]) -> Option<std::process::ExitCode> {
    entry_at(args, Instant::now())
}

fn entry_at(args: &[OsString], started: Instant) -> Option<std::process::ExitCode> {
    if args.first().is_none_or(|word| word != VERB) {
        return None;
    }
    #[cfg(all(test, target_os = "macos"))]
    footprint_tests::observe_first_image_environment();
    let run = || -> io::Result<()> {
        if args.len() != 2 {
            return Err(refused());
        }
        let fd = args[1]
            .to_str()
            .and_then(|word| word.strip_prefix("--control-fd="))
            .and_then(|word| word.parse::<i32>().ok())
            .filter(|fd| (3..=130).contains(fd))
            .ok_or_else(refused)?;
        // SAFETY: this private early role consumes its one reserved connected
        // socket, before other libraries or threads can claim the target slot.
        let channel = CtlStream::from(unsafe { spawnfd::adopt_inherited_socket(fd)? });
        let deadline = started + Duration::from_secs(60);
        channel.set_nonblocking(true)?;
        let peer = transport::local_peer(&channel)?;
        let parent = process::read_lineage(std::process::id() as i32)
            .ok_or_else(refused)?
            .parent;
        let image = process::LoadedImage::read(peer.pid as i32).ok_or_else(refused)?;
        if peer.pid != parent || image.matches_file(&std::env::current_exe()?) != Some(true) {
            return Err(refused_at(
                "prelude controller is not the actual original native parent",
            ));
        }
        let bytes = transport::read_frame(&channel, deadline)?;
        if bytes.len() > MAX_PLAN || bytes.len() < 4 {
            return Err(refused());
        }
        let plan = ExecPlan::decode(&bytes)?;
        if plan.parent != peer
            || transport::local_peer(&channel)? != peer
            || plan.targets.contains(&fd)
        {
            return Err(refused());
        }
        let mut command = spawnfd::command(OsStr::from_bytes(&plan.command.program));
        command
            .args(plan.command.args.iter().map(|word| OsStr::from_bytes(word)))
            .env_clear()
            .envs(
                plan.command
                    .environment
                    .iter()
                    .map(|(key, value)| (OsStr::from_bytes(key), OsStr::from_bytes(value))),
            )
            .current_dir(OsStr::from_bytes(&plan.command.cwd))
            .start_suspended(true);
        // The decoded policy is launch configuration only. Prepare it on the
        // selected image's SETEXEC, never on the inspector/prelude spawn; the
        // second native stop remains the identity/no-fork check, not a public
        // receipt proving an installed memory policy or admission authority.
        #[cfg(target_os = "macos")]
        if let Some(mib) = plan.footprint_mib {
            command.footprint_limit(spawnfd::FootprintLimit::new(mib)?);
        }
        let targets = plan.targets;
        if let Some(arg0) = plan.command.arg0.as_ref() {
            command.arg0(OsStr::from_bytes(arg0));
        }
        let guards = targets
            .into_iter()
            .map(|target| {
                // SAFETY: exact original slots were explicitly installed by the
                // actual parent. Ownership is consumed once and preserves numbers.
                unsafe { std::os::fd::FromRawFd::from_raw_fd(target) }
            })
            .collect();
        command.inherit_exec_guards(guards)?;
        match plan.seal {
            Seal::NoFork => {
                install()?;
                if !no_fork(std::process::id())? {
                    return Err(refused());
                }
            }
            #[cfg(all(test, target_os = "macos"))]
            Seal::Child => {
                if no_fork(std::process::id())? {
                    return Err(refused());
                }
            }
        }
        if aterm_uds::fdpass::send_vectored(&channel, &[io::IoSlice::new(&[0x51])])? != 1 {
            return Err(refused());
        }
        Err(command.exec_suspended())
    };
    Some(match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aterm: native metadata preparation: {error}");
            std::process::ExitCode::from(75)
        }
    })
}

#[cfg(all(test, not(target_os = "macos")))]
#[path = "metadata_prelude_unsupported_tests.rs"]
mod unsupported_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "metadata_prelude_tests.rs"]
mod tests;
