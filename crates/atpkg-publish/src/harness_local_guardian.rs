// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Same-executable detached owner and gated whole-pass worker. The client owns only a
//! lifeline; this owner retains the source/program locks and capacity claim through retirement.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_uds::lifeline::{self, Lifeline, OnWake};
use aterm_uds::private_file;
use aterm_uds::process::{self, Identity};
use serde::{Deserialize, Serialize};

use super::{Config, Status, fs, producer};
use crate::ctx::Ctx;
use crate::error::{Error, Result};

#[path = "harness_local_guardian_process.rs"]
mod owned;

const GUARD: &str = "__harness_local_guard";
const DETACH: &str = "__harness_local_detach";
const WORKER: &str = "__harness_local_worker";
#[cfg(any(test, debug_assertions))]
const FIXTURE_CLIENT: &str = "__harness_local_fixture_client";
#[cfg(any(test, debug_assertions))]
const FIXTURE_LEAF: &str = "__harness_local_fixture_leaf";
const TICK: Duration = Duration::from_millis(100);
const START_WITHIN: Duration = Duration::from_secs(60);
/// How long the detach process waits for its child's exec report. Under the client's
/// [`START_WITHIN`], so a held exec is recorded before the client gives up.
const EXEC_WITHIN: Duration = Duration::from_secs(30);
/// The forked child's own line for a failed exec, written before its report: the private
/// log names the failure whether or not anything still reads the report.
const EXEC_FAILED: &[u8] = b"atpkg-publish: the local update owner could not exec its image\n";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    path: PathBuf,
    device: u64,
    inode: u64,
    size: u64,
    modified: i64,
    modified_ns: i64,
    changed: i64,
    changed_ns: i64,
}

