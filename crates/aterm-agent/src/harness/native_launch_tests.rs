// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real one-shot launcher controls, mounted by the native constructor's owner. The ignored
//! helper executes this existing test image in place; it never enters ordinary harness setup.
//! Every world is retained, no executable is copied, and the fake peer has no server thread.

#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, ExitStatus};
use std::time::{Duration, Instant};

use aterm_uds::exitwatch::ExitWatch;
use aterm_uds::spawnfd::{Child, Stdio};

use super::native_act::{
    self, Binding, Image, ImagePhase, Process, Reply, Request, RestartCommand,
};

const HANG: Duration = Duration::from_secs(60);
const BOUND: usize = 64 * 1024;
const SID: &str = "s-native-launch-fixture";
const TOKEN: &str = "private-native-launch-fixture-token";
const CASE_ENV: &str = "ATERM_NATIVE_LAUNCH_FIXTURE_CASE";
const SOCKET_ENV: &str = "ATERM_NATIVE_LAUNCH_FIXTURE_SOCKET";
const HELPER: &str = "harness::native_launch_tests::native_launcher_helper";
const TARGET: &str = "harness::native_launch_tests::native_exec_stdio_target";
const FAST_TARGET: &str = "harness::native_launch_tests::native_exec_fast_target";

struct GuardedWatch(ExitWatch);

impl aterm_uds::spawn_lock::Descriptors for GuardedWatch {
    fn mark(&self) -> io::Result<()> {
        // SAFETY: the watch owns this descriptor for the complete borrowed mark.
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.0.as_raw_fd()) };
        aterm_uds::keep_out_of_children(&fd)
    }
}

fn arm_watch(pid: u32) -> io::Result<ExitWatch> {
    aterm_uds::kept_out(|| {
        ExitWatch::watch(pid)
            .map(GuardedWatch)
            .ok_or_else(|| io::Error::other("watch not armed"))
    })
    .map(|guarded| guarded.0)
}

fn binding() -> Binding {
    Binding {
        id: "18181818181818181818181818181818".into(),
        capability: "6262626262626262626262626262626262626262626262626262626262626262".into(),
    }
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "one-minute fixture hang detector")
}

