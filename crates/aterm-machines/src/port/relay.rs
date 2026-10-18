// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::relay`]'s live arm: `screen`'s local end (design §6).
//!
//! A listener on `127.0.0.1:0` and one thread that accepts on it. Each connection becomes one
//! run of the relay's [`Cmd`] (the fleet's `ssh … screen` request), started by
//! `aterm_uds::spawnfd::command` with the connection as its standard input and output; this
//! process keeps no copy of the connection, so the viewer sees it close when the run ends.
//! The runs are kept, a finished one reaped as the next connection arrives; [`Live::close`]
//! stops the accepting thread, then kills and reaps every run still going.

use super::{Cmd, Relaying, RunFailed};
use aterm_uds::spawnfd::{Child, Stdio};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// How long the connection that wakes the accepting thread may take: `accept` blocks until
/// one arrives, and a listener on loopback answers at once.
const WAKE_BUDGET: Duration = Duration::from_secs(5);

/// A listening relay.
pub(super) struct Live {
    port: u16,
    stop: Arc<AtomicBool>,
    /// The accepting thread; it answers the first failure it met, if any.
    accepter: Option<JoinHandle<Option<String>>>,
    runs: Arc<Mutex<Vec<Child>>>,
}

/// Listen on a free loopback port and carry each connection over a run of `cmd`, in the
/// cleared environment `env` ([`super::Live`]'s `child_env`, as [`super::Live::run`] gives).
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
        let stream = match listener.accept() {
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
            // The connection was dropped with the run that never started, so the viewer
            // sees it close; the next one is tried again.
            Err(e) => {
                first_failure
                    .get_or_insert_with(|| format!("relay: {}: {e}", cmd.program.display()));
            }
        }
    }
}

/// One run of `cmd` carrying `stream`, in `env`.
fn start(cmd: &Cmd, env: &[(String, String)], stream: TcpStream) -> io::Result<Child> {
    // Nagle off, as on the host's end (`agent::relay`): the stream is interactive.
    let _ = stream.set_nodelay(true);
    let output = stream.try_clone()?;
    let mut child = super::fixed_command(&cmd.program, &cmd.args, env);
    child
        .stdin(OwnedFd::from(stream))
        .stdout(OwnedFd::from(output))
        .stderr(Stdio::null());
    // `child` (the command) holds this process's copies of the connection until it drops at
    // the end of this function.
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
        // The connection that wakes `accept`; refused when the thread has already stopped.
        let _ = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
            WAKE_BUDGET,
        );
        let stopped = accepter.join();
        // No run starts after the join: the accepting thread is gone.
        let runs = std::mem::take(&mut *self.runs.lock().unwrap_or_else(PoisonError::into_inner));
        for mut run in runs {
            let _ = run.kill();
            let _ = run.wait();
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
mod tests {
    use super::*;
    use crate::port::{Live as Port, System};
    use std::io::{Read, Write};

    /// A connection to the relay with a one-minute hang detector on its reads.
    fn connect(port: u16) -> TcpStream {
        let s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        s
    }

    fn read_exactly(s: &mut TcpStream, n: usize) -> Vec<u8> {
        let mut buf = vec![0; n];
        s.read_exact(&mut buf).unwrap();
        buf
    }

    #[test]
    fn each_connection_is_its_own_run_and_close_ends_every_run() {
        let port = Port::new();
        let mut relay = port.relay(&Cmd::new("/bin/cat")).unwrap();
        let mut one = connect(relay.port());
        let mut two = connect(relay.port());
        one.write_all(b"one\x00\xff").unwrap();
        two.write_all(b"two").unwrap();
        assert_eq!(read_exactly(&mut one, 5), b"one\x00\xff");
        assert_eq!(read_exactly(&mut two, 3), b"two");
        relay.close().unwrap();
        // Both `cat`s still had their input open: only the kill ends them, and with them
        // the connections.
        for s in [&mut one, &mut two] {
            let mut rest = Vec::new();
            assert_eq!(s.read_to_end(&mut rest).unwrap(), 0);
        }
        let port_after = relay.port();
        assert!(
            TcpStream::connect((Ipv4Addr::LOCALHOST, port_after)).is_err(),
            "nothing listens after the close"
        );
        relay.close().unwrap();
    }

    #[test]
    fn a_run_that_ends_closes_its_connection_because_the_relay_keeps_no_copy() {
        let port = Port::new();
        let mut relay = port
            .relay(&Cmd::new("/bin/echo").arg("RFB 003.889"))
            .unwrap();
        let mut s = connect(relay.port());
        let mut got = Vec::new();
        s.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"RFB 003.889\n");
        relay.close().unwrap();
    }

    #[test]
    fn a_run_that_does_not_start_is_reported_at_the_close() {
        let port = Port::new();
        let mut relay = port.relay(&Cmd::new("/nonexistent/ssh")).unwrap();
        let mut s = connect(relay.port());
        let mut got = Vec::new();
        // The connection closes with the run that never started.
        s.read_to_end(&mut got).unwrap();
        assert!(got.is_empty());
        let why = relay.close().unwrap_err();
        assert!(why.starts_with("relay: /nonexistent/ssh: "), "{why}");
    }

    #[test]
    fn a_run_a_connection_cannot_carry_is_refused_before_anything_listens() {
        let port = Port::new();
        let user = crate::model::User {
            name: "admin".into(),
            uid: 501,
            gid: 20,
            home: "/Users//admin".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        };
        for cmd in [
            Cmd::new("cat"),
            Cmd::new("/bin/cat").as_user(&user),
            Cmd::new("/bin/cat").stdin_file("/dev/null"),
            Cmd::new("/bin/cat").until_deadline(),
        ] {
            let refused = port.relay(&cmd).err();
            assert!(
                matches!(refused, Some(RunFailed::Refused(_))),
                "{cmd:?}: {refused:?}"
            );
        }
    }
}