impl Image {
    fn current() -> Result<Self> {
        Self::at(&std::env::current_exe().map_err(|e| Error::io("manager image", &e))?)
    }
    fn at(path: &Path) -> Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let path = path
            .canonicalize()
            .map_err(|e| Error::io("pin manager image", &e))?;
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| Error::io("manager image metadata", &e))?;
        if !meta.is_file() || meta.mode() & 0o022 != 0 {
            return Err(Error::refused(
                "manager image must be a regular non-writable-by-others file",
            ));
        }
        Ok(Self {
            path,
            device: meta.dev(),
            inode: meta.ino(),
            size: meta.len(),
            modified: meta.mtime(),
            modified_ns: meta.mtime_nsec(),
            changed: meta.ctime(),
            changed_ns: meta.ctime_nsec(),
        })
    }
    fn check(&self) -> Result<()> {
        if Self::current()? != *self {
            return Err(Error::refused(
                "manager image changed; active attempt preserved",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Boot {
    schema: u32,
    token: String,
    home: PathBuf,
    config: Config,
    image: Image,
    client: String,
    watch: bool,
    #[cfg(any(test, debug_assertions))]
    fixture: Option<Fixture>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema: u32,
    token: String,
    guardian: String,
    home: PathBuf,
    config: Config,
    image: Image,
    initial: Status,
    lease: fs::LeaseProof,
    capacity: String,
    compact: bool,
    cores: u32,
    #[cfg(any(test, debug_assertions))]
    fixture: Option<Fixture>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Active {
    schema: u32,
    token: String,
    guardian: String,
    leader: Option<String>,
    settled: bool,
    compact: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceActive {
    schema: u32,
    source: [u64; 2],
    home: PathBuf,
    prefix: PathBuf,
    program: String,
    active: Active,
}

impl Boot {
    fn controlled(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            self.fixture.is_some()
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            false
        }
    }
}

fn save_active(session: &fs::Session, config: &Config, active: &Active) -> Result<()> {
    let source = SourceActive {
        schema: 1,
        source: session.source_identity(),
        home: session.home().into(),
        prefix: config.prefix.clone(),
        program: config.program.clone(),
        active: active.clone(),
    };
    // Publish at the account/source lock first. No worker is spawned or gated until both
    // durable records name it, so another prefix cannot miss a dead owner's live group.
    session.save_source(&source)?;
    write(session.root(), "active.json", active)
}

fn recover_source(session: &fs::Session) -> Result<()> {
    let Some(mut source) = session.read_source::<SourceActive>()? else {
        return Ok(());
    };
    if source.schema != 1
        || source.source != session.source_identity()
        || source.active.schema != 1
        || !valid_token(&source.active.token)
    {
        return Err(Error::refused(
            "shared source attempt has an unknown binding",
        ));
    }
    if source.active.settled {
        return Ok(());
    }
    let leader = if let Some(leader) = &source.active.leader {
        Some(leader.clone())
    } else {
        let root = fs::root(&source.prefix, &source.program)?;
        fs::ancestry(&source.home, &root, false)?;
        let dir = attempt(&root, &source.active.token)?;
        fs::ancestry(&source.home, &dir, false)?;
        let ready = fs::read::<String>(&dir, "ready.json")?;
        if ready.is_none() && fs::read::<String>(&dir, "go.json")?.is_some() {
            return Err(Error::refused(
                "shared source start gate lacks its worker identity",
            ));
        }
        ready
    };
    if let Some(leader) = leader {
        let expected = Identity::parse(&leader)
            .ok_or_else(|| Error::refused("shared source worker identity is invalid"))?;
        if live(&leader) || !owned::empty_group(expected.pid as u32) {
            return Err(Error::refused(
                "another prefix still owns an unretired source worker; no pass started",
            ));
        }
    }
    source.active.settled = true;
    session.save_source(&source)
}

impl Request {
    fn memory_mib(&self) -> u64 {
        if self.compact {
            256
        } else {
            self.config.memory_mib
        }
    }
    fn disk_mib(&self) -> u64 {
        if self.compact {
            0
        } else {
            self.config.disk_mib.min(8192)
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    done: bool,
    success: bool,
    reason: String,
}

/// Constructible only after the hidden worker verifies its actual guardian, group and gate.
pub(super) struct WorkerPermit {
    request: Request,
    dir: PathBuf,
}

impl WorkerPermit {
    pub(super) fn source_scope(&self) -> producer::SourceScope {
        producer::SourceScope {
            home: self.request.home.clone(),
            config: self.request.config.clone(),
            lease: self.request.lease.clone(),
            guardian: Identity::parse(&self.request.guardian)
                .expect("worker gate checked guardian identity"),
        }
    }

    pub(super) fn package_scope(&self) -> producer::PackageScope {
        producer::PackageScope {
            cache: producer::cache_root(&self.request.config),
            attempt: self.request.token.clone(),
            guardian: Identity::parse(&self.request.guardian)
                .expect("worker gate checked identity")
                .pid as u32,
            group: std::process::id(),
            jobs: self.request.cores,
            registrar: self.request.image.path.clone(),
            budget: self.request.config.disk_mib << 20,
        }
    }
}

fn token() -> Result<String> {
    aterm_uds::rand::hex_token::<16>().map_err(|e| Error::io("manager token", &e))
}
fn valid_token(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn identity() -> Result<Identity> {
    process::read_identity(i32::try_from(std::process::id()).unwrap_or(0))
        .ok_or_else(|| Error::cannot_run("manager process identity is unavailable"))
}
fn live(text: &str) -> bool {
    Identity::parse(text)
        .is_some_and(|expected| process::read_identity(expected.pid).as_ref() == Some(&expected))
}
fn write<T: serde::Serialize>(dir: &Path, name: &str, value: &T) -> Result<()> {
    private_file::write_private(dir, name, &fs::bytes(value)?)
        .map_err(|e| Error::io(format!("manager {name}"), &e))
}
fn require<T: serde::de::DeserializeOwned>(dir: &Path, name: &str) -> Result<T> {
    fs::read(dir, name)?.ok_or_else(|| Error::refused(format!("missing private manager {name}")))
}
fn subdir(home: &Path, root: &Path, kind: &str, token: &str) -> Result<PathBuf> {
    if !valid_token(token) {
        return Err(Error::refused("malformed manager attempt token"));
    }
    let dir = root.join(".ubd").join(format!("{kind}-{token}.noindex"));
    if std::fs::symlink_metadata(&dir).is_ok() {
        return Err(Error::refused(
            "manager attempt namespace is occupied; preserved",
        ));
    }
    fs::create(home, &dir)?;
    private_file::check_private_dir(&dir).map_err(|e| Error::io("private manager attempt", &e))?;
    Ok(dir)
}
fn attempt(root: &Path, token: &str) -> Result<PathBuf> {
    if !valid_token(token) {
        return Err(Error::refused("malformed active manager token"));
    }
    Ok(root.join(".ubd").join(format!("attempt-{token}.noindex")))
}
fn stopping(signalled: &AtomicBool, line: &RefCell<OnWake>) -> bool {
    signalled.load(Ordering::SeqCst) || line.borrow_mut().cut().is_some()
}

/// Called first by the executable: the detach entry forks and immediately execs the owner.
/// Normal publication commands return None and retain their existing entry.
#[must_use = "return a handled guardian command's exit status to the caller"]
pub fn early_entry() -> Option<ExitCode> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let verb = args.first()?.to_str()?;
    #[cfg(any(test, debug_assertions))]
    if verb == FIXTURE_LEAF {
        if args.len() != 2 {
            return Some(ExitCode::from(2));
        }
        let dir = Path::new(&args[1]);
        if write(
            dir,
            "leaf-ready.json",
            &identity().map(|id| id.token()).unwrap_or_default(),
        )
        .is_err()
        {
            return Some(ExitCode::from(1));
        }
        loop {
            if fs::read::<bool>(dir, "finish-fixture.json").ok().flatten() == Some(true) {
                return Some(ExitCode::SUCCESS);
            }
            std::thread::sleep(TICK);
        }
    }
    #[cfg(any(test, debug_assertions))]
    if verb == FIXTURE_CLIENT {
        if args.len() != 2 {
            return Some(ExitCode::from(2));
        }
        return Some(match fixture_client(Path::new(&args[1])) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(error.exit_code())
            }
        });
    }
    if verb != DETACH && verb != GUARD && verb != WORKER {
        return None;
    }
    let result = if verb == DETACH || verb == GUARD {
        if args.len() != 4 || args[2] != lifeline::FLAG || args[3] != "0" {
            return Some(ExitCode::from(2));
        }
        if verb == DETACH {
            detach(Path::new(&args[1]))
        } else {
            guard(Path::new(&args[1]))
        }
    } else {
        if args.len() != 2 {
            return Some(ExitCode::from(2));
        }
        worker(Path::new(&args[1]))
    };
    Some(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("atpkg-publish: {error}");
            ExitCode::from(error.exit_code())
        }
    })
}

/// What the detach process read from its child's close-on-exec report pipe. Only
/// [`Exec::Failed`] is a fact about the owner (there is none); the owner's own
/// acknowledgement, which the client reads, is the one proof that an owner runs.
#[derive(Debug)]
enum Exec {
    /// The pipe closed with nothing written: no exec error was reported. An exec closes
    /// it, and so does a child that a signal ended before its exec, so this does not say
    /// that a new image started.
    Unreported,
    /// The child reported the error `execve` returned with, and ended.
    Failed(std::io::Error),
    /// No verdict: the exec is still held at the bound, or the report could not be read.
    /// The child may yet become the owner.
    Unknown(String),
}

/// Read the forked child's exec report for at most `within`: never an unbounded read, because
/// the system can hold an exec (macOS assesses a program before it starts) and the write end
/// stays open in the child until the exec happens. The bound is judged after every wake, a
/// timeout or an interrupting signal alike.
fn exec_report(report: &std::io::PipeReader, within: Duration) -> Exec {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;

    let by = Instant::now() + within;
    let mut word = [0u8; 4];
    let mut got = 0;
    while got < word.len() {
        let left = by.saturating_duration_since(Instant::now());
        let mut ready = libc::pollfd {
            fd: report.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `ready` is one live pollfd naming a descriptor this borrow keeps open.
        let polled = unsafe {
            libc::poll(
                &raw mut ready,
                1,
                i32::try_from(left.as_nanos().div_ceil(1_000_000)).unwrap_or(i32::MAX),
            )
        };
        // A ready pipe is read without blocking. A timeout joins an interrupting signal:
        // nothing to read yet, and the bound is judged below.
        let read = match polled {
            0 => Err(std::io::ErrorKind::Interrupted.into()),
            n if n < 0 => Err(std::io::Error::last_os_error()),
            _ => (&*report).read(&mut word[got..]),
        };
        match read {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                if Instant::now() >= by {
                    return Exec::Unknown(format!(
                        "the system still holds its exec after {} s",
                        within.as_secs()
                    ));
                }
            }
            Err(error) => return Exec::Unknown(format!("its exec report is unreadable: {error}")),
        }
    }
    match got {
        0 => Exec::Unreported,
        n if n == word.len() => {
            Exec::Failed(std::io::Error::from_raw_os_error(i32::from_ne_bytes(word)))
        }
        _ => Exec::Unknown("its exec report was cut short".into()),
    }
}

/// Fork a child that only execs `program`, and read its exec report for at most `within`.
/// Returns the child's pid with what was read; the child never returns. A failed exec's line
/// goes to `log`, a descriptor open in this process.
///
/// # Safety
/// `program` is a NUL-terminated path, and `argv` and `envp` are NULL-terminated arrays of
/// pointers to NUL-terminated strings, all alive for the call.
unsafe fn exec_detached(
    program: &std::ffi::CStr,
    argv: &[*const libc::c_char],
    envp: &[*const libc::c_char],
    log: std::os::fd::RawFd,
    within: Duration,
) -> Result<(libc::pid_t, Exec)> {
    use std::os::fd::AsRawFd as _;

    // Both ends are close-on-exec and kept out of every other fork and spawn
    // (`aterm_uds::pipe`): an exec closes the child's write end, which the parent reads as end
    // of file, and the owner inherits neither end. This fork alone is handed the write end.
    let (report, reporter) = aterm_uds::pipe().map_err(|e| Error::io("owner exec report", &e))?;
    let reporter_fd = reporter.as_raw_fd();
    let handing = aterm_uds::spawn_lock::handing(&[reporter_fd]);
    // SAFETY: fork takes no pointers. The child calls only the async-signal-safe execve,
    // write and _exit, on data prepared before the fork and kept alive by the caller: no
    // allocator, destructor, lock or ACL code runs there, whatever threads the parent has.
    let forked = unsafe { libc::fork() };
    match forked {
        0 => unsafe {
            libc::execve(program.as_ptr(), argv.as_ptr(), envp.as_ptr());
            exec_failed(log, reporter_fd)
        },
        pid if pid > 0 => {
            drop(handing);
            drop(reporter);
            Ok((pid, exec_report(&report, within)))
        }
        _ => Err(Error::io(
            "the local manager could not detach its owner",
            &std::io::Error::last_os_error(),
        )),
    }
}

/// The forked child after `execve` returned: log the failure, report its error, and end.
///
/// The line is written first and always. The report cannot stand in for it: once the parent
/// stopped at its bound no reader is left, and the report's write fails.
///
/// # Safety
/// Only in the forked child, with `log` an open descriptor and `report` the write end of
/// its exec report pipe.
unsafe fn exec_failed(log: std::os::fd::RawFd, report: std::os::fd::RawFd) -> ! {
    // Reads errno only: an OS error holds its code inline, with no allocation.
    let word = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(0)
        .to_ne_bytes();
    // SAFETY: write and _exit are async-signal-safe; `EXEC_FAILED` and `word` are live.
    unsafe {
        libc::write(log, EXEC_FAILED.as_ptr().cast(), EXEC_FAILED.len());
        libc::write(report, word.as_ptr().cast(), word.len());
        libc::_exit(127)
    }
}

/// Prepare all Rust-owned data before fork. The child must exec before calling any
/// Rust or platform service: Darwin's ACL reader can fail with EINVAL after fork.
///
/// The detach process fails only where it knows no owner exists: before the fork, on a
/// failed fork, or on the exec error its child reported before ending. Otherwise it ends
/// with success, which claims nothing: the child may have started the owner's image, still
/// be held before it, or have been ended by a signal. The owner's acknowledgement decides.
fn detach(dir: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let boot: Boot = require(dir, "boot.json")?;
    boot.image.check()?;
    let cstring = |bytes: &[u8]| {
        CString::new(bytes).map_err(|_| Error::refused("guardian exec data contains NUL"))
    };
    let image = cstring(boot.image.path.as_os_str().as_bytes())?;
    // A controlled body can name an image that is not there, for the failed-exec path.
    #[cfg(any(test, debug_assertions))]
    let program = if boot.controlled() && fs::read::<bool>(dir, "fail-exec.json")? == Some(true) {
        cstring(dir.join("no-such-image").as_os_str().as_bytes())?
    } else {
        image.clone()
    };
    #[cfg(not(any(test, debug_assertions)))]
    let program = image.clone();
    let arguments = [
        image,
        cstring(GUARD.as_bytes())?,
        cstring(dir.as_os_str().as_bytes())?,
        cstring(lifeline::FLAG.as_bytes())?,
        cstring(b"0")?,
    ];
    let environment: Vec<CString> = std::env::vars_os()
        .map(|(key, value)| {
            let mut entry = key.as_os_str().as_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_os_str().as_bytes());
            cstring(&entry)
        })
        .collect::<Result<_>>()?;
    let argv: Vec<_> = arguments
        .iter()
        .map(|word| word.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let envp: Vec<_> = environment
        .iter()
        .map(|word| word.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    // SAFETY: `program`, `argv` and `envp` were built above from NUL-checked strings, each
    // array ends in a null pointer, and their backing storage outlives the call. fd 0 remains
    // the client lifeline, and stdout/stderr retain the private logs, the child's line among
    // them.
    let (_, exec) =
        unsafe { exec_detached(&program, &argv, &envp, libc::STDERR_FILENO, EXEC_WITHIN) }?;
    match exec {
        Exec::Failed(error) => return Err(Error::io("exec the local update owner", &error)),
        Exec::Unknown(reason) => {
            eprintln!("atpkg-publish: the local update owner may still start: {reason}");
        }
        Exec::Unreported => {}
    }
    // SAFETY: _exit takes no pointers. Whatever became of the child is its own to
    // acknowledge; this process ends without running exit handlers.
    unsafe { libc::_exit(0) }
}

/// A controlled worker body for native process-ownership checks. It launches only this
/// same image's sleeping leaf; no provider, compiler, installer or current selection runs.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fixture {
    /// Complete without compiling or selecting anything.
    Complete,
    /// Keep this worker and its own sleeping descendant live until owner cancellation.
    HangWithChild,
    /// Exercise a real rejected stop publication, with no stop record for the owner to read.
    FailedStopPublication,
    /// Exit with a success bit but no completed terminal result.
    IncompleteAck,
}

/// A private fixture launch prepared without starting a process. Spawn `program args` and
/// inspect its private evidence before removing the fixture directory.
#[cfg(any(test, debug_assertions))]
pub struct FixtureLaunch {
    /// The exact existing publication executable, never a copy.
    pub program: PathBuf,
    /// The private hidden fixture-client invocation.
    pub args: Vec<std::ffi::OsString>,
    /// Private evidence directory; no executable is written here.
    pub directory: PathBuf,
    /// The account-wide durable source association used by cross-prefix recovery.
    pub source_record: PathBuf,
}

/// Prepare a native ownership check around the same real client/guardian/worker flow.
/// # Errors
/// Invalid ownership/configuration, occupied namespace or private record failure.
#[cfg(any(test, debug_assertions))]
pub fn prepare_fixture(
    image: &Path,
    home: &Path,
    config: &Config,
    watch: bool,
    fixture: Fixture,
) -> Result<FixtureLaunch> {
    let session = fs::Session::take(home, config)?;
    let boot = Boot {
        schema: 1,
        token: token()?,
        home: home.into(),
        config: config.clone(),
        image: Image::at(image)?,
        client: String::new(),
        watch,
        fixture: Some(fixture),
    };
    let directory = subdir(home, session.root(), "launch", &boot.token)?;
    write(&directory, "boot.json", &boot)?;
    Ok(FixtureLaunch {
        program: boot.image.path,
        args: vec![FIXTURE_CLIENT.into(), directory.clone().into()],
        directory,
        source_record: session.source_record_path(),
    })
}

#[cfg(any(test, debug_assertions))]
fn fixture_client(dir: &Path) -> Result<()> {
    let mut boot: Boot = require(dir, "boot.json")?;
    if boot.fixture.is_none() {
        return Err(Error::refused(
            "fixture client has no controlled fixture body",
        ));
    }
    boot.image.check()?;
    boot.client = identity()?.token();
    write(dir, "boot.json", &boot)?;
    println!("local fixture client running");
    launch_boot(&boot, dir)
}

#[cfg(any(test, debug_assertions))]
fn fixture_worker(permit: &WorkerPermit, fixture: Fixture) -> Result<()> {
    let mut status = permit.request.initial.clone();
    let fixture = if permit.request.compact {
        Fixture::Complete
    } else {
        fixture
    };
    match fixture {
        Fixture::Complete => {
            status.reason =
                "native ownership fixture completed; no harness was built or selected".into();
            status.failures = 0;
            status.retry_seconds = permit.request.config.poll_seconds;
            write(&permit.dir, "status.json", &status)?;
            write(
                &permit.dir,
                "worker-result.json",
                &Ack {
                    done: true,
                    success: true,
                    reason: status.reason,
                },
            )
        }
        Fixture::IncompleteAck => write(
            &permit.dir,
            "worker-result.json",
            &Ack {
                done: false,
                success: true,
                reason: "controlled incomplete terminal acknowledgement".into(),
            },
        ),
        Fixture::HangWithChild | Fixture::FailedStopPublication => {
            status.pending = Some(super::Operation::Build);
            status.reason =
                "native ownership fixture waiting with a child; no harness was built or selected"
                    .into();
            write(&permit.dir, "status.json", &status)?;
            let mut command = aterm_uds::spawnfd::command(&permit.request.image.path);
            command.args([std::ffi::OsStr::new(FIXTURE_LEAF), permit.dir.as_os_str()]);
            command
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null());
            let _child = command
                .spawn()
                .map_err(|e| Error::io("fixture descendant", &e))?;
            let by = Instant::now() + START_WITHIN;
            while fs::read::<String>(&permit.dir, "leaf-ready.json")?.is_none() {
                if Instant::now() >= by {
                    return Err(Error::refused("fixture leaf never showed its execution"));
                }
                std::thread::sleep(TICK);
            }
            if matches!(fixture, Fixture::FailedStopPublication) {
                // The real bounded record writer rejects this before producing stop.json.
                // Therefore the owner's stop-file reader cannot itself trigger retirement.
                owned::GuardTools::new(permit).stop(&"x".repeat(128 << 10));
            }
            loop {
                if fs::read::<bool>(&permit.dir, "finish-fixture.json")? == Some(true) {
                    return Ok(());
                }
                if !live(&permit.request.guardian) {
                    process::kill_self_now();
                }
                std::thread::sleep(TICK);
            }
        }
    }
}

pub(super) fn launch(ctx: &Ctx, config: &Config, watch: bool) -> Result<()> {
    let session = fs::Session::take(&ctx.home, config)?;
    let boot = Boot {
        schema: 1,
        token: token()?,
        home: ctx.home.clone(),
        config: config.clone(),
        image: Image::current()?,
        client: identity()?.token(),
        watch,
        #[cfg(any(test, debug_assertions))]
        fixture: None,
    };
    let dir = subdir(&ctx.home, session.root(), "launch", &boot.token)?;
    write(&dir, "boot.json", &boot)?;
    drop(session);
    launch_boot(&boot, &dir)
}

fn launch_boot(boot: &Boot, dir: &Path) -> Result<()> {
    let signalled = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&signalled);
    if !process::take_stop_signals("local-update-client-stop", move |_| {
        stopped.store(true, Ordering::SeqCst);
    }) {
        return Err(Error::cannot_run(
            "could not arm local updater stop handling",
        ));
    }
    // The detached owner takes these exact locks before acknowledging startup. A racing
    // owner can win them, but cannot share them; a lost startup never authorizes a pass.
    boot.image.check()?;
    let mut command = aterm_uds::spawnfd::command(&boot.image.path);
    command
        .args([std::ffi::OsStr::new(DETACH), dir.as_os_str()])
        .setsid(true);
    command.stdout(
        private_file::open_private_rw(dir, "guardian.stdout")
            .map_err(|e| Error::refused(e.to_string()))?,
    );
    command.stderr(
        private_file::open_private_rw(dir, "guardian.stderr")
            .map_err(|e| Error::refused(e.to_string()))?,
    );
    let mut lifeline =
        Some(Lifeline::arm(&mut command, dir).map_err(|e| Error::io("guardian lifeline", &e))?);
    let mut child = command
        .spawn()
        .map_err(|e| Error::io("start local update owner", &e))?;
    let by = Instant::now() + START_WITHIN;
    let mut armed = false;
    loop {
        if signalled.load(Ordering::SeqCst)
            && let Some(line) = lifeline.take()
        {
            line.cut();
        }
        if let Some(verdict) = look(dir, &mut armed, || {})? {
            return verdict;
        }
        if !armed && Instant::now() >= by {
            drop(lifeline.take());
            let _ = child.kill();
            return Err(Error::cannot_run(format!(
                "guardian startup is unacknowledged; preserved {}",
                dir.display()
            )));
        }
        // Reap only the short-lived detach process, never the owner. Its failure code says
        // no owner exists (`detach`), so nothing can acknowledge: refuse now with its reason
        // rather than at the startup bound. Its success, or its end by a signal, says nothing
        // of the owner, whose acknowledgement alone arms this client.
        let detached = child.try_wait();
        if !armed
            && let Ok(Some(status)) = detached
            && status.code().is_some_and(|code| code != 0)
        {
            drop(lifeline.take());
            return Err(Error::cannot_run(format!(
                "local update owner did not start: {}; preserved {}",
                detach_reason(dir),
                dir.display()
            )));
        }
        std::thread::sleep(TICK);
    }
}

/// The last line the failed detach process left in its private log.
fn detach_reason(dir: &Path) -> String {
    private_file::read_private(dir, "guardian.stderr", 1 << 16)
        .ok()
        .and_then(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "its private log gives no reason".into())
}

/// One look at a launch directory: the owner's verdict once it has one, `None` while it works.
///
/// The owner writes its final acknowledgement and only then ends, so its liveness is read
/// before its acknowledgement: an owner seen ended has written every acknowledgement it ever
/// will, and the read after that sees the last. Read the other way round, an owner that
/// acknowledged and ended between the two reads (`between` stands for that moment) would be
/// refused although it finished. `armed` turns true at the owner's first acknowledgement.
fn look(dir: &Path, armed: &mut bool, between: impl FnOnce()) -> Result<Option<Result<()>>> {
    let ended = fs::read::<String>(dir, "owner.json")?.is_some_and(|owner| !live(&owner));
    between();
    if let Some(ack) = fs::read::<Ack>(dir, "ack.json")? {
        *armed = true;
        if ack.done {
            return Ok(Some(if ack.success {
                Ok(())
            } else {
                Err(Error::refused(ack.reason))
            }));
        }
    }
    Ok((*armed && ended).then(|| {
        Err(Error::refused(
            "local update owner ended without a final acknowledgement; active attempt is preserved",
        ))
    }))
}

fn guard(dir: &Path) -> Result<()> {
    let boot: Boot = require(dir, "boot.json")?;
    boot.config.validate(&boot.home)?;
    fs::ancestry(&boot.home, dir, false)?;
    if boot.schema != 1 || !valid_token(&boot.token) || !live(&boot.client) {
        return Err(Error::refused(
            "local manager client is no longer its recorded process",
        ));
    }
    boot.image.check()?;
    let line = RefCell::new(
        OnWake::new(lifeline::adopt(0).map_err(|e| Error::io("adopt client lifeline", &e))?)
            .map_err(|e| Error::io("watch client lifeline", &e))?,
    );
    let signalled = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&signalled);
    if !process::take_stop_signals("local-update-owner-stop", move |_| {
        stopped.store(true, Ordering::SeqCst);
    }) {
        return Err(Error::cannot_run("could not arm owner stop handling"));
    }
    let session = fs::Session::take(&boot.home, &boot.config)?;
    let guardian = identity()?.token();
    write(dir, "owner.json", &guardian)?;
    write(
        dir,
        "ack.json",
        &Ack {
            done: false,
            success: true,
            reason: "local update owner armed".into(),
        },
    )?;
    let should_stop = || {
        #[cfg(any(test, debug_assertions))]
        if boot.controlled()
            && fs::read::<bool>(dir, "crash-owner.json").ok().flatten() == Some(true)
        {
            process::kill_self_now();
        }
        stopping(&signalled, &line)
    };
    loop {
        if should_stop() {
            break;
        }
        let mut status = session.status()?;
        if let Err(error) = recover(&session, &boot, &mut status) {
            status.reason = format!("{error}; prior attempt preserved");
            status.failures = status.failures.saturating_add(1);
            status.retry_seconds = super::retry_delay(&boot.config, status.failures).as_secs();
            session.save(&status)?;
            if !boot.watch {
                write(
                    dir,
                    "ack.json",
                    &Ack {
                        done: true,
                        success: false,
                        reason: status.reason,
                    },
                )?;
                return Ok(());
            }
        } else {
            let failures = status.failures;
            let before = run_attempt(&session, &boot, &guardian, &mut status, &should_stop, true);
            let mut result = before.and_then(|()| {
                run_attempt(&session, &boot, &guardian, &mut status, &should_stop, false)
            });
            if !should_stop() {
                let update_reason = status.reason.clone();
                let compacted =
                    run_attempt(&session, &boot, &guardian, &mut status, &should_stop, true);
                match compacted {
                    Ok(()) => {
                        status.reason = format!("{update_reason}; {}", status.reason);
                        session.save(&status)?;
                    }
                    Err(error) => {
                        if result.is_ok() {
                            result = Err(error);
                        }
                    }
                }
            }
            if let Err(error) = &result {
                status.failures = failures.saturating_add(1);
                status.retry_seconds = super::retry_delay(&boot.config, status.failures).as_secs();
                status.reason = format!("{error}; partials preserved");
                session.save(&status)?;
            }
            if !boot.watch {
                let reason = status.reason.clone();
                write(
                    dir,
                    "ack.json",
                    &Ack {
                        done: true,
                        success: result.is_ok(),
                        reason,
                    },
                )?;
                return result;
            }
        }
        let until = Instant::now()
            + Duration::from_secs(
                status
                    .retry_seconds
                    .clamp(super::MIN_POLL_SECONDS, super::MAX_POLL_SECONDS),
            );
        while Instant::now() < until && !should_stop() {
            std::thread::sleep(TICK);
        }
    }
    write(
        dir,
        "ack.json",
        &Ack {
            done: true,
            success: true,
            reason: "local updater stopped after worker retirement".into(),
        },
    )
}

