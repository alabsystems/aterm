// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Only the outer fixture process owns the build scripts' server endpoints.

use aterm_uds::{ready, spawnfd};
use std::{
    cell::{Cell, RefCell},
    io::{self, Read as _, Write as _},
    os::{
        fd::AsFd as _,
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(super) const SOCKET: &str = "ATPKG_HEADLESS_SESSION_OWNER";
pub(super) const OWNER: &str = "ATPKG_HEADLESS_SESSION_OWNER_ACTOR";
pub(super) const AFTER_OWNER_DEATH: &str = "ATPKG_HEADLESS_SESSION_AFTER_OWNER_DEATH";
pub(super) const PEER_PID: &str = "ATPKG_HEADLESS_SESSION_OWNER_PROGRESS_PID";
pub(super) const FAILED: &str = "fixture-lifetime-worker-failed";
pub(super) const OWNER_CASE: &str =
    "build_admit::tests::host_service::lifetime::owner_actor_if_requested";

/// Socket names stay below macOS's sun_path bound. A panic retains emitted images;
/// no cleanup assumes an interrupted native child finished its first execution.
pub(super) struct World(Option<aterm_tempfile::TempDir>);

impl World {
    pub(super) fn new() -> Self {
        Self(Some(
            aterm_tempfile::Builder::new()
                .prefix("session-")
                .tempdir_in("/tmp")
                .unwrap(),
        ))
    }
    pub(super) fn path(&self) -> &Path {
        self.0.as_ref().unwrap().path()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let kept = self.0.take().unwrap().keep();
            eprintln!("KEPT {}: interrupted host-session fixture", kept.display());
        }
    }
}

/// Retain the actual collected actor result before peer-shutdown assertions can
/// obscure an earlier actor failure. Raw streams are written before the status.
pub(super) fn record_actor_output(world: &Path, output: &std::process::Output) {
    use std::os::unix::process::ExitStatusExt as _;
    let status = aterm_json::json!({
        "raw": output.status.into_raw(),
        "display": output.status.to_string(),
        "debug": format!("{:?}", output.status),
        "code": output.status.code(),
        "signal": output.status.signal(),
        "success": output.status.success(),
        "stdout_bytes": output.stdout.len(),
        "stderr_bytes": output.stderr.len(),
    });
    let status = format!("{status}\n");
    for (name, bytes) in [
        ("actor.stdout", output.stdout.as_slice()),
        ("actor.stderr", output.stderr.as_slice()),
        ("actor.status.json", status.as_bytes()),
    ] {
        let path = world.join(name);
        std::fs::write(&path, bytes).unwrap_or_else(|error| {
            panic!(
                "retaining native actor output at {}: {error}; actual status {}:\nstdout:\n{}\nstderr:\n{}",
                path.display(),
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        });
    }
}

type Scripts = Arc<Mutex<Vec<(UnixStream, bool)>>>;

pub(super) struct Owner {
    // The main fixture owns this Arc independently of worker unwinding. No ready
    // script loses its final endpoint merely because the I/O worker failed.
    _held: Scripts,
    stop: Arc<ready::Doorbell>,
    worker: Option<std::thread::JoinHandle<io::Result<()>>>,
}

impl Owner {
    pub(super) fn start(socket: &Path) -> Self {
        let listener = aterm_uds::kept_out(|| UnixListener::bind(socket)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(ready::Doorbell::new().unwrap());
        let wake = Arc::clone(&stop);
        let held = Arc::new(Mutex::new(Vec::new()));
        let scripts = Arc::clone(&held);
        let worker = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                serve(listener, &wake, &scripts)
            }));
            match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => {
                    failed();
                    Err(error)
                }
                Err(panic) => {
                    failed();
                    std::panic::resume_unwind(panic)
                }
            }
        });
        Self {
            _held: held,
            stop,
            worker: Some(worker),
        }
    }
    pub(super) fn finish(mut self) {
        self.stop.ring();
        self.worker.take().unwrap().join().unwrap().unwrap();
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.stop.ring();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn failed() {
    // A bounded marker goes to the observer's still-open pipe before this worker
    // returns/unwinds. The observer reads it after actually reaping the owner.
    // libtest's pretty runner leaves its test-name prefix without a newline.
    // One short write frames the reserved marker independently of that prefix.
    std::io::stdout()
        .lock()
        .write_all(format!("\n{FAILED}\n").as_bytes())
        .unwrap();
}

/// B registers a build script; P proves both held scripts still answer after clients
/// disconnect; R explicitly releases them. Closing a control connection has no effect.
fn serve(listener: UnixListener, stop: &ready::Doorbell, held: &Scripts) -> io::Result<()> {
    let mut pending = Vec::<UnixStream>::new();
    let mut scripts = held.lock().unwrap();
    let mut probe = None::<UnixStream>;
    loop {
        let mut fds = vec![listener.as_fd(), stop.fd()];
        fds.extend(pending.iter().map(UnixStream::as_fd));
        if probe.is_some() {
            fds.extend(
                scripts
                    .iter()
                    .filter_map(|(stream, awaiting)| awaiting.then_some(stream.as_fd())),
            );
        }
        ready::any(&fds, None)?;
        drop(fds);
        if stop.clear() {
            return Ok(());
        }
        loop {
            match aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Nonblocking,
            ) {
                Ok((stream, _)) => {
                    assert!(pending.len() < 8, "only this fixture's bounded peers");
                    pending.push(stream);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        for index in (0..pending.len()).rev() {
            let mut byte = [0];
            match pending[index].read(&mut byte) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) => return Err(error),
                Ok(0) => {
                    pending.swap_remove(index);
                }
                Ok(_) => {
                    let mut stream = pending.swap_remove(index);
                    match byte[0] {
                        b'B' => {
                            assert!(scripts.len() < 2);
                            // Retain in main-owned custody BEFORE acknowledging readiness.
                            scripts.push((stream, false));
                            scripts.last_mut().unwrap().0.write_all(b"A")?;
                        }
                        b'P' => {
                            assert!(probe.is_none());
                            assert_eq!(scripts.len(), 2);
                            for (script, awaiting) in scripts.iter_mut() {
                                script.write_all(b"?")?;
                                *awaiting = true;
                            }
                            probe = Some(stream);
                        }
                        b'R' => {
                            assert!(probe.is_none());
                            assert_eq!(scripts.len(), 2);
                            for (script, _) in scripts.iter_mut() {
                                script.write_all(b"R")?;
                            }
                            scripts.clear();
                            let _ = stream.write_all(b"A");
                        }
                        _ => return Err(io::Error::other("unknown fixture lifetime request")),
                    }
                }
            }
        }
        if probe.is_some() {
            for (stream, awaiting) in scripts.iter_mut() {
                if !*awaiting {
                    continue;
                }
                let mut byte = [0];
                match stream.read(&mut byte) {
                    Ok(1) if byte[0] == b'A' => *awaiting = false,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error),
                    _ => return Err(io::Error::other("build script left its lifetime barrier")),
                }
            }
            assert_eq!(scripts.len(), 2, "a probe acknowledges both actual scripts");
            if scripts.iter().all(|(_, awaiting)| !*awaiting) {
                let _ = probe.take().unwrap().write_all(b"A");
            }
        }
    }
}

