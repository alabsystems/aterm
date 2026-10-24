// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The watch connection, which the agent dials at start (CID 2, port 1025): `Hello` at
//! once, `Booting{jobs}` every tick while systemd boots, `Ready` once
//! `is-system-running --wait` has said `running` or `degraded` and the lane user's
//! manager answers, then `Beat` every tick. The host's `Clock` steps the guest's clock.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::serve::Stream;
use crate::system::{Booted, System};
use crate::wire::{self, Frame, Hello, Job, Ready};

/// How often the watch connection speaks: 5 s.
pub const TICK: Duration = Duration::from_secs(5);

enum Event {
    Booted(Booted),
    Closed,
}

fn send<S: Stream>(conn: &mut S, frame: &Frame) -> io::Result<()> {
    wire::write_frame(conn, frame).map_err(|e| match e {
        wire::WireError::Io(e) => e,
        other => io::Error::other(other.to_string()),
    })
}

/// Speak on `conn` until the host closes it.
///
/// # Errors
/// The host could not be written to, or a thread could not start.
pub fn run<S: Stream>(
    conn: S,
    system: &Arc<dyn System>,
    hello: &Hello,
    tick: Duration,
) -> io::Result<()> {
    let mut out = conn.try_clone()?;
    send(&mut out, &Frame::Hello(hello.clone()))?;
    let (tx, events) = mpsc::channel();

    let host = tx.clone();
    let clock = Arc::clone(system);
    thread::Builder::new()
        .name("vz-agent watch input".into())
        .spawn(move || {
            let mut conn = conn;
            loop {
                match wire::read_frame(&mut conn) {
                    Ok(Some(Frame::Clock { unix_ns })) => {
                        if let Err(why) = clock.set_clock(unix_ns) {
                            clock.say(&format!("the clock was not set to the host's: {why}"));
                        }
                    }
                    Ok(Some(other)) => clock.say(&format!(
                        "the watch connection carried frame kind 0x{:02x}; ignored",
                        other.kind()
                    )),
                    Ok(None) | Err(_) => {
                        let _ = host.send(Event::Closed);
                        return;
                    }
                }
            }
        })?;

    let boot = Arc::clone(system);
    thread::Builder::new()
        .name("vz-agent boot".into())
        .spawn(move || {
            loop {
                let booted = boot.booted();
                if booted.is_up() {
                    let _ = tx.send(Event::Booted(booted));
                    return;
                }
                boot.say(&format!(
                    "systemd says the system is {}; asking again",
                    booted.state
                ));
                thread::sleep(tick);
            }
        })?;

    let mut first_seen: HashMap<u32, Instant> = HashMap::new();
    let mut booted: Option<Booted> = None;
    // Why `Ready` last waited on the lane user's bus, said once each time it changes: a
    // boot systemd calls over can stall there alone (no linger, no lane user), with no
    // job left for the host to name.
    let mut bus_wait: Option<String> = None;
    loop {
        if let Some(b) = &booted {
            match system.user_bus() {
                Ok(()) => {
                    // `Ready` implies the agent already listens for commands: its start
                    // binds the command listener before it dials this connection.
                    send(
                        &mut out,
                        &Frame::Ready(Ready {
                            state: b.state.clone(),
                            failed_units: b.failed_units.clone(),
                        }),
                    )?;
                    break;
                }
                Err(why) => {
                    if bus_wait.as_deref() != Some(why.as_str()) {
                        system.say(&format!(
                            "systemd says the system is {}; Ready waits for the lane user's \
                             manager: {why}",
                            b.state
                        ));
                        bus_wait = Some(why);
                    }
                }
            }
        }
        let now = Instant::now();
        let raw = system.jobs();
        first_seen.retain(|id, _| raw.iter().any(|j| j.id == *id));
        let jobs = raw
            .into_iter()
            .map(|j| {
                let since = *first_seen.entry(j.id).or_insert(now);
                Job {
                    id: j.id,
                    unit: j.unit,
                    kind: j.kind,
                    state: j.state,
                    elapsed_ms: u64::try_from(now.duration_since(since).as_millis())
                        .unwrap_or(u64::MAX),
                }
            })
            .collect();
        send(&mut out, &Frame::Booting(jobs))?;
        match events.recv_timeout(tick) {
            Ok(Event::Booted(b)) => booted = Some(b),
            Ok(Event::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    loop {
        match events.recv_timeout(tick) {
            Ok(Event::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Ok(Event::Booted(_)) | Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        send(
            &mut out,
            &Frame::Beat {
                uptime_ms: system.uptime_ms(),
            },
        )?;
    }
}