fn recover(session: &fs::Session, boot: &Boot, status: &mut Status) -> Result<()> {
    let config = &boot.config;
    recover_source(session)?;
    let Some(mut active) = fs::read::<Active>(session.root(), "active.json")? else {
        return Ok(());
    };
    if active.schema != 1 || !valid_token(&active.token) {
        return Err(Error::refused("unsupported active updater record"));
    }
    if active.settled {
        return Ok(());
    }
    let dir = attempt(session.root(), &active.token)?;
    let leader = active
        .leader
        .as_deref()
        .map(str::to_owned)
        .or(fs::read::<String>(&dir, "ready.json")?);
    if let Some(leader) = leader {
        let expected = Identity::parse(&leader)
            .ok_or_else(|| Error::refused("invalid previous worker identity"))?;
        if live(&leader) || !owned::empty_group(expected.pid as u32) {
            return Err(Error::refused(
                "previous worker retirement is not proved; no new pass started",
            ));
        }
    } else if fs::read::<String>(&dir, "go.json")?.is_some() {
        return Err(Error::refused(
            "previous start gate lacks a proved worker identity",
        ));
    }
    // Without a ready leader there was no issued start gate; an orphaned gated helper
    // verifies the old guardian before doing anything and cannot start a producer.
    if let Some(previous) = fs::read::<Status>(&dir, "status.json")? {
        *status = previous;
    }
    if !boot.controlled() && !active.compact && active.guardian == identity()?.token() {
        let cache = producer::cache_root(config);
        fs::create(session.home(), &cache)?;
        let _coordinator = private_file::PrivateLock::take(
            &cache,
            atpkg::build_cache::registered::package::COORDINATOR,
            Duration::ZERO,
            "compiler recovery coordinator is held",
        )
        .map_err(|e| Error::io("compiler recovery coordinator", &e))?;
        let args: Vec<std::ffi::OsString> = vec![
            "settled".into(),
            "--cache".into(),
            cache.into(),
            "--attempt".into(),
            active.token.clone().into(),
        ];
        atpkg::build_cache::registered::package::request_local(&args).map_err(|reason| {
            Error::refused(format!("prior compiler target settlement: {reason}"))
        })?;
    }
    settle(config, status)?;
    session.save(status)?;
    active.settled = true;
    save_active(session, config, &active)
}

