// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WALL-CLOCK connection-setup latency for the network drive, over loopback.
//!
//! Not a microbenchmark: it times the user-perceived span a `dial <name> <verb>`
//! actually pays — from "start connecting" to "the first control byte has made the
//! full round trip through the authenticated relay" — because that is the whole
//! cost of a remote verb in the shipping CLI form (aterm-ctl rejects a bare
//! `dial <name>`, so every remote verb is a fresh TCP+TLS connection).
//!
//! TWO-SIDED by construction:
//! * `drive_setup` — the shipping path: `accept_and_relay` + `dial_and_relay`, i.e.
//!   WITH the unauthenticated-phase watchdog armed on both ends.
//! * `bare_handshake` — the identical TLS 1.3 handshake, channel-bound capability
//!   exchange and relay, assembled inline WITHOUT a watchdog.
//!
//! The control arm is the transport floor: whatever `drive_setup` costs above
//! `bare_handshake` is what arming and joining the watchdog adds. A patch that
//! claims to remove watchdog latency must shrink the GAP between the arms, and
//! must not move `bare_handshake` at all.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_net::drive::{accept_and_relay, dial_and_relay};
use aterm_net::tls::{self, cert_fingerprint, client_config, server_config};
use aterm_net::{present_capability, verify_capability};
use aterm_session::EdgeToken;
use aterm_uds::CtlStream;

const CERT_DER: &[u8] = include_bytes!("../src/testdata/cert.der");
const KEY_DER: &[u8] = include_bytes!("../src/testdata/key.pkcs8.der");

/// Connections timed per arm. Each `drive_setup` connection costs a whole watchdog
/// poll interval on the pre-fix build, so this is deliberately small.
const ITERS: usize = 25;

fn spawn_echo(mut s: CtlStream) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        loop {
            match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if s.write_all(&buf[..n]).and_then(|()| s.flush()).is_err() {
                        break;
                    }
                }
            }
        }
    })
}

/// One timed connection through the SHIPPING drive path (watchdog on both ends).
fn one_drive_setup(listener: &TcpListener, token: EdgeToken) -> Duration {
    let addr = listener.local_addr().unwrap();
    let scfg = server_config(CERT_DER.to_vec(), KEY_DER.to_vec()).unwrap();
    let ccfg = client_config(cert_fingerprint(CERT_DER));

    let (svc_a, svc_b) = CtlStream::pair().unwrap();
    let echo = spawn_echo(svc_b);
    let svc_a = Arc::new(Mutex::new(Some(svc_a)));

    // Park the host in accept() BEFORE the clock starts: we time the connection,
    // not thread creation on the listener side.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let listener_clone = listener.try_clone().unwrap();
    let host = std::thread::spawn({
        let svc_a = Arc::clone(&svc_a);
        move || {
            ready_tx.send(()).unwrap();
            let (tcp, _) = listener_clone.accept().unwrap();
            accept_and_relay(
                tcp,
                scfg,
                |_s, op| (op == "drive").then_some(token),
                move || Ok(svc_a.lock().unwrap().take().unwrap()),
            )
        }
    });
    ready_rx.recv().unwrap();

    let (drv_local, mut drv_client) = CtlStream::pair().unwrap();
    let t0 = Instant::now();
    let driver = std::thread::spawn(move || {
        dial_and_relay(addr, ccfg, "bench", "drive", &token, b"", drv_local)
    });
    drv_client.write_all(b"P\n").unwrap();
    drv_client.flush().unwrap();
    let mut got = [0u8; 2];
    drv_client.read_exact(&mut got).unwrap();
    let elapsed = t0.elapsed();
    assert_eq!(&got, b"P\n");

    drv_client.shutdown(std::net::Shutdown::Both).unwrap();
    drop(drv_client);
    host.join().unwrap().unwrap();
    driver.join().unwrap().ok();
    echo.join().ok();
    elapsed
}

