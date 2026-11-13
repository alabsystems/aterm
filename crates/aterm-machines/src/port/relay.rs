// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::relay`]'s live arm: `screen`'s local end (design §6).

use super::{Cmd, Relaying, RunFailed};
use aterm_uds::spawnfd::{Child, Stdio};
use aterm_uds::{AcceptWait, AcceptedMode};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// How long the connection that wakes the accepting thread may take: `accept` blocks until one arrives, and a listener on loopback answers at once.
const WAKE_BUDGET: Duration = Duration::from_secs(5);

/// A listening relay.
pub(super) struct Live {
    port: u16,
    stop: Arc<AtomicBool>,
    /// The accepting thread; it answers the first failure it met, if any.
    accepter: Option<JoinHandle<Option<String>>>,
    runs: Arc<Mutex<Vec<Child>>>,
    /// The listener, which only the accepting thread owns: gone once that thread has ended, so a test sees the close end it without asking the machine-wide port space, where another process may take the freed port at once.
    #[cfg(test)]
    listener: std::sync::Weak<TcpListener>,
}

/// Listen on a free loopback port and carry each connection over a run of `cmd`, in the
/// cleared environment `env` ([`super::Live`]'s `child_env`, as
/// [`super::Live::run`](super::Live#method.run) gives).
pub(super) fn listen(cmd: &Cmd, env: Vec<(String, String)>) -> Result<Live, RunFailed> {
    if cmd.user.is_some() || cmd.session || cmd.stdin.is_some() || cmd.keep_on_timeout {
        return Err(RunFailed::Refused(format!(
            "{}: a relayed connection is a plain run's standard input and output",
            cmd.display()
        )));
    }
    let failed = |e: io::Error| RunFailed::Spawn(format!("relay: 127.0.0.1: {e}"));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(failed)?;
    let port = listener.local_addr().map_err(failed)?.port();
    let listener = Arc::new(listener);
    #[cfg(test)]
    let watch = Arc::downgrade(&listener);
    let stop = Arc::new(AtomicBool::new(false));
    let runs = Arc::new(Mutex::new(Vec::new()));
    let accepter = {
        let (stop, runs, cmd) = (Arc::clone(&stop), Arc::clone(&runs), cmd.clone());
        std::thread::Builder::new()
            .name("machines-relay".into())
            .spawn(move || accept(&listener, &stop, &runs, &cmd, &env))
            .map_err(failed)?
    };
    Ok(Live {
        port,
        stop,
        accepter: Some(accepter),
        runs,
        #[cfg(test)]
        listener: watch,
    })
}

/// The accepting thread: until `stop`, one run of `cmd` per connection.
fn accept(
    listener: &TcpListener,
    stop: &AtomicBool,
    runs: &Mutex<Vec<Child>>,
    cmd: &Cmd,
    env: &[(String, String)],
) -> Option<String> {
    let mut first_failure = None;
    loop {
        let stream = match aterm_uds::accept(listener, AcceptWait::Forever, AcceptedMode::Blocking)
        {
            Ok((stream, _)) => stream,
            Err(_) if stop.load(Ordering::SeqCst) => return first_failure,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                ) =>
            {
                continue;
            }
            Err(e) => return first_failure.or_else(|| Some(format!("relay: accept: {e}"))),
        };
        if stop.load(Ordering::SeqCst) {
            return first_failure;
        }
        let mut held = runs.lock().unwrap_or_else(PoisonError::into_inner);
        held.retain_mut(|run| matches!(run.try_wait(), Ok(None)));
        match start(cmd, env, stream) {
            Ok(run) => held.push(run),
            Err(e) => {
                first_failure
                    .get_or_insert_with(|| format!("relay: {}: {e}", cmd.program.display()));
            }
        }
    }
}

/// One run of `cmd` carrying `stream`, in `env`.
fn start(cmd: &Cmd, env: &[(String, String)], stream: TcpStream) -> io::Result<Child> {
    let _ = stream.set_nodelay(true);
    let output = stream.try_clone()?;
    let mut child = super::fixed_command(&cmd.program, &cmd.args, env, cmd.cwd.as_deref());
    child
        .stdin(OwnedFd::from(stream))
        .stdout(OwnedFd::from(output))
        .stderr(Stdio::null());
    child.spawn()
}

impl Relaying for Live {
    fn port(&self) -> u16 {
        self.port
    }

    fn close(&mut self) -> Result<(), String> {
        let Some(accepter) = self.accepter.take() else {
            return Ok(());
        };
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
            WAKE_BUDGET,
        );
        let stopped = accepter.join();
        let runs = std::mem::take(&mut *self.runs.lock().unwrap_or_else(PoisonError::into_inner));
        for mut run in runs {
            aterm_uds::deadline::kill_within(&mut run, super::REAP_HANG);
        }
        match stopped {
            Ok(None) => Ok(()),
            Ok(Some(why)) => Err(why),
            Err(_) => Err("relay: the accepting thread panicked".into()),
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