fn settle(config: &Config, status: &mut Status) -> Result<()> {
    let lock = atpkg::lock::try_lock_store(&atpkg::store::Layout {
        prefix: config.prefix.clone(),
    })
    .map_err(|e| Error::refused(format!("current readback after retirement is unknown: {e}")))?;
    status.selected = match atpkg::local_harness::observe_current(&lock, &config.program)
        .map_err(|e| Error::io("current after worker retirement", &e))?
    {
        atpkg::local_harness::CurrentLocal::Absent
        | atpkg::local_harness::CurrentLocal::Foreign(_) => None,
        atpkg::local_harness::CurrentLocal::Selected(identity) => Some(identity.into()),
    };
    status.pending = None;
    status.live_adoption = false;
    Ok(())
}

fn run_attempt(
    session: &fs::Session,
    boot: &Boot,
    guardian: &str,
    status: &mut Status,
    stopped: &dyn Fn() -> bool,
    compact: bool,
) -> Result<()> {
    use aterm_capacity::{Admit, Need};
    session.check_owned()?;
    recover(session, boot, status)?;
    let cache = producer::cache_root(&boot.config);
    fs::create(&boot.home, &cache)?;
    let _coordinator = if compact {
        None
    } else {
        Some(
            private_file::PrivateLock::take(
                &cache,
                atpkg::build_cache::registered::package::COORDINATOR,
                Duration::ZERO,
                "compiler target coordinator is held",
            )
            .map_err(|e| Error::io("compiler target coordinator", &e))?,
        )
    };
    let seat = atpkg::build_admit::Seat::live();
    let ledger = seat.ledger.as_ref().ok_or_else(|| {
        Error::cannot_run("capacity ledger is unavailable; update remains unstarted")
    })?;
    let need = Need::new(format!("local harness {}", boot.config.program))
        .mem((if compact { 256 } else { boot.config.memory_mib }) << 20)
        .cores(if compact { 1 } else { boot.config.jobs })
        .disk(
            (if compact {
                0
            } else {
                boot.config.disk_mib.min(8192)
            }) << 20,
            &boot.config.out,
        )
        .checkout(boot.config.checkout.to_string_lossy())
        .covers();
    status.reason = "waiting for local update memory capacity".into();
    session.save(status)?;
    let mut claim = aterm_capacity::admit_until(
        ledger,
        seat.machine,
        need,
        // Controlled bodies launch no compiler and use their fixture's private seat.
        if boot.controlled() {
            seat.mode
        } else {
            Admit::Wait
        },
        seat.pace,
        &mut |line| eprintln!("{line}"),
        stopped,
    )
    .map_err(|e| Error::refused(format!("local update capacity: {e}")))?;
    let request = Request {
        schema: 1,
        token: token()?,
        guardian: guardian.into(),
        home: boot.home.clone(),
        config: boot.config.clone(),
        image: boot.image.clone(),
        initial: status.clone(),
        lease: session.lease()?,
        capacity: claim.record().cover_marker(),
        compact,
        cores: claim
            .record()
            .held
            .as_ref()
            .map_or(1, |held| held.cores.max(1)),
        #[cfg(any(test, debug_assertions))]
        fixture: boot.fixture,
    };
    let dir = subdir(&boot.home, session.root(), "attempt", &request.token)?;
    write(&dir, "request.json", &request)?;
    write(&dir, "status.json", status)?;
    let mut active = Active {
        schema: 1,
        token: request.token.clone(),
        guardian: guardian.into(),
        leader: None,
        settled: false,
        compact,
    };
    save_active(session, &boot.config, &active)?;
    boot.image.check()?;
    let mut command = aterm_uds::spawnfd::command(&boot.image.path);
    command
        .args([std::ffi::OsStr::new(WORKER), dir.as_os_str()])
        .process_group(0)
        .stdin(aterm_uds::spawnfd::Stdio::null());
    command.stdout(
        private_file::open_private_rw(&dir, "worker.stdout")
            .map_err(|e| Error::refused(e.to_string()))?,
    );
    command.stderr(
        private_file::open_private_rw(&dir, "worker.stderr")
            .map_err(|e| Error::refused(e.to_string()))?,
    );
    command.env(aterm_capacity::COVER_ENV, claim.record().cover_marker());
    owned::scrub(&mut command);
    let child = command
        .spawn()
        .map_err(|e| Error::io("start gated local worker", &e))?;
    let mut child = owned::Worker::new(child);
    let mut result = (|| {
        let leader = owned::bind(&child)?;
        active.leader = Some(leader.token());
        claim
            .credit(&[child.id()])
            .map_err(|e| Error::io("credit local worker", &e))?;
        save_active(session, &boot.config, &active)?;
        owned::run(
            &mut child,
            &leader,
            &dir,
            &boot.config,
            &request.token,
            stopped,
            request.memory_mib(),
        )
    })();
    // Every success/error path keeps the claim and lifetime locks until the entire group
    // has retired. The process module never reports kill(2)'s ACK as an empty group.
    let retired = owned::retire(&mut child);
    if let Err(error) = retired {
        status.reason =
            format!("{error}; owner keeps locks and capacity while retirement is unresolved");
        let _ = session.save(status);
        loop {
            if matches!(owned::finish_if_retired(&mut child), Ok(true)) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    if let Some(worker_status) = fs::read::<Status>(&dir, "status.json")? {
        *status = worker_status;
    }
    let mut package_args: Vec<std::ffi::OsString> = vec![
        "settled".into(),
        "--cache".into(),
        cache.clone().into(),
        "--attempt".into(),
        request.token.clone().into(),
    ];
    if result.is_ok() {
        package_args.push("--success".into());
    }
    let package_settle = if compact || boot.controlled() {
        Ok(aterm_json::Value::Null)
    } else {
        atpkg::build_cache::registered::package::request_local(&package_args)
            .map_err(|reason| Error::refused(format!("compiler target settlement: {reason}")))
    };
    let settle_result = settle(&boot.config, status);
    if !compact
        && result.is_ok()
        && status.source_version.is_some()
        && status.source_version != status.selected
    {
        result = Err(Error::refused(
            "current changed or failed verification after worker retirement",
        ));
    }
    session.save(status)?;
    settle_result?;
    package_settle?;
    active.settled = true;
    save_active(session, &boot.config, &active)?;
    drop(claim);
    result
}

fn worker(dir: &Path) -> Result<()> {
    let request: Request = require(dir, "request.json")?;
    request.config.validate(&request.home)?;
    fs::ancestry(&request.home, dir, false)?;
    request.image.check()?;
    let me = identity()?;
    let parent = process::read_lineage(me.pid)
        .ok_or_else(|| Error::refused("worker lineage unavailable"))?;
    let guardian = Identity::parse(&request.guardian)
        .ok_or_else(|| Error::refused("invalid guardian identity"))?;
    if request.schema != 1
        || !valid_token(&request.token)
        || parent.parent != guardian.pid as u32
        || parent.pgid != me.pid as u32
        || !live(&request.guardian)
    {
        return Err(Error::refused(
            "worker is not in its recorded guardian's own group",
        ));
    }
    write(dir, "ready.json", &me.token())?;
    let by = Instant::now() + START_WITHIN;
    loop {
        if !live(&request.guardian) || Instant::now() >= by {
            return Err(Error::refused(
                "worker start gate expired or its guardian ended",
            ));
        }
        if fs::read::<String>(dir, "go.json")?.as_deref() == Some(&request.token) {
            break;
        }
        std::thread::sleep(TICK);
    }
    owned::limits(&request.config)?;
    verify_worker_gate(dir, &request, &me, &guardian)?;
    let permit = WorkerPermit {
        request,
        dir: dir.into(),
    };
    #[cfg(any(test, debug_assertions))]
    if let Some(fixture) = permit.request.fixture {
        return fixture_worker(&permit, fixture);
    }
    if permit.request.compact {
        let cache = producer::cache_root(&permit.request.config);
        let args: Vec<std::ffi::OsString> = vec!["compact".into(), "--cache".into(), cache.into()];
        let reply = atpkg::build_cache::registered::package::request_local(&args)
            .map_err(|reason| Error::refused(format!("compiler target compaction: {reason}")))?;
        let mut status = permit.request.initial.clone();
        status.reason = format!(
            "compiler target compaction: {}",
            aterm_json::to_string(&reply).map_err(|e| Error::refused(e.to_string()))?
        );
        write(&permit.dir, "status.json", &status)?;
        write(
            &permit.dir,
            "worker-result.json",
            &Ack {
                done: true,
                success: true,
                reason: status.reason,
            },
        )?;
        return Ok(());
    }
    let ctx = Ctx::from_env()?;
    if ctx.home != permit.request.home {
        return Err(Error::refused("worker HOME changed"));
    }
    let tools = owned::GuardTools::new(&permit);
    let mut driver = producer::Producer::new(
        &ctx,
        &tools,
        producer::QualifiedGuardian::in_worker(&permit),
    );
    let mut status = permit.request.initial.clone();
    let result = super::pass(&permit.request.config, &mut status, &mut driver, |status| {
        if !live(&permit.request.guardian) {
            return Err(Error::refused("guardian ended before status publication"));
        }
        write(&permit.dir, "status.json", status)
    });
    write(
        &permit.dir,
        "worker-result.json",
        &Ack {
            done: true,
            success: result.is_ok(),
            reason: status.reason.clone(),
        },
    )?;
    result
}

/// A private file is a transport, not the ownership grant. Bind it to this image's actual
/// GUARD parent, that parent's stable boot, exact held locks, active gate and admitted claim.
fn verify_worker_gate(
    dir: &Path,
    request: &Request,
    me: &Identity,
    guardian: &Identity,
) -> Result<()> {
    let root = fs::root(&request.config.prefix, &request.config.program)?;
    if attempt(&root, &request.token)? != dir {
        return Err(Error::refused(
            "worker is outside its exact attempt namespace",
        ));
    }
    let parent_image =
        process::read_process_path(guardian.pid).and_then(|path| path.canonicalize().ok());
    if parent_image.as_ref() != Some(&request.image.path) {
        return Err(Error::refused(
            "worker parent is not the recorded manager image",
        ));
    }
    let argv = process::read_process_argv(guardian.pid)
        .ok_or_else(|| Error::refused("guardian entry is unreadable"))?;
    if argv.len() != 5 || argv[1] != GUARD || argv[3] != lifeline::FLAG || argv[4] != "0" {
        return Err(Error::refused(
            "worker parent is not the early guardian entry",
        ));
    }
    let launch = PathBuf::from(&argv[2]);
    fs::ancestry(&request.home, &launch, false)?;
    let boot: Boot = require(&launch, "boot.json")?;
    let expected_launch = root
        .join(".ubd")
        .join(format!("launch-{}.noindex", boot.token));
    if boot.schema != 1
        || !valid_token(&boot.token)
        || launch != expected_launch
        || boot.config != request.config
        || boot.image != request.image
        || boot.home != request.home
    {
        return Err(Error::refused(
            "guardian boot differs from the worker scope",
        ));
    }
    fs::verify_lease(&request.home, &request.config, &request.lease)?;
    let active: Active = require(&root, "active.json")?;
    let source: SourceActive = fs::source_record(&request.home, &request.config.checkout)?
        .ok_or_else(|| Error::refused("worker has no shared source attempt"))?;
    if source.schema != 1
        || source.home != request.home
        || source.prefix != request.config.prefix
        || source.program != request.config.program
        || source.active != active
    {
        return Err(Error::refused(
            "worker differs from the account-wide source attempt",
        ));
    }
    if active.schema != 1
        || active.settled
        || active.token != request.token
        || active.guardian != request.guardian
        || active.leader.as_deref() != Some(me.token().as_str())
        || active.compact != request.compact
    {
        return Err(Error::refused(
            "worker is not the guardian's exact active attempt",
        ));
    }
    let seat = atpkg::build_admit::Seat::live();
    let ledger = seat
        .ledger
        .as_ref()
        .ok_or_else(|| Error::refused("worker capacity ledger is unavailable"))?;
    let cover = std::env::var(aterm_capacity::COVER_ENV).ok();
    let claims = ledger
        .of_pid(guardian.pid as u32)
        .map_err(|e| Error::io("verify guardian capacity claim", &e))?;
    if cover.as_deref() != Some(request.capacity.as_str())
        || !claims.iter().any(|claim| {
            claim.held.is_some()
                && claim.cover_marker() == request.capacity
                && claim.need.mem == request.memory_mib() << 20
                && claim.need.covers
                && claim.need.disk == request.disk_mib() << 20
                && claim
                    .held
                    .as_ref()
                    .is_some_and(|held| held.cores.max(1) == request.cores)
                && (1..=request.config.jobs).contains(&request.cores)
                && claim.checkout == request.config.checkout.to_string_lossy()
                && claim.credit.contains(&(me.pid as u32))
        })
    {
        return Err(Error::refused(
            "worker lacks its actual guardian's admitted covering claim",
        ));
    }
    if !live(&request.guardian) {
        return Err(Error::refused("guardian changed during worker admission"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "harness_local_guardian_tests.rs"]
mod tests;
