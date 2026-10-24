// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The serve loop: one thread per command connection, each carrying one request.
//!
//! An `Exec` runs as the [`System`]'s identity, in its `cwd` (entered as that identity)
//! with exactly its `env`, in a process group of its own. Its output streams back as
//! `Out`/`Err` frames (one pipe for both under `merge`, in the guest's order), and one
//! `Exit` ends the connection. A connection the host drops before that `Exit` SIGKILLs
//! the command's group.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use crate::sys;
use crate::system::{Identity, System, needs_step};
use crate::wire::{self, Exec, Exit, Frame, Stdin};

/// One connection's byte stream, read and written from two threads.
pub trait Stream: Read + Write + Send + Sized + 'static {
    /// A second handle on the same connection.
    ///
    /// # Errors
    /// The descriptor could not be duplicated.
    fn try_clone(&self) -> io::Result<Self>;

    /// End both directions: every reader of the connection sees EOF.
    fn shutdown(&self);
}

/// Where command connections come from: vsock port 1024 from CID 2 in the guest, a unix
/// socket on the macOS seam.
pub trait Listener {
    /// What it accepts.
    type Stream: Stream;

    /// The next connection from the host.
    ///
    /// # Errors
    /// The listener failed.
    fn accept(&self) -> io::Result<Self::Stream>;
}

impl Stream for std::os::unix::net::UnixStream {
    fn try_clone(&self) -> io::Result<Self> {
        std::os::unix::net::UnixStream::try_clone(self)
    }

    fn shutdown(&self) {
        let _ = std::os::unix::net::UnixStream::shutdown(self, std::net::Shutdown::Both);
    }
}

impl Listener for std::os::unix::net::UnixListener {
    type Stream = std::os::unix::net::UnixStream;

    fn accept(&self) -> io::Result<Self::Stream> {
        std::os::unix::net::UnixListener::accept(self).map(|(s, _)| s)
    }
}

/// The largest `Out`/`Err` frame the agent sends.
const CHUNK: usize = 64 * 1024;

/// The largest amount of stdin waiting for a command, including a blocked write.
pub const MAX_PENDING_STDIN_BYTES: usize = 8 * wire::MAX_FRAME;

/// The largest number of stdin chunks waiting for a command, including a blocked write.
pub const MAX_PENDING_STDIN_CHUNKS: usize = 128;

#[derive(Default)]
struct PendingInput {
    bytes: usize,
    chunks: usize,
}

#[derive(Clone, Default)]
struct InputBudget(Arc<Mutex<PendingInput>>);

impl InputBudget {
    fn admit(&self, bytes: Vec<u8>) -> Result<Option<InputChunk>, ()> {
        if bytes.is_empty() {
            return Ok(None);
        }
        let mut pending = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if bytes.len() > MAX_PENDING_STDIN_BYTES - pending.bytes
            || pending.chunks == MAX_PENDING_STDIN_CHUNKS
        {
            return Err(());
        }
        pending.bytes += bytes.len();
        pending.chunks += 1;
        Ok(Some(InputChunk {
            bytes,
            budget: self.clone(),
        }))
    }
}

/// A stdin chunk, charged to its budget until its write finishes or it is discarded.
struct InputChunk {
    bytes: Vec<u8>,
    budget: InputBudget,
}

impl Drop for InputChunk {
    fn drop(&mut self) {
        let mut pending = self
            .budget
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.bytes -= self.bytes.len();
        pending.chunks -= 1;
    }
}

struct InputQueue {
    sender: mpsc::Sender<InputChunk>,
    budget: InputBudget,
}

impl InputQueue {
    /// Admission never waits for the child pipe; a closed pipe ignores further input.
    fn push(&self, bytes: Vec<u8>) -> Result<(), ()> {
        if let Some(chunk) = self.budget.admit(bytes)? {
            let _ = self.sender.send(chunk);
        }
        Ok(())
    }
}

fn forward_input(mut pipe: impl Write, input: mpsc::Receiver<InputChunk>) {
    for chunk in input {
        if pipe.write_all(&chunk.bytes).is_err() {
            return;
        }
    }
}