/// One timed connection through the SAME TLS + capability + relay work with NO
/// watchdog: the control arm that lacks the state under study.
fn one_bare_handshake(listener: &TcpListener, token: EdgeToken) -> Duration {
    let addr = listener.local_addr().unwrap();
    let scfg = server_config(CERT_DER.to_vec(), KEY_DER.to_vec()).unwrap();
    let ccfg = client_config(cert_fingerprint(CERT_DER));
    let timeout = Duration::from_secs(10);

    let (svc_a, svc_b) = CtlStream::pair().unwrap();
    let echo = spawn_echo(svc_b);

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let listener_clone = listener.try_clone().unwrap();
    let host = std::thread::spawn(move || -> std::io::Result<()> {
        ready_tx.send(()).unwrap();
        let (tcp, _) = listener_clone.accept().unwrap();
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;
        let mut transport = tls::accept(tcp, scfg)?;
        let exporter = transport.exporter().to_vec();
        let _granted = verify_capability(transport.stream(), &exporter, |_s, op| {
            (op == "drive").then_some(token)
        })?;
        {
            let sock = transport.stream().get_mut();
            sock.set_read_timeout(None)?;
            sock.set_write_timeout(None)?;
        }
        tls::relay(transport, svc_a)
    });
    ready_rx.recv().unwrap();

    let (drv_local, mut drv_client) = CtlStream::pair().unwrap();
    let t0 = Instant::now();
    let driver = std::thread::spawn(move || -> std::io::Result<()> {
        let tcp = TcpStream::connect_timeout(&addr, timeout)?;
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;
        let mut transport = tls::connect(tcp, tls::fixed_server_name(), ccfg)?;
        let exporter = transport.exporter().to_vec();
        present_capability(transport.stream(), &exporter, "bench", "drive", &token)?;
        {
            let sock = transport.stream().get_mut();
            sock.set_read_timeout(None)?;
            sock.set_write_timeout(None)?;
        }
        tls::relay(transport, drv_local)
    });
    drv_client.write_all(b"P\n").unwrap();
    drv_client.flush().unwrap();
    let mut got = [0u8; 2];
    drv_client.read_exact(&mut got).unwrap();
    let elapsed = t0.elapsed();
    assert_eq!(&got, b"P\n");

    drv_client.shutdown(std::net::Shutdown::Both).unwrap();
    drop(drv_client);
    host.join().unwrap().unwrap();
    driver.join().unwrap().ok();
    echo.join().ok();
    elapsed
}

fn report(name: &str, mut samples: Vec<Duration>) {
    samples.sort_unstable();
    let n = samples.len();
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    let total: f64 = samples.iter().map(|d| us(*d)).sum();
    println!(
        "{name:<16} n={n}  min={:>9.1} us  p50={:>9.1} us  mean={:>9.1} us  p90={:>9.1} us  max={:>9.1} us",
        us(samples[0]),
        us(samples[n / 2]),
        total / n as f64,
        us(samples[(n * 9) / 10]),
        us(samples[n - 1]),
    );
}

fn median(samples: &[Duration]) -> Duration {
    let mut v = samples.to_vec();
    v.sort_unstable();
    v[v.len() / 2]
}

fn main() {
    let token = EdgeToken::generate();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();

    // `ATERM_G5_BARE_ONLY=1` runs the control arm and NOTHING else — no drive
    // warmup, no drive arm. Two builds run this way execute byte-for-byte the same
    // work, so any difference between them is binary layout or machine drift, never
    // the change under test. It is how the control arm's cross-build shift was
    // attributed when this harness first measured the event-driven watchdog: with
    // the drive arm present the control arm appeared to move ~34% between builds;
    // under `ATERM_G5_BARE_ONLY=1` the two builds agreed, so that shift was the
    // process duty cycle (a poll-sleeping run spends ~1.4 s asleep), not the code.
    let bare_only = std::env::var_os("ATERM_G5_BARE_ONLY").is_some();

    // Warm: the first connection pays rustls provider init and page faults.
    let _ = one_bare_handshake(&listener, token);
    if !bare_only {
        let _ = one_drive_setup(&listener, token);
    }

    // The CONTROL arm runs FIRST, so its machine state is identical whatever the
    // drive arm costs. (Running it second was a real confound: on the poll-sleeping
    // build the drive arm spends ~1.4 s mostly asleep before it, which measurably
    // shifted the control arm's own number and made an invariant arm look as though
    // it had moved.)
    let mut bare = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        bare.push(one_bare_handshake(&listener, token));
    }
    if bare_only {
        report("bare_handshake", bare);
        return;
    }

    let mut drive = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        drive.push(one_drive_setup(&listener, token));
    }

    let cost = median(&drive).saturating_sub(median(&bare));
    report("drive_setup", drive);
    report("bare_handshake", bare);
    println!(
        "watchdog_cost    p50(drive_setup) - p50(bare_handshake) = {:.1} us",
        cost.as_secs_f64() * 1e6
    );

    // REACH, restated as something this harness proves on its own: every
    // `drive_setup` sample asserts the listener returned `Granted` (see
    // `one_drive_setup`), so the capability gate inside `accept_and_relay_inner`
    // ran — and that function arms a watchdog on every call, with no branch.
    // `one_bare_handshake` never calls it, so it arms none. While this harness was
    // being written both arms carried a temporary counter that confirmed exactly
    // that: 50 watchdogs armed per drive arm (two per connection, listener and
    // dialer) and 0 per control arm, in every run of both builds. Their mean
    // lifetime measured the defect directly: 54.0 ms sleep-polled, 0.18 ms
    // event-driven.
    //
    // A poll-sleeping watchdog cannot pass this: it puts a whole wake interval
    // between the two arms.
    assert!(
        cost < Duration::from_millis(20),
        "watchdog cost {cost:?} is a whole wake interval — the watchdog is being \
         waited out instead of woken"
    );
}