/// The same original deadline covers retries, including EINTR and spurious readiness.
fn readable(fd: i32, events: i16, until: Instant) -> io::Result<()> {
    loop {
        let left = until
            .checked_duration_since(Instant::now())
            .ok_or_else(timed_out)?;
        let millis = i32::try_from(left.as_millis().saturating_add(1)).unwrap_or(i32::MAX);
        let mut slot = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        // SAFETY: one initialized slot lives for the bounded call; no descriptor ownership moves.
        let ready = unsafe { libc::poll(&mut slot, 1, millis) };
        if ready > 0 {
            if slot.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            return Ok(());
        }
        if ready == 0 {
            return Err(timed_out());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn line<R: Read + AsRawFd>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    until: Instant,
) -> io::Result<String> {
    loop {
        if let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            return String::from_utf8(buffer.drain(..=end).collect())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
        if buffer.len() >= BOUND {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture line bound",
            ));
        }
        readable(reader.as_raw_fd(), libc::POLLIN, until)?;
        let mut bytes = [0; 1024];
        let remaining = bytes.len().min(BOUND - buffer.len());
        match reader.read(&mut bytes[..remaining]) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(count) => buffer.extend_from_slice(&bytes[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
}

struct Peer {
    stream: UnixStream,
    buffer: Vec<u8>,
}

impl Peer {
    fn request(&mut self, until: Instant) -> io::Result<Request> {
        let text = line(&mut self.stream, &mut self.buffer, until)?;
        let tail = text
            .trim_end_matches('\n')
            .strip_prefix(&format!("@{SID} native-act "))
            .ok_or_else(|| io::Error::other("wrong session or verb"))?;
        Request::parse(tail).ok_or_else(|| io::Error::other("malformed native request"))
    }

    fn write(&mut self, bytes: &[u8], until: Instant) -> io::Result<()> {
        if bytes.len() > BOUND {
            return Err(io::Error::other("fixture reply bound"));
        }
        let mut at = 0;
        while at < bytes.len() {
            readable(self.stream.as_raw_fd(), libc::POLLOUT, until)?;
            match self.stream.write(&bytes[at..]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                Ok(count) => at += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn eof(&mut self, until: Instant) -> io::Result<()> {
        if !self.buffer.is_empty() {
            return Err(io::Error::other("unexpected buffered request"));
        }
        loop {
            readable(self.stream.as_raw_fd(), libc::POLLIN, until)?;
            let mut byte = [0];
            match self.stream.read(&mut byte) {
                Ok(0) => return Ok(()),
                Ok(_) => return Err(io::Error::other("unexpected late request")),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
}

/// The controller owns one leaf Child, its wait right, every peer FD, and the retained watch.
/// Closing peer/stdin precedes error cleanup; no cached PGID is ever signalled after a reap.
struct Fixture {
    world: PathBuf,
    listener: Option<UnixListener>,
    peer: Option<Peer>,
    child: Child,
    stdout: ChildStdout,
    output: Vec<u8>,
    owned_birth: Option<u64>,
    captured: Option<Process>,
    watch: Option<ExitWatch>,
    reaped: bool,
}

fn child_wait_policy() {
    // SAFETY: reads this test process's disposition without changing parallel runner policy.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a valid writable action; null replacement only reads the current disposition.
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) },
        0
    );
    // Darwin sys/signal.h: SA_NOCLDWAIT=0x0020. No process-wide policy is changed by a case.
    assert_eq!(
        action.sa_sigaction,
        libc::SIG_DFL,
        "fixture needs exclusive child wait ownership"
    );
    assert_eq!(
        action.sa_flags & 0x0020,
        0,
        "fixture cannot auto-reap children"
    );
}

impl Fixture {
    fn spawn(case: &str, until: Instant) -> io::Result<Self> {
        child_wait_policy();
        let world = aterm_tempfile::tempdir()?.keep();
        eprintln!("native launcher fixture retained at {}", world.display());
        std::fs::create_dir(world.join("home"))?;
        let socket = world.join("launch.sock");
        let token_name = aterm_types::control_socket::token_name_for_sock("launch.sock");
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(world.join(token_name))?
            .write_all(TOKEN.as_bytes())?;
        let listener = aterm_uds::kept_out(|| UnixListener::bind(&socket))?;
        listener.set_nonblocking(true)?;
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe()?);
        let error_log = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(world.join("helper-stderr.txt"))?;
        command
            .args(["--ignored", "--exact", HELPER, "--nocapture"])
            .env_clear()
            .env("HOME", world.join("home"))
            .env("TMPDIR", &world)
            .env("PATH", "/usr/bin:/bin")
            .env("USER", "native-launch-fixture")
            .env("LOGNAME", "native-launch-fixture")
            .current_dir(&world)
            .env(CASE_ENV, case)
            .env(SOCKET_ENV, socket.as_os_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(error_log)
            .process_group(0);
        let mut child = command.spawn()?;
        let owned_birth = aterm_uds::process::read_birth(child.id() as i32).map(|birth| birth.0);
        let stdout = child.stdout.take().expect("requested stdout pipe");
        let mut fixture = Self {
            world,
            listener: Some(listener),
            peer: None,
            child,
            stdout,
            output: Vec::new(),
            owned_birth,
            captured: None,
            watch: None,
            reaped: false,
        };
        fixture.accept(until)?;
        fixture.body("launcher-body", until)?;
        Ok(fixture)
    }

    fn accept(&mut self, until: Instant) -> io::Result<()> {
        let listener = self.listener.as_ref().expect("listener");
        let stream = loop {
            readable(listener.as_raw_fd(), libc::POLLIN, until)?;
            match aterm_uds::kept_out(|| listener.accept()) {
                Ok((stream, _)) => break stream,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        };
        // Darwin can inherit the listener's O_NONBLOCK: all fake-peer I/O deliberately uses poll.
        stream.set_nonblocking(true)?;
        let peer = aterm_uds::peer::peer_audit_token(&stream)
            .ok_or_else(|| io::Error::other("no kernel peer"))?;
        if peer.pid() != self.child.id() {
            return Err(io::Error::other("wrong helper peer"));
        }
        let mut wire = Peer {
            stream,
            buffer: Vec::new(),
        };
        if line(&mut wire.stream, &mut wire.buffer, until)? != format!("AUTH {TOKEN}\n") {
            return Err(io::Error::other("wrong AUTH"));
        }
        assert_eq!(
            wire.request(until)?,
            Request::Attach(binding(), ImagePhase::Forward)
        );
        let captured =
            Process::observe(self.child.id()).ok_or_else(|| io::Error::other("unread helper"))?;
        if captured.parent != std::process::id()
            || captured.group != self.child.id()
            || captured.image
                != Image::at(&std::env::current_exe()?)
                    .ok_or_else(|| io::Error::other("test image"))?
        {
            return Err(io::Error::other("helper topology/image mismatch"));
        }
        // No metadata/native predicate exists in the fixture. Arm before Go and bracket birth
        // and the mapped image again; NOTE_EXIT retains even a fast-target/ENOEXEC immediate exit.
        let watch = arm_watch(self.child.id())?;
        // SAFETY: the fixture owns this exact watch; F_GETFD changes no descriptor flags.
        let flags = unsafe { libc::fcntl(watch.as_raw_fd(), libc::F_GETFD) };
        if flags < 0 || flags & libc::FD_CLOEXEC == 0 {
            return Err(io::Error::other("watch must be CLOEXEC"));
        }
        if Process::observe(self.child.id()).as_ref() != Some(&captured)
            || watch.exit_status().is_some()
        {
            return Err(io::Error::other("helper changed while arming watch"));
        }
        let identity = aterm_uds::process::read_identity(self.child.id() as i32)
            .ok_or_else(|| io::Error::other("peer identity"))?;
        assert_eq!(
            identity.idversion.and_then(|v| u32::try_from(v).ok()),
            Some(peer.pid_version())
        );
        std::fs::write(
            self.world.join("captured.txt"),
            format!("{captured:#?}\npeer={peer:?}\n"),
        )?;
        self.captured = Some(captured);
        self.owned_birth = self.captured.as_ref().map(|process| process.birth);
        self.watch = Some(watch);
        self.peer = Some(wire);
        Ok(())
    }

    fn body(&mut self, marker: &str, until: Instant) -> io::Result<()> {
        let expected = format!("{marker} pid={}\n", self.child.id());
        let mut total = 0;
        loop {
            let got = line(&mut self.stdout, &mut self.output, until)?;
            total += got.len();
            if total > BOUND {
                return Err(io::Error::other("helper stdout bound"));
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(self.world.join("helper-stdout.txt"))?
                .write_all(got.as_bytes())?;
            if got == expected {
                return Ok(());
            }
        }
    }

    fn wait(&mut self, until: Instant) -> io::Result<ExitStatus> {
        let watch = self
            .watch
            .as_ref()
            .ok_or_else(|| io::Error::other("missing retained watch"))?;
        readable(watch.as_raw_fd(), libc::POLLIN, until)?;
        let observed = watch
            .exit_status()
            .ok_or_else(|| io::Error::other("no retained exit status"))?;
        // The exact retained watch reports exit; no generic wait/reaper or other child's PID.
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            if Instant::now() >= until {
                return Err(timed_out());
            }
            std::thread::park_timeout(Duration::from_millis(10));
        };
        self.reaped = true;
        assert_eq!(observed, status.into_raw());
        std::fs::write(
            self.world.join("outcome.txt"),
            format!("status={status}\nraw={observed}\nreaped=true\n"),
        )?;
        Ok(status)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.peer.take());
        drop(self.listener.take());
        drop(self.child.stdin.take());
        if self.reaped {
            return;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {
                self.reaped = true;
                return;
            }
            Err(error) => {
                eprintln!("{}: child wait unknown: {error}", self.world.display());
                return;
            }
            Ok(None) => {}
        }
        match self.child.exited() {
            Ok(false) => {}
            Ok(true) => {
                match self.child.try_wait() {
                    Ok(Some(_)) => self.reaped = true,
                    other => eprintln!(
                        "{}: exited child wait unknown: {other:?}",
                        self.world.display()
                    ),
                }
                return;
            }
            Err(error) => {
                eprintln!("{}: child ownership unknown: {error}", self.world.display());
                return;
            }
        }
        // Even an error before Attach owns this exact unreaped Child. Prearm outside any
        // predicate before cleanup; if arming fails, EOF still releases its finite reads.
        let watch = self
            .watch
            .take()
            .or_else(|| arm_watch(self.child.id()).ok());
        // No group kill and no post-reap signal. These private helper/fast/stdio targets never
        // spawns children. Only the still-owned child's freshly matching birth and parent
        // allow its Child handle to kill; unread/reused identities remain untouched.
        if let Some(now) = Process::observe(self.child.id()) {
            let same = self.captured.as_ref().is_none_or(|old| {
                old.pid == now.pid
                    && old.birth == now.birth
                    && old.group == now.group
                    && old.session == now.session
            });
            if same
                && self.owned_birth == Some(now.birth)
                && now.parent == std::process::id()
                && now.group == self.child.id()
            {
                let _killed = self.child.kill();
            }
        }
        let until = Instant::now() + HANG;
        if let Some(watch) = watch.as_ref() {
            let _exit = readable(watch.as_raw_fd(), libc::POLLIN, until);
        }
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.reaped = true;
                    break;
                }
                Err(error) => {
                    eprintln!("{}: cleanup wait unknown: {error}", self.world.display());
                    break;
                }
                Ok(None) if Instant::now() < until => {
                    // Only cleanup retry cadence, never a latency assertion. A failed watch
                    // must not turn the exact Child wait into an unbounded wait or busy loop.
                    std::thread::park_timeout(Duration::from_millis(10));
                }
                Ok(None) => {
                    eprintln!(
                        "{}: retained child {} cleanup timed out",
                        self.world.display(),
                        self.child.id()
                    );
                    break;
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Case {
    FastExit,
    Plaintext,
    Denied,
    LostAck,
    Stdio,
}

impl Case {
    const fn word(self) -> &'static str {
        match self {
            Self::FastExit => "fast-exit",
            Self::Plaintext => "plaintext",
            Self::Denied => "denied",
            Self::LostAck => "lost-ack",
            Self::Stdio => "stdio",
        }
    }
}

fn selected(case: Case, world: &Path) -> io::Result<RestartCommand> {
    let (program, argv) = match case {
        Case::Plaintext => {
            let data = world.join("not-an-executable.txt");
            // Deliberately executable plain data: no shebang, no shell syntax or fallback.
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o700)
                .open(&data)?
                .write_all(b"plain non-program fixture\n")?;
            (data, Vec::new())
        }
        Case::Stdio => (
            std::env::current_exe()?,
            vec![
                "--ignored".into(),
                "--exact".into(),
                TARGET.into(),
                "--nocapture".into(),
            ],
        ),
        Case::FastExit | Case::Denied | Case::LostAck => (
            std::env::current_exe()?,
            vec![
                "--ignored".into(),
                "--exact".into(),
                FAST_TARGET.into(),
                "--nocapture".into(),
            ],
        ),
    };
    let image = Image::at(&program).ok_or_else(|| io::Error::other("selected image unreadable"))?;
    Ok(RestartCommand {
        image,
        program: program
            .to_str()
            .ok_or_else(|| io::Error::other("fixture path UTF-8"))?
            .into(),
        argv,
        env: Vec::new(),
        unset: None,
        cwd: None,
        heal: None,
        dialect: super::upgrade::Dialect::Zsh,
    })
}

fn exercise(case: Case) -> io::Result<()> {
    let until = Instant::now() + HANG;
    let native_until = native_act::monotonic_us()
        .ok_or_else(|| io::Error::other("monotonic clock"))?
        .checked_add(60_000_000)
        .ok_or_else(|| io::Error::other("clock overflow"))?;
    let mut fixture = Fixture::spawn(case.word(), until)?;
    let command = selected(case, &fixture.world)?;
    let encoded = command
        .encode()
        .ok_or_else(|| io::Error::other("selected command codec"))?;
    let response = format!(
        "OK native-act state=launch deadline-us={native_until} command={}\n",
        aterm_codec::hex::encode_lower(encoded.as_bytes())
    );
    assert_eq!(
        Reply::parse(&response),
        Some(Reply::Launch {
            command,
            deadline_us: native_until
        })
    );
    fixture
        .peer
        .as_mut()
        .expect("authenticated peer")
        .write(response.as_bytes(), until)?;
    assert_eq!(
        fixture.peer.as_mut().expect("peer").request(until)?,
        Request::Execute(binding(), ImagePhase::Forward)
    );
    assert_eq!(
        fixture.watch.as_ref().expect("pre-Go watch").exit_status(),
        None
    );
    assert_eq!(
        Process::observe(fixture.child.id()).as_ref(),
        fixture.captured.as_ref()
    );
    let original_version = aterm_uds::process::read_identity(fixture.child.id() as i32)
        .and_then(|identity| identity.idversion)
        .ok_or_else(|| io::Error::other("pre-exec pid version"))?;
    match case {
        Case::Denied => {
            fixture
                .peer
                .as_mut()
                .expect("peer")
                .write(b"ERR denied native exec\n", until)?;
            fixture.peer.as_mut().expect("peer").eof(until)?;
        }
        Case::LostAck => {
            // The host accepted Execute, but its answer ends partway through the frame.
            // Without a complete exact ACK, the launcher must never dispatch the fixed target.
            fixture
                .peer
                .as_mut()
                .expect("peer")
                .write(b"OK native-act state=execute deadline-us=", until)?;
            drop(fixture.peer.take());
        }
        Case::FastExit | Case::Plaintext | Case::Stdio => {
            let go = format!("OK native-act state=execute deadline-us={native_until}\n");
            fixture
                .peer
                .as_mut()
                .expect("peer")
                .write(go.as_bytes(), until)?;
            match case {
                Case::Plaintext => {
                    assert_eq!(
                        fixture.peer.as_mut().expect("peer").request(until)?,
                        Request::ExecFailed(binding(), ImagePhase::Forward, libc::ENOEXEC)
                    );
                    fixture
                        .peer
                        .as_mut()
                        .expect("peer")
                        .write(b"OK native-act state=exec-failed\n", until)?;
                    fixture.peer.as_mut().expect("peer").eof(until)?;
                }
                Case::Stdio => {
                    fixture.body("exec-stdio-body", until)?;
                    // The target is still blocked on inherited stdin. EOF now proves the
                    // operation socket was CLOEXEC, independently of eventual process exit.
                    fixture.peer.as_mut().expect("peer").eof(until)?;
                    assert!(!fixture.child.exited()?);
                    assert_eq!(
                        Process::observe(fixture.child.id()).as_ref(),
                        fixture.captured.as_ref()
                    );
                    let after = aterm_uds::process::read_identity(fixture.child.id() as i32)
                        .and_then(|identity| identity.idversion)
                        .ok_or_else(|| io::Error::other("post-exec pid version"))?;
                    assert_ne!(after, original_version, "a real in-place exec occurred");
                    let input = fixture.child.stdin.as_mut().expect("inherited stdin");
                    readable(input.as_raw_fd(), libc::POLLOUT, until)?;
                    input.write_all(b"finish\n")?;
                }
                Case::FastExit => {
                    // The unique marker comes only from the exec target, not launcher entry.
                    // The watch was armed before Go and retains this rapid exit. This proves
                    // an actual same-image exec, not a transition to a different file image.
                    fixture.body("exec-fast-body", until)?;
                    fixture.peer.as_mut().expect("peer").eof(until)?;
                }
                Case::Denied | Case::LostAck => unreachable!(),
            }
        }
    }
    let status = fixture.wait(until)?;
    let expected = if matches!(case, Case::FastExit | Case::Stdio) {
        0
    } else {
        126
    };
    assert_eq!(
        status.code(),
        Some(expected),
        "{} retained at {}",
        case.word(),
        fixture.world.display()
    );
    // Reap is terminal: no post-reap cleanup may touch this pid/group, even on a later panic.
    assert!(fixture.reaped);
    Ok(())
}

/// Direct entry only: no normal harness setup, account lookup, provider, shell or worker.
#[test]
#[ignore = "invoked only by the bounded one-shot launcher fixture"]
fn native_launcher_helper() {
    if std::env::var_os(CASE_ENV).is_none() {
        return;
    }
    let socket = std::env::var(SOCKET_ENV).expect("private fixture socket");
    println!("\nlauncher-body pid={}", std::process::id());
    std::io::stdout().flush().expect("helper body proof");
    let b = binding();
    let arguments = [
        "native-launch".into(),
        "contract=1".into(),
        OsString::from(format!(
            "socket={}",
            aterm_codec::hex::encode_lower(socket.as_bytes())
        )),
        OsString::from(format!("session={SID}")),
        OsString::from(format!("id={}", b.id)),
        OsString::from(format!("cap={}", b.capability)),
        "phase=forward".into(),
    ];
    assert!(super::native_launch::entry(&arguments).is_some());
    // Returning from entry is always the production constructor's terminal refusal. A
    // successful exec never comes back here; fast/stdio success therefore cannot be faked.
    std::process::exit(126);
}

#[test]
#[ignore = "fixed in-place target of the bounded one-shot launcher fixture"]
fn native_exec_stdio_target() {
    if std::env::var(CASE_ENV).as_deref() != Ok("stdio") {
        return;
    }
    println!("\nexec-stdio-body pid={}", std::process::id());
    std::io::stdout().flush().expect("exec target body proof");
    // The controller's input or EOF releases this leaf. The process is isolated, and its
    // retained parent watch enforces the fixture's one-minute hang detector and cleanup.
    let mut input = [0; 7];
    std::io::stdin()
        .read_exact(&mut input)
        .expect("preserved stdin");
    assert_eq!(&input, b"finish\n");
}

#[test]
#[ignore = "fixed same-image fast target of the bounded one-shot launcher fixture"]
fn native_exec_fast_target() {
    if !matches!(
        std::env::var(CASE_ENV).as_deref(),
        Ok("fast-exit" | "denied" | "lost-ack")
    ) {
        return;
    }
    println!("\nexec-fast-body pid={}", std::process::id());
    std::io::stdout()
        .flush()
        .expect("actual fast target body proof");
    std::process::exit(0);
}

#[test]
fn native_launcher_execs_the_fixed_same_image_target_and_retains_its_fast_exit() {
    exercise(Case::FastExit).expect("same-image fast-exit fixture");
}

#[test]
fn native_launcher_reports_enoexec_without_a_shell_fallback() {
    exercise(Case::Plaintext).expect("ENOEXEC fixture");
}

#[test]
fn native_launcher_denied_go_never_execs() {
    exercise(Case::Denied).expect("denied Go fixture");
}

#[test]
fn native_launcher_lost_go_ack_never_execs() {
    exercise(Case::LostAck).expect("lost Go ACK fixture");
}

#[test]
fn native_launcher_keeps_stdio_identity_and_closes_its_control_fd_at_exec() {
    exercise(Case::Stdio).expect("stdio/CLOEXEC fixture");
}