// One transition decides whether completion or an input refusal happened first.
const RUNNING: u8 = 0;
const INPUT_OVERFLOW: u8 = 1;
const FINISHED: u8 = 2;

/// Serve `listener` until it fails: each connection on its own thread.
///
/// # Errors
/// The listener failed with something other than an interrupted or aborted accept.
pub fn serve<L: Listener>(listener: &L, system: &Arc<dyn System>) -> io::Result<()> {
    loop {
        let conn = match listener.accept() {
            Ok(conn) => conn,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e),
        };
        let system = Arc::clone(system);
        thread::Builder::new()
            .name("vz-agent command".into())
            .spawn(move || connection(conn, system.as_ref()))?;
    }
}

/// One command connection: its request, then its answer.
pub fn connection<S: Stream>(conn: S, system: &dyn System) {
    let mut reader = match conn.try_clone() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("aterm-vz-agent: a command connection could not be read: {e}");
            return;
        }
    };
    let exit = match wire::read_frame(&mut reader) {
        Ok(Some(Frame::Exec(exec))) => return run(conn, reader, &exec, system),
        Ok(Some(Frame::Mount(shares))) => system.mount(&shares).map(|()| Exit::Exited(0)),
        Ok(Some(Frame::Poweroff)) => system.poweroff().map(|()| Exit::Exited(0)),
        Ok(Some(other)) => Err(format!(
            "frame kind 0x{:02x} is not a request (Exec, Mount or Poweroff)",
            other.kind()
        )),
        Ok(None) => return,
        Err(e) => Err(format!("an unreadable request: {e}")),
    };
    let mut conn = conn;
    let exit = exit.unwrap_or_else(Exit::NotStarted);
    if let Err(e) = wire::write_frame(&mut conn, &Frame::Exit(exit)) {
        eprintln!("aterm-vz-agent: a request's answer was not sent: {e}");
    }
    conn.shutdown();
}

/// The one child constructor (clippy.toml's one-child-constructor ban): std's, because the
/// agent is std-only for the musl guest build.
#[expect(
    clippy::disallowed_types,
    reason = "std-only for the musl guest build: aterm-uds needs aterm-libc, which has no musl cell"
)]
pub fn command(program: &OsStr) -> std::process::Command {
    std::process::Command::new(program)
}

/// A started command and its output's ends.
struct Started {
    child: Child,
    merged: Option<io::PipeReader>,
}

/// Why `exec` did not start, from `spawn`'s error (std reports a failed `chdir`, hook or
/// `execve` through its close-on-exec pipe, never as an exit status). As the lane user a
/// refusal names the `cwd` with the program, since the root agent may see a directory the
/// user cannot enter.
fn not_started(exec: &Exec, identity: &Identity, e: &io::Error) -> String {
    let program = String::from_utf8_lossy(exec.argv.first().map_or(&[][..], Vec::as_slice));
    let cwd = Path::new(OsStr::from_bytes(&exec.cwd));
    match identity {
        Identity::Keep if cwd.is_dir() => format!("{program}: {e}"),
        Identity::Keep => format!("cwd {}: {e}", cwd.display()),
        Identity::Switch { uid, .. } if cwd.is_dir() => {
            format!("{program} in cwd {} (as uid {uid}): {e}", cwd.display())
        }
        Identity::Switch { uid, .. } => format!("cwd {} (as uid {uid}): {e}", cwd.display()),
    }
}