pub(super) fn disconnect(socket: &Path) {
    drop(aterm_uds::kept_out(|| UnixStream::connect(socket)).unwrap());
}

pub(super) fn request(socket: &Path, request: u8) {
    let mut stream = aterm_uds::kept_out(|| UnixStream::connect(socket)).unwrap();
    stream.write_all(&[request]).unwrap();
    let mut ack = [0];
    stream.read_exact(&mut ack).unwrap();
    assert_eq!(ack, *b"A");
}

/// Server FDs originate only after this exec. Its observer sibling cannot keep a
/// hidden copy alive; SIGKILL closes them without running Rust cleanup.
#[test]
fn owner_actor_if_requested() {
    let Some(world) = std::env::var_os(OWNER) else {
        return;
    };
    let world = PathBuf::from(world);
    let socket = PathBuf::from(std::env::var_os(SOCKET).unwrap());
    let owner = Owner::start(&socket);
    std::fs::write(world.join("owner-ready"), b"lifetime socket bound").unwrap();
    let mut end = Vec::new();
    std::io::stdin().read_to_end(&mut end).unwrap();
    owner.finish();
}

pub(super) struct Child(pub(super) spawnfd::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Observe exactly the native actor's work and its still-owned lifetime peer.
/// No observer CPU, output byte counter or unrelated libtest child advances this clock.
struct Work {
    actor: aterm_uds::deadline::ProcessProgressProbe,
    peer: aterm_uds::deadline::ProcessProgressProbe,
    began: Instant,
    last: Instant,
    sampled: Option<Instant>,
    seen: Option<(Option<u32>, u64, u64)>,
}

impl Work {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            actor: Default::default(),
            peer: Default::default(),
            began: now,
            last: now,
            sampled: None,
            seen: None,
        }
    }

    fn observe(&mut self, root: u32, descendants: bool, peer: Option<u32>, what: &str) {
        let now = Instant::now();
        assert!(
            now.duration_since(self.began) < aterm_spec::wait::WORKING_GUARD,
            "native work exceeded its overall guard while waiting for {what}",
        );
        if self
            .sampled
            .is_some_and(|at| now.duration_since(at) < Duration::from_millis(500))
            && now.duration_since(self.last) < aterm_spec::wait::MUST_HAPPEN
        {
            return;
        }
        self.sampled = Some(now);
        let actor = if descendants {
            self.actor.read_descendants(root)
        } else {
            self.actor.read(root)
        };
        let owner = peer.map_or(Some(0), |pid| self.peer.read(pid));
        let reading = actor.zip(owner).map(|(actor, owner)| (peer, actor, owner));
        // As in the shared progress collectors, an unavailable/incomparable
        // process read is not evidence that the required processes stalled.
        match reading {
            Some(reading) if self.seen == Some(reading) => assert!(
                now.duration_since(self.last) < aterm_spec::wait::MUST_HAPPEN,
                "native actor/owner made no CPU or I/O progress while waiting for {what}",
            ),
            _ => {
                self.seen = reading;
                self.last = now;
            }
        }
    }
}

