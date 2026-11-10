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
use plan::ExecPlan;

const VERB: &str = "__resource-metadata-exec-v1";
const MAX_PLAN: usize = 65536;

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
    channel: CtlStream,
    child_channel: Option<OwnedFd>,
    target: i32,
    program: OsString,
    arg0: Option<OsString>,
    arguments: Vec<OsString>,
    cwd: OsString,
    footprint_mib: Option<u32>,
    payload: Vec<u8>,
    written: usize,
    acknowledgement: bool,
    started: bool,
    ready: bool,
    original: Option<process::Lineage>,
    retirement: Option<NoForkTarget>,
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
impl Owner {
    pub(super) fn new(
        command: &spawnfd::Command,
        targets: &[u32],
        deadline: Instant,
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
        prelude
            .arg(VERB)
            .arg(format!("--control-fd={target}"))
            .env_clear()
            .envs(
                command
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .current_dir(command.get_current_dir().ok_or_else(refused)?)
            .setsid(true)
            .start_suspended(true);
        Ok((
            Self {
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
                footprint_mib,
                payload: Vec::new(),
                written: 0,
                acknowledgement: false,
                started: false,
                ready: false,
                original: None,
                retirement: None,
                deadline,
            },
            prelude,
        ))
    }
    pub(super) fn extra(&mut self) -> io::Result<(OwnedFd, i32)> {
        Ok((self.child_channel.take().ok_or_else(refused)?, self.target))
    }
    pub(super) fn configured(&mut self, command: &mut spawnfd::Command) -> io::Result<()> {
        if !self.payload.is_empty() {
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
            let stop = child.try_job_event()?.ok_or_else(refused)?;
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
            || !no_fork(pid)?
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
        self.ready = true;
        self.retirement = Some(NoForkTarget { original: current });
        Ok(true)
    }

    pub(super) fn take_retirement(&mut self) -> Option<NoForkTarget> {
        self.retirement.take()
    }
}

pub(super) fn entry(args: &[OsString]) -> Option<std::process::ExitCode> {
    entry_at(args, Instant::now())
}

fn entry_at(args: &[OsString], started: Instant) -> Option<std::process::ExitCode> {
    if args.first().is_none_or(|word| word != VERB) {
        return None;
    }
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
        install()?;
        if !no_fork(std::process::id())? {
            return Err(refused());
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
mod unsupported_tests {
    #[test]
    fn unsupported_platform_cannot_claim_native_no_fork_enforcement() {
        assert_eq!(
            super::no_fork(std::process::id()).unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
        assert_eq!(
            super::install().unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd as _;
    use std::os::unix::process::ExitStatusExt as _;
    const ACTOR: &str = "metadata_prelude::tests::native_prelude_actor";
    const DELAYED_ACTOR: &str = "metadata_prelude::tests::native_delayed_prelude_actor";
    const TARGET: &str = "metadata_prelude::tests::native_no_fork_target";
    const REPLACEMENT: &str = "metadata_prelude::tests::native_exec_replacement";
    struct ChildGuard(spawnfd::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn native_prelude_actor() {
        let Some(fd) = std::env::var_os("ATERM_TEST_METADATA_PRELUDE_FD") else {
            return;
        };
        let args = vec![
            OsString::from(VERB),
            OsString::from(format!("--control-fd={}", fd.to_str().unwrap())),
        ];
        let result = entry(&args).expect("private prelude route");
        std::process::exit(if result == std::process::ExitCode::SUCCESS {
            0
        } else {
            75
        });
    }

    #[test]
    fn native_delayed_prelude_actor() {
        let Some(fd) = std::env::var_os("ATERM_TEST_METADATA_PRELUDE_FD") else {
            return;
        };
        let args = vec![
            OsString::from(VERB),
            OsString::from(format!("--control-fd={}", fd.to_str().unwrap())),
        ];
        let result = entry_at(&args, Instant::now() - Duration::from_secs(31)).unwrap();
        std::process::exit(if result == std::process::ExitCode::SUCCESS {
            0
        } else {
            75
        });
    }

    #[test]
    fn actual_native_preparation_survives_scheduling_delay() {
        crate::runtime::test_endpoint().unwrap();
        let scratch = aterm_tempfile::tempdir().unwrap();
        let marker = scratch.path().join("delayed-target-ran");
        let (result, _) = run_selected_actor(&marker, None, DELAYED_ACTOR);
        assert!(result.success());
        assert!(marker.is_file());
    }
    #[test]
    fn native_no_fork_target() {
        let Some(marker) = std::env::var_os("ATERM_TEST_METADATA_MARKER") else {
            return;
        };
        assert!(no_fork(std::process::id()).unwrap());
        assert_eq!(
            std::env::args_os().next().unwrap(),
            OsStr::new("original-selected-argv0")
        );
        assert_eq!(unsafe { libc::getsid(0) }, std::process::id() as i32);
        assert_eq!(
            unsafe { libc::setsid() },
            -1,
            "a selected session leader cannot detach"
        );
        assert_eq!(
            unsafe { libc::setpgid(0, libc::getpgid(libc::getppid())) },
            -1,
            "selected metadata cannot leave its owned group"
        );
        assert_eq!(unsafe { libc::getpgid(0) }, std::process::id() as i32);
        if let Ok(mode) = std::env::var("ATERM_TEST_METADATA_EXEC") {
            let marker = std::path::Path::new(&marker);
            std::fs::write(
                marker.with_extension("attempt"),
                std::process::id().to_string(),
            )
            .unwrap();
            let mut attempt = spawnfd::command(std::env::current_exe().unwrap());
            attempt
                .arg0("original-selected-argv0")
                .args(["--exact", REPLACEMENT, "--nocapture"])
                .env_clear()
                .env("ATERM_TEST_METADATA_REPLACEMENT", marker);
            match mode.as_str() {
                "control" => {}
                "session" => {
                    attempt.setsid(true);
                }
                "own-group" => {
                    attempt.process_group(0);
                }
                "parent-group" => {
                    attempt.process_group(unsafe { libc::getpgid(libc::getppid()) });
                }
                _ => panic!("unknown native exec control"),
            }
            let error = attempt.exec();
            assert_eq!(
                error.raw_os_error(),
                Some(libc::EPERM),
                "actual SETEXEC escape flags must be denied, not merely unsupported: {error}"
            );
            assert_eq!(unsafe { libc::getsid(0) }, std::process::id() as i32);
            assert_eq!(unsafe { libc::getpgid(0) }, std::process::id() as i32);
            assert!(no_fork(std::process::id()).unwrap());
            std::fs::write(marker.with_extension("denied"), b"returned EPERM").unwrap();
            return;
        }
        assert_eq!(
            std::env::var("CARGO_MAKEFLAGS").unwrap(),
            "-j --jobserver-auth=3,4"
        );
        for (fd, access) in [(3, libc::O_RDONLY), (4, libc::O_WRONLY)] {
            let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
            assert!(std::os::unix::fs::FileTypeExt::is_fifo(
                &file.metadata().unwrap().file_type()
            ));
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_ACCMODE,
                access
            );
        }
        // This actual target is past SETEXEC and final GO. Neither fork nor
        // native posix_spawn may create an unowned/escaped descendant.
        let result = unsafe { libc::fork() };
        if result == 0 {
            unsafe { libc::_exit(93) };
        }
        assert_eq!(result, -1, "metadata target unexpectedly forked");
        if let Ok(mut child) = spawnfd::command("/usr/bin/true").spawn() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("metadata target unexpectedly posix_spawned");
        }
        let fd = std::env::var("ATERM_TEST_METADATA_PRELUDE_FD")
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) },
            -1,
            "prelude control descriptor leaked into selected image"
        );
        std::fs::write(
            marker,
            "actual target denied fork and posix_spawn after same-pid exec",
        )
        .unwrap();
    }
    #[test]
    fn native_exec_replacement() {
        let Some(marker) = std::env::var_os("ATERM_TEST_METADATA_REPLACEMENT") else {
            return;
        };
        assert!(no_fork(std::process::id()).unwrap());
        assert_eq!(
            std::env::args_os().next().unwrap(),
            OsStr::new("original-selected-argv0")
        );
        std::fs::write(
            marker,
            format!(
                "{} {} {}",
                std::process::id(),
                unsafe { libc::getsid(0) },
                unsafe { libc::getpgid(0) }
            ),
        )
        .unwrap();
    }
    #[test]
    fn actual_same_pid_two_holds_apply_no_fork_before_target_go() {
        crate::runtime::test_endpoint().unwrap();
        assert!(
            !no_fork(std::process::id()).unwrap(),
            "unsandboxed allow control"
        );
        let scratch = aterm_tempfile::tempdir().unwrap();
        let marker = scratch.path().join("target-ran");
        let (result, _) = run_selected(&marker, None);
        assert!(
            result.success(),
            "actual target result {result:?}, signal {:?}",
            result.signal()
        );
        assert!(marker.is_file());
        // SETEXEC applies group/session attrs after image activation on this
        // OS; denial can terminate that SAME process rather than return errno.
        // Keep destructive attempts separate and prove a no-attrs replacement
        // really executes under the identical no-fork/session-leader profile.
        for mode in ["control", "session", "own-group", "parent-group"] {
            let marker = scratch.path().join(mode);
            let (result, pid) = run_selected(&marker, Some(mode));
            assert_eq!(
                std::fs::read_to_string(marker.with_extension("attempt")).unwrap(),
                pid.to_string()
            );
            if mode == "control" {
                assert!(
                    result.success(),
                    "same-profile SETEXEC allow control {result:?}"
                );
                assert_eq!(
                    std::fs::read_to_string(&marker).unwrap(),
                    format!("{pid} {pid} {pid}")
                );
            } else {
                assert!(!marker.exists(), "SETEXEC escape replacement ran");
                assert!(
                    result.signal() == Some(libc::SIGKILL)
                        || (result.success()
                            && std::fs::read(marker.with_extension("denied")).unwrap()
                                == b"returned EPERM"),
                    "group/session denial was neither exact EPERM nor actual fatal exec rejection: {result:?}"
                );
            }
        }
    }
    fn run_selected(
        marker: &std::path::Path,
        mode: Option<&str>,
    ) -> (std::process::ExitStatus, u32) {
        run_selected_actor(marker, mode, ACTOR)
    }

    fn run_selected_actor(
        marker: &std::path::Path,
        mode: Option<&str>,
        actor: &str,
    ) -> (std::process::ExitStatus, u32) {
        let mut target = spawnfd::command(std::env::current_exe().unwrap());
        target
            .arg0("original-selected-argv0")
            .args(["--exact", TARGET, "--nocapture"])
            .env_clear()
            .env("ATERM_TEST_METADATA_MARKER", marker)
            .current_dir(marker.parent().unwrap());
        if let Some(mode) = mode {
            target.env("ATERM_TEST_METADATA_EXEC", mode);
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let (mut owner, _) = Owner::new(&target, &[3, 4], deadline).unwrap();
        let mut prelude = spawnfd::command(std::env::current_exe().unwrap());
        prelude
            .args(["--exact", actor, "--nocapture"])
            .env_clear()
            .env("ATERM_TEST_METADATA_MARKER", marker)
            .env("ATERM_TEST_METADATA_PRELUDE_FD", owner.target.to_string())
            .env("CARGO_MAKEFLAGS", "-j --jobserver-auth=3,4")
            .current_dir(marker.parent().unwrap())
            .setsid(true)
            .start_suspended(true);
        if let Some(mode) = mode {
            prelude.env("ATERM_TEST_METADATA_EXEC", mode);
        }
        let extra = owner.extra().unwrap();
        let (read, write) = io::pipe().unwrap();
        prelude
            .inherit_guards_at(vec![
                extra,
                (OwnedFd::from(read), 3),
                (OwnedFd::from(write), 4),
            ])
            .unwrap();
        owner.configured(&mut prelude).unwrap();
        let mut guard = ChildGuard(prelude.spawn().unwrap());
        let child = &mut guard.0;
        let original = process::read_lineage(child.id() as i32).unwrap();
        assert!(!marker.exists());
        loop {
            if owner
                .poll(child, |pid| {
                    assert_eq!(pid, original.identity.pid as u32);
                    assert!(no_fork(pid)?);
                    Ok(())
                })
                .unwrap()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "actual second stopped-at-exec hold"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let selected = process::read_lineage(child.id() as i32).unwrap();
        assert_eq!(selected.identity.birth, original.identity.birth);
        assert_eq!(selected.uniqueid, original.uniqueid);
        assert_eq!(selected.pgid, original.pgid);
        assert_ne!(selected.identity.idversion, original.identity.idversion);
        assert!(
            !marker.exists(),
            "trusted preparation is not selected target GO"
        );
        child.resume().unwrap();
        let result = child.wait().unwrap();
        (result, child.id())
    }
}