fn start(exec: &Exec, identity: Identity) -> Result<Started, String> {
    use std::os::unix::process::CommandExt as _;
    let Some((program, args)) = exec.argv.split_first() else {
        return Err("an empty argv".into());
    };
    if exec.cwd.is_empty() {
        return Err("no cwd".into());
    }
    let mut cmd = command(OsStr::from_bytes(program));
    cmd.args(args.iter().map(|a| OsStr::from_bytes(a)))
        .env_clear()
        .envs(
            exec.env
                .iter()
                .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
        )
        .process_group(0)
        .stdin(match exec.stdin {
            Stdin::Null => Stdio::null(),
            Stdin::Pipe => Stdio::piped(),
        });
    let merged = if exec.merge {
        let (read, write) = io::pipe().map_err(|e| format!("no pipe for the output: {e}"))?;
        let write2 = write
            .try_clone()
            .map_err(|e| format!("no pipe for the output: {e}"))?;
        cmd.stdout(write).stderr(write2);
        Some(read)
    } else {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        None
    };
    let cwd = OsStr::from_bytes(&exec.cwd);
    match identity {
        Identity::Keep => {
            cmd.current_dir(cwd);
        }
        Identity::Switch { .. } => switch_identity(&mut cmd, &identity, cwd)?,
    }
    let child = cmd.spawn().map_err(|e| not_started(exec, &identity, &e))?;
    // The command holds the merged pipe's write ends; only the child may keep them, so
    // the reader sees EOF when the command's side closes.
    drop(cmd);
    Ok(Started { child, merged })
}

/// Make `cmd`'s child the lane user ([`Identity::Switch`]) in `cwd`, before its exec. The
/// hook enters `cwd` after the switch because std's `current_dir` runs before any
/// `pre_exec` hook, and as root it would enter a directory the user cannot (lima, which
/// `cd`s as the user, refuses those).
#[cfg(target_os = "linux")]
fn switch_identity(
    cmd: &mut impl std::os::unix::process::CommandExt,
    identity: &Identity,
    cwd: &OsStr,
) -> Result<(), String> {
    if let Identity::Switch { uid, gid, groups } = identity.clone() {
        let dir = std::ffi::CString::new(cwd.as_bytes())
            .map_err(|_| format!("cwd {}: holds a NUL byte", cwd.to_string_lossy()))?;
        // SAFETY: the hook runs in the forked child before its exec and calls only
        // `setgroups`, `setgid`, `setuid` and `chdir` (async-signal-safe) on a list and a
        // path built before the fork; it allocates nothing.
        unsafe {
            cmd.pre_exec(move || {
                sys::become_user(uid, gid, &groups)?;
                sys::change_dir(&dir)
            });
        }
    }
    Ok(())
}

/// Off Linux only [`Identity::Keep`] runs: the identity switch is the guest's.
#[cfg(not(target_os = "linux"))]
fn switch_identity(_cmd: &mut impl Sized, identity: &Identity, _cwd: &OsStr) -> Result<(), String> {
    match identity {
        Identity::Keep => Ok(()),
        Identity::Switch { .. } => Err("the identity switch runs only in the Linux guest".into()),
    }
}

/// Send `frame` on the shared writer.
fn send<S: Stream>(writer: &Mutex<S>, frame: &Frame) -> Result<(), wire::WireError> {
    let mut w = writer
        .lock()
        .map_err(|_| wire::WireError::Malformed("a writer poisoned by a panic"))?;
    wire::write_frame(&mut *w, frame)
}

/// Copy `from` to the host as `wrap` frames until EOF. Once the host is gone it keeps
/// reading, so the command never blocks on a full pipe before its group is killed.
fn pump<S: Stream>(
    mut from: impl Read + Send + 'static,
    writer: Arc<Mutex<S>>,
    wrap: fn(Vec<u8>) -> Frame,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("vz-agent output".into())
        .spawn(move || {
            let mut buf = vec![0u8; CHUNK];
            let mut host = true;
            loop {
                match from.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
                        if host {
                            host = send(&writer, &wrap(buf[..n].to_vec())).is_ok();
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return,
                }
            }
        })
}