/// The actor's registration wait also depends on the sibling owner responding.
pub(super) fn until_ready<T>(peer: u32, mut found: impl FnMut() -> Option<T>) -> T {
    let mut work = Work::new();
    loop {
        if let Some(found) = found() {
            return found;
        }
        work.observe(
            std::process::id(),
            true,
            Some(peer),
            "native script registration",
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Existing streaming capture owns/reaps A. Only its actual spawned PID and the
/// caller's still-owned P participate in inactivity detection. The peer callback
/// stops naming P immediately after its actual SIGKILL/wait, before PID reuse.
pub(super) fn capture(
    command: &mut spawnfd::Command,
    mut peer: impl FnMut() -> Option<u32>,
) -> std::process::Output {
    let actor = Cell::new(0);
    let exited = Cell::new(None::<Instant>);
    let output = RefCell::new((Vec::new(), Vec::new()));
    let mut work = Work::new();
    let status = aterm_uds::deadline::stream_until_stopped_observing_exit(
        command,
        None,
        |pid| actor.set(pid),
        |stream, bytes| {
            let mut output = output.borrow_mut();
            let target = match stream {
                aterm_uds::deadline::ChildOutput::Stdout => &mut output.0,
                aterm_uds::deadline::ChildOutput::Stderr => &mut output.1,
            };
            if target.len().saturating_add(bytes.len()) > 16 * 1024 * 1024 {
                return Err(io::Error::other(
                    "native actor diagnostics exceeded their bound",
                ));
            }
            target.extend_from_slice(bytes);
            Ok(())
        },
        || {
            let peer = peer();
            if let Some(exited) = exited.get() {
                // A's compiler/monitor children have closed or redirected their
                // stdio. After natural A exit, no legitimate A-output writer works.
                assert!(
                    exited.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                    "naturally exited native actor did not close its output",
                );
            } else if actor.get() != 0 {
                work.observe(actor.get(), false, peer, "native actor and lifetime owner");
            }
            false
        },
        |_| {
            if exited.get().is_none() {
                exited.set(Some(Instant::now()));
            }
        },
    )
    .expect("native actor output and owned retirement");
    let (stdout, stderr) = output.into_inner();
    std::process::Output {
        status: status.expect("capture never requests cancellation"),
        stdout,
        stderr,
    }
}

/// Both ordinary and SIGKILL cases use an actual peer process, so server work is
/// observable separately from H's polling and from other tests in H's process.
pub(super) struct Peer {
    child: Child,
    input: Option<std::process::ChildStdin>,
    health: Option<std::process::ChildStdout>,
    kill_requested: bool,
    kill_reaped: bool,
}

impl Peer {
    pub(super) fn start(world: &Path, socket: &Path) -> Self {
        use spawnfd::Stdio;
        let mut command = spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", OWNER_CASE, "--test-threads=1", "--nocapture"])
            .env(OWNER, world)
            .env(SOCKET, socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                std::fs::File::create(world.join("owner.log")).unwrap(),
            ));
        let mut child = Child(command.spawn().unwrap());
        let input = child.0.stdin.take();
        let health = child.0.stdout.take();
        aterm_spec::wait::until_while_working(
            child.0.id(),
            "lifetime owner bound its socket",
            || {
                assert!(child.0.try_wait().unwrap().is_none());
                world.join("owner-ready").is_file().then_some(())
            },
        );
        Self {
            child,
            input,
            health,
            kill_requested: false,
            kill_reaped: false,
        }
    }

    pub(super) fn id(&self) -> u32 {
        self.child.0.id()
    }

    /// One prompt collector transition: signal at most once and never wait here.
    /// Keep observing this owned PID until try_wait actually reaps it; after that
    /// return None immediately, before the collector could sample a reused PID.
    pub(super) fn poll_kill(&mut self, request: bool) -> Option<u32> {
        use std::os::unix::process::ExitStatusExt as _;
        if self.kill_reaped {
            return None;
        }
        if let Some(status) = self.child.0.try_wait().unwrap() {
            assert!(
                self.kill_requested,
                "owner ended before its requested SIGKILL"
            );
            assert_eq!(status.signal(), Some(libc::SIGKILL));
            self.kill_reaped = true;
            return None;
        }
        if request && !self.kill_requested {
            self.child.0.kill().unwrap();
            self.kill_requested = true;
        }
        Some(self.id())
    }

    /// Finish only after A's output collector has returned and retired A.
    pub(super) fn finish_kill(&mut self) {
        use std::os::unix::process::ExitStatusExt as _;
        assert!(
            self.kill_requested,
            "both live scripts reached their post-disconnect barriers"
        );
        if !self.kill_reaped {
            let status = aterm_spec::wait::until_while_working(
                self.id(),
                "owned lifetime owner SIGKILL reap",
                || self.child.0.try_wait().unwrap(),
            );
            assert_eq!(status.signal(), Some(libc::SIGKILL));
            self.kill_reaped = true;
        }
    }

    pub(super) fn finish(mut self) {
        drop(self.input.take());
        let status = aterm_spec::wait::until_while_working(
            self.id(),
            "ordinary lifetime owner shutdown",
            || self.child.0.try_wait().unwrap(),
        );
        assert!(status.success(), "ordinary lifetime owner failed: {status}");
        self.assert_health();
    }

    /// Call only after P was reaped AND A capture returned with its owning wait.
    pub(super) fn assert_health(&mut self) {
        let mut health = self.health.take().unwrap();
        let mut record = Vec::new();
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            ready::until(health.as_fd(), ready::Interest::Read, deadline)
                .expect("the reaped owner's health pipe reached EOF");
            let mut bytes = [0; 1024];
            match health.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => record.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("owner health pipe: {error}"),
            }
        }
        let record = String::from_utf8(record).unwrap();
        assert!(
            !record.contains(FAILED),
            "owner worker failed before process death: {record}"
        );
    }
}
