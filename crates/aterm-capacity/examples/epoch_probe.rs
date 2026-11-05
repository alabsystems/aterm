// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit native qualification: `epoch_probe --targo /absolute/genuine/targo
//! --jobserver-source /absolute/trust/vendor/jobserver`.
//! Add `--owner-crash` to qualify actual owner SIGKILL with running and retired work.
//! No unavailable helper is counted as a passing test.

#[cfg(unix)]
mod native {
    use std::{
        fs,
        io::{self, Read as _, Write as _},
        num::NonZeroU32,
        os::{
            fd::{AsFd as _, AsRawFd as _},
            unix::{
                fs::{DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _},
                process::ExitStatusExt as _,
            },
        },
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use aterm_capacity::{
        Ledger, Machine, Need, Polled, Procs, Reading, Waiter,
        build_cpu::{Owner, RETAINED_DIRECTORY_NAME, RetainedPool, RetainedSlotState},
        cohort_namespace::Namespace,
        epoch_controller::{Config, EpochController},
        leadership::Fence,
    };
    use aterm_uds::{
        childwake::ChildWake,
        process,
        ready::{self, Interest, Watch},
        spawnfd::{self, Child, Stdio},
    };

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    const HANG: Duration = Duration::from_secs(60);

    fn wait_epoch(
        controller: &EpochController,
        wake: &ChildWake,
        until: Instant,
    ) -> io::Result<()> {
        let plan = controller.readiness();
        if plan.immediate {
            return Ok(());
        }
        // An orphan's exit need not notify this owner. Retry actual settlement
        // at a paced deadline only once the original helper really retired and
        // exited, never replace pipe/child readiness with timer-only polling.
        let until = if plan.retry_finish {
            until.min(Instant::now() + Duration::from_millis(100))
        } else {
            until
        };
        let mut interests = vec![Watch::new(wake.as_fd(), Interest::Read)];
        interests.extend(plan.interests());
        ready::any_interests_until(&interests, until)?;
        wake.drain()
    }

    // This private qualification namespace has a deterministic admission input;
    // it neither changes the account's global allocator nor measures throughput.
    struct ProbeMachine;
    impl Machine for ProbeMachine {
        fn read(&self, _: Option<&Path>) -> Reading {
            Reading {
                cores: 8,
                load1: Some(0.0),
                load5: Some(0.0),
                available: Some(40 << 30),
                physical: Some(64 << 30),
                ..Reading::default()
            }
        }
        fn procs(&self) -> Option<Procs> {
            None
        }
    }

    struct World {
        directory: Option<aterm_tempfile::TempDir>,
        passed: bool,
    }

    impl World {
        fn new() -> io::Result<Self> {
            Ok(Self {
                directory: Some(aterm_tempfile::tempdir()?),
                passed: false,
            })
        }
        fn path(&self) -> &Path {
            self.directory.as_ref().unwrap().path()
        }
        fn keep_evidence(&mut self) -> PathBuf {
            self.passed = true;
            self.directory.take().unwrap().keep()
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            if !self.passed {
                let _ = fs::write(self.path().join("abort"), b"native qualification stopped\n");
                let _ = fs::write(self.path().join("probe.release"), b"cleanup\n");
                if let Some(directory) = self.directory.take() {
                    let path = directory.keep();
                    let _ = writeln!(
                        io::stderr(),
                        "kept failed native epoch probe: {}",
                        path.display()
                    );
                }
            }
        }
    }

    fn private_dir(path: &Path) -> io::Result<()> {
        fs::DirBuilder::new().mode(0o700).create(path)
    }

    fn wait_note(
        world: &Path,
        stage: &str,
        controller: &mut EpochController,
        fence: &Fence,
    ) -> Result<String> {
        let deadline = Instant::now() + HANG;
        loop {
            // Root requests are queued, not synchronously written. Keep the
            // actual private protocol moving while awaiting the child's barrier.
            controller.poll(fence)?;
            match fs::read_to_string(world.join(format!("probe.{stage}"))) {
                Ok(note) => return Ok(note),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "actual member barrier did not arrive",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn require(condition: bool, why: &'static str) -> Result<()> {
        if condition {
            Ok(())
        } else {
            Err(io::Error::other(why).into())
        }
    }

    fn execute_artifact(path: &Path, world: &Path) -> Result<()> {
        use aterm_uds::spawnfd::{self, Stdio};
        let stdout = world.join("artifact.stdout");
        let mut child = spawnfd::command(path)
            .stdin(Stdio::null())
            .stdout(Stdio::from(fs::File::create(&stdout)?))
            .stderr(Stdio::from(fs::File::create(
                world.join("artifact.stderr"),
            )?))
            .spawn()?;
        let deadline = Instant::now() + HANG;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "actual emitted artifact did not exit",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        require(
            status.success() && fs::read(&stdout)? == b"native-artifact-ok\n",
            "actual emitted artifact failed",
        )
    }

    fn publish_challenge(root: &Path, stage: &str, challenge: &str) -> Result<()> {
        let path = root.join(format!("probe.{stage}"));
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => return Err(io::Error::other("fresh challenge already exists").into()),
        }
        let pending = root.join(format!(".probe.{stage}.pending"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pending)?;
        writeln!(file, "{challenge}")?;
        drop(file);
        // Actors wait for existence before reading. Publish the complete nonce
        // by rename so they never observe a created-but-empty request.
        fs::rename(pending, path)?;
        Ok(())
    }

    fn script(world: &Path) -> String {
        let root = format!("{:?}", world.to_str().unwrap());
        r#"
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
unsafe extern "C" { fn getpgrp() -> i32; }
fn note(root: &Path, stage: &str, body: &str) {
    let path = root.join(format!("probe.{stage}"));
    let temporary = path.with_extension(format!("{stage}.tmp"));
    // SAFETY: getpgrp only observes the actual executing process's group.
    let group = unsafe { getpgrp() };
    std::fs::write(&temporary, format!("pid={}\npgid={group}\n{body}\n", std::process::id())).unwrap();
    std::fs::rename(temporary, path).unwrap();
}
fn wait(root: &Path, stage: &str) {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(!root.join("abort").exists(), "native probe aborted");
        if root.join(format!("probe.{stage}")).exists() { return; }
        assert!(Instant::now() < until, "native probe barrier timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn main() {
    let root = PathBuf::from(__ROOT__);
    if std::env::var_os("ATERM_EPOCH_PROBE_MEMBER").is_some() {
        let flags = std::env::var("CARGO_MAKEFLAGS").unwrap();
        let expected = format!("--jobserver-auth=fifo:{}", root.join("transport.noindex/credits.fifo").display());
        assert!(flags.split_ascii_whitespace().any(|flag| flag == expected));
        note(&root, "ready", &format!("flags={flags}"));
        if root.join("crash.running").exists() {
            wait(&root, "challenge");
            let challenge = std::fs::read_to_string(root.join("probe.challenge")).unwrap();
            note(&root, "alive", &format!("challenge={}", challenge.trim()));
            wait(&root, "release");
            note(&root, "released", "released");
            return;
        }
        wait(&root, "import");
        let challenge = std::fs::read_to_string(root.join("probe.import")).unwrap();
        // SAFETY: the inherited metadata names the original known FIFO, not
        // descriptors that earlier opens could reuse. The helper has exited.
        let server = unsafe { jobserver::Client::from_env_ext(true) }.client.unwrap();
        assert_eq!(server.available().unwrap(), 0);
        assert!(server.try_acquire().unwrap().is_none());
        note(&root, "imported", &format!("flags={flags}\nchallenge={}\nempty=true", challenge.trim()));
        wait(&root, "release");
        note(&root, "released", "released");
        return;
    }
    let child = Command::new(std::env::current_exe().unwrap())
        .env("ATERM_EPOCH_PROBE_MEMBER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(root.join("member.stdout")).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(root.join("member.stderr")).unwrap()))
        .spawn().unwrap();
    wait(&root, "ready");
    assert!(std::fs::read_to_string(root.join("probe.ready")).unwrap().starts_with(&format!("pid={}\n", child.id())));
    if root.join("crash.running").exists() {
        note(&root, "root-ready", "blocked-before-root-finish");
        wait(&root, "root-challenge");
        let challenge = std::fs::read_to_string(root.join("probe.root-challenge")).unwrap();
        note(&root, "root-alive", &format!("challenge={}", challenge.trim()));
        wait(&root, "root-finish");
    }
    drop(child); // The outer native probe owns the actual survivor's release barrier.
    println!("cargo:rerun-if-changed=build.rs");
}
"#.replace("__ROOT__", &root)
    }

    struct Session {
        // Original controller custody drops before its actual leadership fence.
        controller: EpochController,
        pool: RetainedPool,
        wake: ChildWake,
        fence: Fence,
    }

    fn prepare(root: &Path, helper: &Path, jobserver: &Path) -> Result<Session> {
        // Register before any actual helper birth or exit observation.
        let wake = ChildWake::new()?;
        let home = root.join("home");
        private_dir(&home)?;
        let namespace = Namespace::create_private(root, &home)?;
        let ledger = Ledger::at(namespace.fence());
        let machine = ProbeMachine;
        let claim = match Waiter::enter(
            &ledger,
            &machine,
            Need::new("native GNU epoch probe").mem_gib(1),
        )?
        .poll(&machine)
        {
            Polled::Admitted(claim) => claim,
            _ => {
                return Err(io::Error::other("private native probe claim was not admitted").into());
            }
        };
        let fence = Fence::acquire(&namespace, Instant::now() + HANG)?;
        let pool =
            RetainedPool::create_private(&root.join("cpu.noindex"), NonZeroU32::new(2).unwrap())?;
        let mut backing = Vec::new();
        for _ in 0..2 {
            backing.push(
                pool.try_acquire(Owner::current(None))?
                    .ok_or_else(|| io::Error::other("original CPU backing missing"))?,
            );
        }
        let transport = root.join("transport.noindex");
        let mut controller = EpochController::new(
            &fence,
            claim,
            backing,
            Config {
                helper: helper.to_owned(),
                directory: transport.clone(),
                epoch: aterm_uds::rand::hex_token::<16>()?,
                roots: 1,
            },
        )?;
        let project = root.join("project");
        private_dir(&project)?;
        private_dir(&project.join("src"))?;
        fs::write(
            project.join("Cargo.toml"),
            format!(
                "[package]\nname='epoch_probe_native'\nversion='0.0.0'\nedition='2024'\n[workspace]\n[build-dependencies]\njobserver={{path={}}}\n",
                aterm_json::to_string(
                    jobserver
                        .to_str()
                        .ok_or_else(|| io::Error::other("jobserver source path is not UTF-8"))?
                )?
            ),
        )?;
        fs::write(
            project.join("src/main.rs"),
            "fn main() { assert_eq!((0..10).sum::<u64>(), 45); println!(\"native-artifact-ok\"); }\n",
        )?;
        fs::write(project.join("build.rs"), script(root))?;
        controller.launch(&fence)?;
        let deadline = Instant::now() + HANG;
        loop {
            if controller.poll(&fence)?.opened {
                break;
            }
            if Instant::now() >= deadline {
                return Err(
                    io::Error::new(io::ErrorKind::TimedOut, "actual helper did not open").into(),
                );
            }
            wait_epoch(&controller, &wake, deadline)?;
        }
        controller.spawn_root(
            &fence,
            0,
            &project.join("Cargo.toml"),
            &root.join("stable-target.noindex"),
        )?;
        Ok(Session {
            controller,
            pool,
            wake,
            fence,
        })
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum CrashPhase {
        Running,
        Retired,
    }

    impl CrashPhase {
        fn name(self) -> &'static str {
            match self {
                Self::Running => "running",
                Self::Retired => "retired",
            }
        }
        fn parse(value: &std::ffi::OsStr) -> Result<Self> {
            match value.to_str() {
                Some("running") => Ok(Self::Running),
                Some("retired") => Ok(Self::Retired),
                _ => Err(
                    io::Error::new(io::ErrorKind::InvalidInput, "unknown owner crash phase").into(),
                ),
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct OracleScope {
        host: String,
        boot: String,
        namespace: Option<String>,
    }

    impl OracleScope {
        fn read() -> Result<Self> {
            let missing = || io::Error::other("native process scope is unavailable");
            let host = process::host_id()
                .filter(|value| !value.is_empty())
                .ok_or_else(missing)?;
            let boot = process::boot_id()
                .filter(|value| !value.is_empty())
                .ok_or_else(missing)?;
            let namespace = if cfg!(target_os = "linux") {
                Some(process::pid_namespace().ok_or_else(missing)?)
            } else {
                None
            };
            Ok(Self {
                host,
                boot,
                namespace,
            })
        }
    }

    fn note_identity(note: &str) -> Result<(u32, u32)> {
        let mut lines = note.lines();
        let pid = lines
            .next()
            .and_then(|line| line.strip_prefix("pid="))
            .ok_or_else(|| io::Error::other("actual member PID missing"))?
            .parse::<u32>()?;
        let group = lines
            .next()
            .and_then(|line| line.strip_prefix("pgid="))
            .ok_or_else(|| io::Error::other("actual member group missing"))?
            .parse::<u32>()?;
        require(pid > 1 && group > 1, "invalid actual member identity")?;
        Ok((pid, group))
    }

    fn live_identity(pid: u32) -> Result<process::Identity> {
        let pid = i32::try_from(pid)?;
        require(
            process::process_presence(pid) == process::ProcessPresence::Present,
            "actual compiled work is not live",
        )?;
        Ok(process::read_lineage(pid)
            .ok_or_else(|| io::Error::other("actual compiled work identity is unreadable"))?
            .identity)
    }

    fn wait_observed_note(root: &Path, stage: &str) -> Result<String> {
        let deadline = Instant::now() + HANG;
        loop {
            match fs::read_to_string(root.join(format!("probe.{stage}"))) {
                Ok(note) => return Ok(note),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            require(
                Instant::now() < deadline,
                "actual post-crash response did not arrive",
            )?;
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn owner_worker(root: &Path, helper: &Path, jobserver: &Path, phase: CrashPhase) -> Result<()> {
        let mut session = prepare(root, helper, jobserver)?;
        let member = wait_note(root, "ready", &mut session.controller, &session.fence)?;
        let group = session
            .controller
            .helper_id()
            .ok_or_else(|| io::Error::other("actual helper missing"))?;
        require(
            note_identity(&member)?.1 == group,
            "native member escaped its original group",
        )?;
        let root_note = if phase == CrashPhase::Running {
            let note = wait_note(root, "root-ready", &mut session.controller, &session.fence)?;
            require(
                note_identity(&note)?.1 == group,
                "native build script escaped its original group",
            )?;
            require(
                !root.join("probe.root-finish").exists(),
                "root finish barrier was already released",
            )?;
            require(
                session.controller.root_outcome(0).is_none(),
                "running root already completed",
            )?;
            Some(note)
        } else {
            session.controller.close(&session.fence)?;
            let deadline = Instant::now() + HANG;
            loop {
                let status = session.controller.poll(&session.fence)?;
                if status.retired && status.helper_exited && status.control_eof {
                    break;
                }
                require(
                    Instant::now() < deadline,
                    "actual helper retirement did not complete",
                )?;
                wait_epoch(&session.controller, &session.wake, deadline)?;
            }
            require(
                session
                    .controller
                    .root_outcome(0)
                    .is_some_and(|outcome| outcome.success),
                "native root failed",
            )?;
            None
        };
        let deadline = Instant::now() + HANG;
        let driver = loop {
            session.controller.poll(&session.fence)?;
            if let Some(pid) = session.controller.root_pid(0) {
                break pid;
            }
            require(
                Instant::now() < deadline,
                "actual Targo root birth was not observed",
            )?;
            wait_epoch(&session.controller, &session.wake, deadline)?;
        };
        let status = session.controller.status();
        if phase == CrashPhase::Running {
            require(
                !status.retired && !status.helper_exited && !status.control_eof,
                "running helper was already retired",
            )?;
            require(
                process::process_presence(i32::try_from(group)?)
                    == process::ProcessPresence::Present,
                "running helper is not live",
            )?;
        }
        require(
            status.settled_units == 0 && status.remaining_units == 2,
            "owner lost original CPU custody before crash",
        )?;
        require(
            session.pool.try_acquire(Owner::current(None))?.is_none(),
            "owner did not retain all backing",
        )?;
        writeln!(
            io::stdout(),
            "{}",
            aterm_json::json!({
                "v":1,"owner_pid":std::process::id(),"phase":phase.name(),"helper_group":group,"driver_pid":driver,
                "member":member,"root":root_note,"retired":status.retired,"helper_exited":status.helper_exited,
                "control_eof":status.control_eof
            })
        )?;
        io::stdout().flush()?;
        let deadline = Instant::now() + HANG;
        // Keep every original object alive. Only the outer native fixture may
        // kill its actual owned Child; a timer never impersonates owner death.
        loop {
            session.controller.poll(&session.fence)?;
            require(
                Instant::now() < deadline,
                "actual owner was not killed by its fixture",
            )?;
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    struct OwnedOwner {
        child: Child,
        output: std::process::ChildStdout,
    }

    impl OwnedOwner {
        fn spawn(root: &Path, helper: &Path, jobserver: &Path, phase: CrashPhase) -> Result<Self> {
            let mut child = spawnfd::command(std::env::current_exe()?)
                .args(["--targo"])
                .arg(helper)
                .arg("--jobserver-source")
                .arg(jobserver)
                .arg("--owner-worker")
                .arg(root)
                .arg(phase.name())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::from(fs::File::create(root.join("owner.stderr"))?))
                .spawn()?;
            let output = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("actual owner stdout missing"))?;
            let fd = output.as_raw_fd();
            // SAFETY: this is our exclusively held private read end, never an
            // inherited GNU descriptor or terminal description.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                let error = io::Error::last_os_error();
                let _ = child.kill();
                return Err(error.into());
            }
            Ok(Self { child, output })
        }

        fn ready(&mut self, root: &Path) -> Result<aterm_json::Value> {
            let deadline = Instant::now() + HANG;
            let mut incoming = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                match self.output.read(&mut buffer) {
                    Ok(0) => {
                        return Err(io::Error::other("actual owner ended before readiness").into());
                    }
                    Ok(count) => incoming.extend_from_slice(&buffer[..count]),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
                require(
                    incoming.len() <= 16 * 1024,
                    "actual owner output exceeded its frame bound",
                )?;
                if incoming.last() == Some(&b'\n') {
                    fs::write(root.join("owner.stdout"), &incoming)?;
                    return Ok(aterm_json::from_slice(&incoming)?);
                }
                require(
                    Instant::now() < deadline,
                    "actual owner did not become ready",
                )?;
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn kill_and_reap(&mut self) -> Result<u32> {
            let pid = self.child.id();
            require(
                self.child.try_wait()?.is_none(),
                "actual owner exited before SIGKILL",
            )?;
            self.child.kill()?;
            let deadline = Instant::now() + HANG;
            loop {
                if let Some(status) = self.child.try_wait()? {
                    require(
                        status.signal() == Some(libc::SIGKILL),
                        "actual owner did not die by SIGKILL",
                    )?;
                    return Ok(pid);
                }
                require(
                    Instant::now() < deadline,
                    "actual killed owner was not reaped",
                )?;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for OwnedOwner {
        fn drop(&mut self) {
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
            }
            // Native Child Drop retains unreaped children through its reaper.
            let _ = self.child.try_wait();
        }
    }

    fn slot_path(root: &Path, slot: u32) -> PathBuf {
        root.join("cpu.noindex")
            .join(RETAINED_DIRECTORY_NAME)
            .join(format!("slot-{slot:08x}"))
    }

    fn inode(metadata: &fs::Metadata) -> (u64, u64) {
        (metadata.dev(), metadata.ino())
    }

    struct OriginalNode {
        file: fs::File,
        path: PathBuf,
        identity: (u64, u64),
    }

    impl OriginalNode {
        fn fifo(path: PathBuf) -> Result<Self> {
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&path)?;
            let metadata = file.metadata()?;
            require(
                metadata.file_type().is_fifo() && metadata.nlink() == 1,
                "original transport is not a single FIFO",
            )?;
            let node = Self {
                file,
                path,
                identity: inode(&metadata),
            };
            node.check_named()?;
            Ok(node)
        }
        fn check_named(&self) -> Result<()> {
            let held = self.file.metadata()?;
            let named = fs::symlink_metadata(&self.path)?;
            require(
                held.file_type().is_fifo()
                    && named.file_type().is_fifo()
                    && held.nlink() == 1
                    && named.nlink() == 1
                    && inode(&held) == self.identity
                    && inode(&named) == self.identity,
                "original transport changed physical identity",
            )
        }
    }

    // These are independent observational opens AFTER owner birth, not copied
    // lease descriptions. Before SIGKILL each must actually contend with its
    // original owner; after SIGKILL the SAME inode must become kernel-lockable.
    fn original_backing_is_locked(root: &Path, owner: u32) -> Result<Vec<fs::File>> {
        let pool = RetainedPool::open_existing(&root.join("cpu.noindex"))?
            .ok_or_else(|| io::Error::other("original CPU domain missing"))?;
        let states = pool.snapshot()?;
        require(states.len() == 2 && states.iter().all(|state| matches!(state,
            RetainedSlotState::Held { reported_owner: Some(reported), .. } if reported.pid == owner
        )), "CPU backing was not held by the actual owner")?;
        let mut original = Vec::new();
        for slot in 0..2 {
            let path = slot_path(root, slot);
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&path)?;
            let metadata = file.metadata()?;
            require(
                metadata.is_file()
                    && metadata.nlink() == 1
                    && inode(&metadata) == inode(&fs::symlink_metadata(&path)?),
                "original CPU slot changed before crash",
            )?;
            match aterm_uds::file_lock::lock_exclusive_until(
                &file,
                Instant::now(),
                "original CPU slot is held",
            ) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
                Ok(()) => {
                    file.unlock()?;
                    return Err(
                        io::Error::other("CPU kernel lock was free before owner death").into(),
                    );
                }
            }
            original.push(file);
        }
        Ok(original)
    }

    fn debt_after_kernel_release(
        root: &Path,
        owner: u32,
        files: Vec<fs::File>,
    ) -> Result<Vec<(u64, u64)>> {
        require(
            files.len() == 2,
            "two original CPU slot observations are required",
        )?;
        let mut identities = Vec::new();
        for (slot, file) in files.into_iter().enumerate() {
            let metadata = file.metadata()?;
            let named = fs::symlink_metadata(slot_path(root, u32::try_from(slot)?))?;
            require(
                metadata.is_file() && metadata.nlink() == 1 && inode(&metadata) == inode(&named),
                "original CPU slot is not the same single regular file",
            )?;
            aterm_uds::file_lock::lock_exclusive_until(
                &file,
                Instant::now() + HANG,
                "original CPU lock survived owner death",
            )?;
            file.unlock()?;
            identities.push(inode(&metadata));
            drop(file);
        }
        // Both independent probes are unlocked and closed before the fresh API
        // denial; neither a live owner nor a transient inherited lock explains it.
        let pool = RetainedPool::open_existing(&root.join("cpu.noindex"))?
            .ok_or_else(|| io::Error::other("original CPU domain missing"))?;
        let states = pool.snapshot()?;
        require(states.len() == 2 && states.iter().all(|state| matches!(state,
            RetainedSlotState::Retained { reported_owner, .. } if reported_owner.pid == owner
        )), "lost original owner did not leave two retained debt records")?;
        require(
            pool.try_acquire(Owner::current(None))?.is_none(),
            "original lost CPU authority was recreated",
        )?;
        Ok(identities)
    }

    fn qualify_owner_crash(helper: &Path, jobserver: &Path, phase: CrashPhase) -> Result<()> {
        let scope = OracleScope::read()?;
        let mut world = World::new()?;
        let root = world.path().to_owned();
        if phase == CrashPhase::Running {
            fs::write(
                root.join("crash.running"),
                b"keep the actual build script alive\n",
            )?;
        }
        let mut owner = OwnedOwner::spawn(&root, helper, jobserver, phase)?;
        let ready = owner.ready(&root)?;
        let pid = owner.child.id();
        require(
            ready["v"].as_u64() == Some(1)
                && ready["owner_pid"].as_u64() == Some(u64::from(pid))
                && ready["phase"].as_str() == Some(phase.name()),
            "readiness did not come from the actual selected owner",
        )?;
        let group = u32::try_from(
            ready["helper_group"]
                .as_u64()
                .ok_or_else(|| io::Error::other("actual helper group missing"))?,
        )?;
        let member = ready["member"]
            .as_str()
            .ok_or_else(|| io::Error::other("actual member readiness missing"))?;
        let member_identity = note_identity(member)?;
        require(
            member_identity.1 == group && group > 1,
            "original actual member group differs",
        )?;
        let retired = phase == CrashPhase::Retired;
        require(
            ready["retired"].as_bool() == Some(retired)
                && ready["helper_exited"].as_bool() == Some(retired)
                && ready["control_eof"].as_bool() == Some(retired),
            "owner crash point did not match actual helper state",
        )?;
        require(
            !root.join("probe.challenge").exists()
                && !root.join("probe.import").exists()
                && !root.join("probe.root-challenge").exists(),
            "post-death challenges already existed",
        )?;
        let member_birth = live_identity(member_identity.0)?;
        let helper_birth = if retired {
            None
        } else {
            Some(live_identity(group)?)
        };
        let driver_birth = if retired {
            None
        } else {
            let driver = u32::try_from(
                ready["driver_pid"]
                    .as_u64()
                    .ok_or_else(|| io::Error::other("actual Targo root PID missing"))?,
            )?;
            require(
                process::read_lineage(i32::try_from(driver)?)
                    .is_some_and(|lineage| lineage.pgid == group),
                "actual Targo root escaped its original group",
            )?;
            Some(live_identity(driver)?)
        };
        let root_birth = if retired {
            None
        } else {
            let root_identity = note_identity(
                ready["root"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("actual running root is missing"))?,
            )?;
            require(
                root_identity.1 == group,
                "actual running root group differs",
            )?;
            Some(live_identity(root_identity.0)?)
        };
        let original = original_backing_is_locked(&root, pid)?;
        let fifo = OriginalNode::fifo(root.join("transport.noindex/credits.fifo"))?;
        require(
            OracleScope::read()? == scope,
            "native process scope changed before crash",
        )?;
        owner.kill_and_reap()?;
        let challenge = aterm_uds::rand::hex_token::<16>()?;
        let (request, response) = if retired {
            ("import", "imported")
        } else {
            ("challenge", "alive")
        };
        publish_challenge(&root, request, &challenge)?;
        let answer = wait_observed_note(&root, response)?;
        require(
            note_identity(&answer)? == member_identity
                && answer
                    .lines()
                    .any(|line| line == format!("challenge={challenge}")),
            "actual survivor did not answer its fresh post-death challenge",
        )?;
        if retired {
            require(
                answer.ends_with("\nempty=true\n"),
                "retired original FIFO did not import empty",
            )?;
        }
        if !retired {
            let original_root = ready["root"]
                .as_str()
                .ok_or_else(|| io::Error::other("running root readiness missing"))?;
            require(
                !root.join("probe.root-finish").exists(),
                "running root was released before owner death",
            )?;
            let root_challenge = aterm_uds::rand::hex_token::<16>()?;
            publish_challenge(&root, "root-challenge", &root_challenge)?;
            let answer = wait_observed_note(&root, "root-alive")?;
            require(
                note_identity(&answer)? == note_identity(original_root)?
                    && answer
                        .lines()
                        .any(|line| line == format!("challenge={root_challenge}")),
                "actual build script did not survive owner death",
            )?;
        }
        let identities = debt_after_kernel_release(&root, pid, original)?;
        require(
            live_identity(member_identity.0)? == member_birth,
            "actual survivor ended before kernel-release observation",
        )?;
        if let Some(birth) = helper_birth {
            require(
                live_identity(group)? == birth,
                "actual helper ended before kernel-release observation",
            )?;
        }
        if let Some(birth) = root_birth {
            require(
                live_identity(u32::try_from(birth.pid)?)? == birth,
                "actual build script ended before kernel-release observation",
            )?;
        }
        if let Some(birth) = driver_birth {
            require(
                live_identity(u32::try_from(birth.pid)?)? == birth,
                "actual Targo driver ended before kernel-release observation",
            )?;
        }
        fifo.check_named()?;
        fs::write(root.join("probe.release"), b"release known actual member\n")?;
        fs::write(
            root.join("probe.root-finish"),
            b"release known actual build script\n",
        )?;
        let deadline = Instant::now() + HANG;
        loop {
            require(
                OracleScope::read()? == scope,
                "native process scope changed during group observation",
            )?;
            let presence = process::group_presence(group);
            require(
                OracleScope::read()? == scope,
                "native process scope changed across group observation",
            )?;
            if presence == process::ProcessPresence::Absent {
                break;
            }
            require(
                Instant::now() < deadline,
                "original group did not reach conclusive ESRCH",
            )?;
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut original = Vec::new();
        for (slot, identity) in identities.iter().enumerate() {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(slot_path(&root, u32::try_from(slot)?))?;
            require(
                inode(&file.metadata()?) == *identity,
                "original CPU slot changed after group drain",
            )?;
            original.push(file);
        }
        require(
            debt_after_kernel_release(&root, pid, original)? == identities,
            "original CPU observations changed after drain",
        )?;
        fifo.check_named()?;
        let evidence = world.keep_evidence();
        writeln!(
            io::stdout(),
            "{}",
            aterm_json::json!({
                "qualified":"native-targo-owner-sigkill","phase":phase.name(),"owner_pid":pid,"group":group,
                "fresh_survivor_response":true,"original_kernel_locks_free":true,"retained_units_after_crash":2,
                "whole_group_esrch":true,"retained_units_after_drain":2,"original_fifo_still_named":true,"evidence":evidence
            })
        )?;
        Ok(())
    }

    pub fn run() -> Result<()> {
        let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
        if arguments.len() < 4 || arguments[0] != "--targo" || arguments[2] != "--jobserver-source"
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: epoch_probe --targo PATH --jobserver-source PATH [--owner-crash]",
            )
            .into());
        }
        let helper = PathBuf::from(&arguments[1]).canonicalize()?;
        let jobserver = PathBuf::from(&arguments[3]).canonicalize()?;
        require(
            helper.is_file() && jobserver.join("Cargo.toml").is_file(),
            "explicit native toolchain inputs are unavailable",
        )?;
        match &arguments[4..] {
            [] => qualify_success(&helper, &jobserver),
            [flag] if flag == "--owner-crash" => {
                qualify_owner_crash(&helper, &jobserver, CrashPhase::Running)?;
                qualify_owner_crash(&helper, &jobserver, CrashPhase::Retired)
            }
            [flag, root, phase] if flag == "--owner-worker" => owner_worker(
                &PathBuf::from(root).canonicalize()?,
                &helper,
                &jobserver,
                CrashPhase::parse(phase)?,
            ),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid native probe arguments",
            )
            .into()),
        }
    }

    fn qualify_success(helper: &Path, jobserver: &Path) -> Result<()> {
        let mut world = World::new()?;
        let root = world.path().to_owned();
        let mut session = prepare(&root, helper, jobserver)?;
        let Session {
            controller,
            pool,
            wake,
            fence,
        } = &mut session;
        let transport = root.join("transport.noindex");
        let ready = wait_note(&root, "ready", controller, fence)?;
        let group = controller
            .helper_id()
            .ok_or_else(|| io::Error::other("actual helper birth missing"))?;
        require(
            ready.contains(&format!("\npgid={group}\n")),
            "actual survivor escaped the original helper group",
        )?;
        controller.close(fence)?;
        let deadline = Instant::now() + HANG;
        loop {
            let status = controller.poll(fence)?;
            if status.retired && status.helper_exited && status.control_eof {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native helper did not retire and exit",
                )
                .into());
            }
            wait_epoch(controller, wake, deadline)?;
        }
        require(
            controller
                .root_outcome(0)
                .is_some_and(|outcome| outcome.success && outcome.code == Some(0))
                && controller.status().failed_roots == 0,
            "actual Targo root compilation failed despite helper retirement",
        )?;
        require(
            controller.try_finish(fence).is_err(),
            "retirement released CPU while the actual member survived",
        )?;
        require(
            pool.try_acquire(Owner::current(None))?.is_none(),
            "backing returned before actual group drain",
        )?;
        require(
            transport.join("credits.fifo").exists(),
            "FIFO disappeared before actual group drain",
        )?;
        let challenge = aterm_uds::rand::hex_token::<16>()?;
        publish_challenge(&root, "import", &challenge)?;
        let imported = wait_note(&root, "imported", controller, fence)?;
        let identity = ready.lines().take(2).collect::<Vec<_>>();
        require(
            imported.lines().take(2).collect::<Vec<_>>() == identity
                && imported
                    .lines()
                    .any(|line| line == format!("challenge={challenge}"))
                && imported.ends_with("\nempty=true\n"),
            "the original actual member did not import the empty FIFO",
        )?;
        require(
            pool.try_acquire(Owner::current(None))?.is_none(),
            "late FIFO import released CPU backing",
        )?;
        fs::write(root.join("probe.release"), b"actual controller release\n")?;
        wait_note(&root, "released", controller, fence)?;
        let deadline = Instant::now() + HANG;
        loop {
            match controller.try_finish(fence) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "actual cohort never drained and settled",
                )
                .into());
            }
            wait_epoch(controller, wake, deadline)?;
        }
        require(
            controller.is_settled() && controller.status().settled_units == 2,
            "controller did not confirm all original units",
        )?;
        require(
            !transport.join("credits.fifo").exists(),
            "original endpoint was not removed after actual drain",
        )?;
        require(
            pool.snapshot()?
                .iter()
                .all(|slot| matches!(slot, RetainedSlotState::Available { .. })),
            "original CPU budget did not return exactly",
        )?;
        let mut restored = Vec::new();
        for _ in 0..2 {
            restored.push(
                pool.try_acquire(Owner::current(None))?
                    .ok_or_else(|| io::Error::other("restored unit missing"))?,
            );
        }
        require(
            pool.try_acquire(Owner::current(None))?.is_none(),
            "extra CPU capacity was minted",
        )?;
        for capability in restored {
            capability.settle()?;
        }
        execute_artifact(
            &root.join("stable-target.noindex/debug/epoch_probe_native"),
            &root,
        )?;
        writeln!(
            io::stdout(),
            "{}",
            aterm_json::json!({"qualified":"native-targo-epoch-controller","helper":helper,"jobserver_source":jobserver,"retained_while_member_alive":true,"late_original_fifo_imported_empty":true,"endpoint_removed_after_group_drain":true,"restored_units":2,"artifact_executed":true})
        )?;
        world.passed = true;
        Ok(())
    }
}

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::run()
}

#[cfg(not(unix))]
fn main() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "native GNU epoch qualification requires Unix",
    ))
}