/// The host's frames during an `Exec`: stdin, its end, and signals for the group. A
/// connection that ends before completion SIGKILLs the group.
fn host_input<S: Stream>(
    mut reader: S,
    mut stdin: Option<InputQueue>,
    group: u32,
    completion: &AtomicU8,
) {
    loop {
        match wire::read_frame(&mut reader) {
            Ok(Some(Frame::Stdin(bytes))) => {
                if stdin
                    .as_ref()
                    .is_some_and(|input| input.push(bytes).is_err())
                {
                    if completion
                        .compare_exchange(
                            RUNNING,
                            INPUT_OVERFLOW,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        let _ = sys::kill_group(group, sys::SIGKILL);
                    }
                    return;
                }
            }
            Ok(Some(Frame::StdinEof)) => stdin = None,
            Ok(Some(Frame::Kill(signal))) => {
                if completion.load(Ordering::SeqCst) != FINISHED
                    && let Err(e) = sys::kill_group(group, signal)
                {
                    eprintln!("aterm-vz-agent: signal {signal} to group {group}: {e}");
                }
            }
            // The connection ended, failed, or carried what no `Exec` takes: the host is
            // gone, or no longer the host this command answers to.
            Ok(Some(_) | None) | Err(_) => {
                if completion.load(Ordering::SeqCst) == RUNNING {
                    let _ = sys::kill_group(group, sys::SIGKILL);
                }
                return;
            }
        }
    }
}

