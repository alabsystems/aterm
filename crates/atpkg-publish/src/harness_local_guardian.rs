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
const WORKER: &str = "__harness_local_worker";
#[cfg(any(test, debug_assertions))]
const FIXTURE_CLIENT: &str = "__harness_local_fixture_client";
#[cfg(any(test, debug_assertions))]
const FIXTURE_LEAF: &str = "__harness_local_fixture_leaf";
const TICK: Duration = Duration::from_millis(100);
const START_WITHIN: Duration = Duration::from_secs(60);

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

/// Called first by the executable: only the guardian's early arm forks, before threads.
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
    if verb != GUARD && verb != WORKER {
        return None;
    }
    let result = if verb == GUARD {
        if args.len() != 4 || args[2] != lifeline::FLAG || args[3] != "0" {
            return Some(ExitCode::from(2));
        }
        // SAFETY: the binary calls this as main's first operation, before any thread or
        // library initialization. fork takes no pointers. The parent runs no exit handlers.
        match unsafe { libc::fork() } {
            0 => guard(Path::new(&args[1])),
            pid if pid > 0 => unsafe { libc::_exit(0) },
            _ => Err(Error::cannot_run(
                "the local manager could not detach its owner",
            )),
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
        .args([std::ffi::OsStr::new(GUARD), dir.as_os_str()])
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
        let _ = child.try_wait(); // Reap only the short-lived detach parent, never the owner.
        std::thread::sleep(TICK);
    }
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
mod tests {
    use super::*;

    /// A launch directory whose owner record names `owner` and whose acknowledgement is `ack`.
    fn launch(owner: &Identity, ack: &Ack) -> (aterm_tempfile::TempDir, PathBuf) {
        let tmp = aterm_tempfile::Builder::new()
            .prefix("guardian-look-")
            .tempdir()
            .expect("private temp");
        let dir = tmp
            .path()
            .canonicalize()
            .expect("canonical launch")
            .join("launch");
        private_file::create_private_dir(&dir).expect("private launch");
        write(&dir, "owner.json", &owner.token()).expect("owner record");
        write(&dir, "ack.json", ack).expect("acknowledgement");
        (tmp, dir)
    }

    fn armed() -> Ack {
        Ack {
            done: false,
            success: true,
            reason: "local update owner armed".into(),
        }
    }

    fn finished() -> Ack {
        Ack {
            done: true,
            success: true,
            reason: "native ownership fixture completed".into(),
        }
    }

    /// This process with another birth: a recorded owner that no longer runs.
    fn ended_owner() -> Identity {
        let me = identity().expect("own identity");
        Identity {
            birth: process::Birth(me.birth.0.wrapping_add(1)),
            ..me
        }
    }

    /// The order `look` replaced, kept here as the negative control: acknowledgement first.
    fn look_acknowledgement_first(dir: &Path, between: impl FnOnce()) -> Option<Result<()>> {
        let ack = fs::read::<Ack>(dir, "ack.json").unwrap().expect("armed");
        if ack.done {
            return Some(Ok(()));
        }
        between();
        let owner = fs::read::<String>(dir, "owner.json")
            .unwrap()
            .expect("owner");
        (!live(&owner)).then(|| Err(Error::refused("ended without a final acknowledgement")))
    }

    #[test]
    fn an_owner_that_acknowledges_and_ends_between_the_reads_is_not_refused() {
        // 0.193.0's release tree (m3, 2026-11-10): the replacement owner wrote
        // `{"done":true,"success":true}` and ended between the client's two reads, and the
        // client refused it as "ended without a final acknowledgement".
        let (_tmp, dir) = launch(&ended_owner(), &armed());
        let mut was_armed = false;
        let verdict = look(&dir, &mut was_armed, || {
            write(&dir, "ack.json", &finished()).expect("final acknowledgement");
        })
        .expect("look");
        assert!(matches!(verdict, Some(Ok(()))), "{verdict:?}");

        // Negative control: the same moment, read in the replaced order, is refused.
        let (_tmp, dir) = launch(&ended_owner(), &armed());
        let old = look_acknowledgement_first(&dir, || {
            write(&dir, "ack.json", &finished()).expect("final acknowledgement");
        });
        assert!(matches!(old, Some(Err(_))), "{old:?}");
    }

    #[test]
    fn an_owner_that_ends_unacknowledged_is_still_refused() {
        let (_tmp, dir) = launch(&ended_owner(), &armed());
        let mut was_armed = false;
        let verdict = look(&dir, &mut was_armed, || {}).expect("look");
        let Some(Err(error)) = verdict else {
            panic!("an unacknowledged end must refuse: {verdict:?}");
        };
        assert!(
            error
                .to_string()
                .contains("ended without a final acknowledgement"),
            "{error}"
        );
        assert!(was_armed);
    }

    #[test]
    fn a_live_owner_still_working_has_no_verdict() {
        let (_tmp, dir) = launch(&identity().expect("own identity"), &armed());
        let mut was_armed = false;
        let verdict = look(&dir, &mut was_armed, || {}).expect("look");
        assert!(verdict.is_none(), "{verdict:?}");
        assert!(was_armed);
    }
}