/// Run `exec` and answer on `conn`, reading the host's frames from `reader`.
fn run<S: Stream>(conn: S, reader: S, exec: &Exec, system: &dyn System) {
    if needs_step(system.now_unix_ns(), exec.clock_unix_ns)
        && let Err(why) = system.set_clock(exec.clock_unix_ns)
    {
        eprintln!("aterm-vz-agent: the clock was not stepped to the host's: {why}");
    }
    let writer = Arc::new(Mutex::new(conn));
    let started = system.identity().and_then(|identity| start(exec, identity));
    let Started { mut child, merged } = match started {
        Ok(s) => s,
        Err(why) => {
            let _ = send(&writer, &Frame::Exit(Exit::NotStarted(why)));
            if let Ok(w) = writer.lock() {
                w.shutdown();
            }
            return;
        }
    };
    let group = child.id();
    let completion = Arc::new(AtomicU8::new(RUNNING));
    let exit = (|| -> io::Result<Exit> {
        let stdin = match child.stdin.take() {
            Some(pipe) => {
                let (sender, rx) = mpsc::channel();
                thread::Builder::new()
                    .name("vz-agent stdin".into())
                    .spawn(move || forward_input(pipe, rx))?;
                Some(InputQueue {
                    sender,
                    budget: InputBudget::default(),
                })
            }
            None => None,
        };
        let mut pumps = Vec::new();
        if let Some(out) = merged {
            pumps.push(pump(out, Arc::clone(&writer), Frame::Out)?);
        }
        if let Some(out) = child.stdout.take() {
            pumps.push(pump(out, Arc::clone(&writer), Frame::Out)?);
        }
        if let Some(err) = child.stderr.take() {
            pumps.push(pump(err, Arc::clone(&writer), Frame::Err)?);
        }
        let done = Arc::clone(&completion);
        let input = thread::Builder::new()
            .name("vz-agent input".into())
            .spawn(move || host_input(reader, stdin, group, &done))?;
        let status = child.wait();
        for p in pumps {
            let _ = p.join();
        }
        let mut exit = match status {
            Ok(status) => {
                use std::os::unix::process::ExitStatusExt as _;
                match (status.code(), status.signal()) {
                    (Some(code), _) => Exit::Exited(code),
                    (None, Some(signal)) => Exit::Signaled(signal),
                    (None, None) => Exit::Signaled(0),
                }
            }
            Err(e) => {
                let _ = sys::kill_group(group, sys::SIGKILL);
                let _ = send(
                    &writer,
                    &Frame::Err(format!("aterm-vz-agent: wait failed: {e}\n").into_bytes()),
                );
                Exit::Signaled(sys::SIGKILL)
            }
        };
        if completion.swap(FINISHED, Ordering::SeqCst) == INPUT_OVERFLOW {
            let _ = send(&writer, &Frame::Err(format!(
                "aterm-vz-agent: stdin pending input limit exceeded ({MAX_PENDING_STDIN_BYTES} bytes or \
                 {MAX_PENDING_STDIN_CHUNKS} chunks); command group ended\n"
            ).into_bytes()));
            exit = Exit::Signaled(sys::SIGKILL);
        }
        let _ = send(&writer, &Frame::Exit(exit.clone()));
        if let Ok(w) = writer.lock() {
            w.shutdown();
        }
        let _ = input.join();
        Ok(exit)
    })();
    if let Err(e) = exit {
        // A thread could not start: the command runs on unwatched, so it is ended.
        let _ = sys::kill_group(group, sys::SIGKILL);
        let _ = child.wait();
        let _ = send(
            &writer,
            &Frame::Err(format!("aterm-vz-agent: {e}\n").into_bytes()),
        );
        let _ = send(&writer, &Frame::Exit(Exit::Signaled(sys::SIGKILL)));
        if let Ok(w) = writer.lock() {
            w.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    const HANG: Duration = Duration::from_secs(60);

    /// A child pipe whose first write blocks until the test releases it.
    struct HeldWriter {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        first: bool,
        fail: bool,
        written: Arc<AtomicUsize>,
    }

    impl Write for HeldWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.first {
                self.first = false;
                let _ = self.entered.send(());
                self.release.recv().map_err(|_| io::ErrorKind::BrokenPipe)?;
            }
            if self.fail {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            self.written.fetch_add(bytes.len(), Ordering::SeqCst);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct HeldInput {
        queue: Option<InputQueue>,
        release: Option<mpsc::Sender<()>>,
        done: mpsc::Receiver<()>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl HeldInput {
        fn finish(&mut self) -> bool {
            drop(self.queue.take());
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
            if self.worker.is_none() {
                return true;
            }
            if matches!(
                self.done.recv_timeout(HANG),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                drop(self.worker.take());
                return false;
            }
            self.worker.take().expect("the worker").join().is_ok()
        }
    }

    impl Drop for HeldInput {
        fn drop(&mut self) {
            // Release on every assertion failure too; do not strand the writer on its gate.
            if !self.finish() {
                eprintln!("hung: the released stdin worker did not finish within {HANG:?}");
            }
        }
    }

    #[test]
    fn a_stalled_input_writer_keeps_both_pending_limits_until_the_write_ends() {
        for (size, chunks, fail) in [
            (
                wire::MAX_FRAME,
                MAX_PENDING_STDIN_BYTES / wire::MAX_FRAME,
                false,
            ),
            (1, MAX_PENDING_STDIN_CHUNKS, false),
            (
                wire::MAX_FRAME,
                MAX_PENDING_STDIN_BYTES / wire::MAX_FRAME,
                true,
            ),
        ] {
            let budget = InputBudget::default();
            let written = Arc::new(AtomicUsize::new(0));
            let (sender, input) = mpsc::channel();
            let (entered, writing) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let (done, ended) = mpsc::channel();
            let pipe = HeldWriter {
                entered,
                release: released,
                first: true,
                fail,
                written: Arc::clone(&written),
            };
            let worker = thread::spawn(move || {
                forward_input(pipe, input);
                let _ = done.send(());
            });
            let mut held = HeldInput {
                queue: Some(InputQueue {
                    sender,
                    budget: budget.clone(),
                }),
                release: Some(release),
                done: ended,
                worker: Some(worker),
            };
            let queue = held.queue.as_ref().unwrap();
            assert!(queue.push(vec![7; size]).is_ok());
            writing
                .recv_timeout(HANG)
                .expect("hung: the worker never entered its write");
            for _ in 1..chunks {
                assert!(queue.push(vec![7; size]).is_ok());
            }
            for _ in 0..3 {
                assert!(
                    queue.push(Vec::new()).is_ok(),
                    "empty input uses no queue entry"
                );
            }
            assert!(
                queue.push(vec![1]).is_err(),
                "the worker-held chunk stays charged"
            );
            {
                let pending = budget.0.lock().unwrap();
                assert_eq!((pending.bytes, pending.chunks), (size * chunks, chunks));
            }
            assert!(held.finish(), "hung: the released worker did not drain");
            let pending = budget.0.lock().unwrap();
            assert_eq!(
                (pending.bytes, pending.chunks),
                (0, 0),
                "all charges released"
            );
            assert_eq!(
                written.load(Ordering::SeqCst),
                if fail { 0 } else { size * chunks }
            );
        }
    }
}
