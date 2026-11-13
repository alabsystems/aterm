// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Ran};
use aterm_uds::{AcceptWait, AcceptedMode};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Sample `launchctl print-disabled system` output with Screen Sharing enabled.
const PRINT_DISABLED: &str = "\n\tdisabled services = {\n\t\t\"com.apple.screensharing\" => enabled\n\t\t\"com.apple.CSCSupportd\" => disabled\n\t\t\"com.apple.mdmclient.daemon.runatboot\" => disabled\n\t\t\"com.apple.ftpd\" => disabled\n\t\t\"com.openssh.sshd\" => enabled\n\t}\n";

const PRINT_DISABLED_CMD: &str = "/bin/launchctl print-disabled system";

/// A host where launchd enables Screen Sharing.
fn enabled() -> Fake {
    let fake = Fake::root();
    fake.on(PRINT_DISABLED_CMD, Ran::ok(PRINT_DISABLED));
    fake
}

/// A listener standing in for screensharingd, and its port.
fn stand_in() -> (TcpListener, u16) {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    (l, port)
}

/// Accept one connection with a one-minute hang detector on its reads.
fn accept(l: &TcpListener) -> TcpStream {
    let (s, _) = aterm_uds::accept(l, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    s
}

const BANNER: &[u8] = b"RFB 003.889\n";

#[test]
fn bytes_flow_both_ways_and_the_end_of_input_closes_only_the_write_half() {
    let (l, port) = stand_in();
    let server = std::thread::spawn(move || {
        let mut s = accept(&l);
        s.write_all(BANNER).unwrap();
        let mut got = Vec::new();
        s.read_to_end(&mut got).unwrap();
        s.write_all(format!("got {}", got.len()).as_bytes())
            .unwrap();
        got
    });
    let mut out = Vec::new();
    let code = screen(
        &enabled(),
        port,
        io::Cursor::new(b"RFB 003.008\n\x01".to_vec()),
        &mut out,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(server.join().unwrap(), b"RFB 003.008\n\x01");
    assert_eq!(out, [BANNER, b"got 13"].concat());
}

#[test]
fn the_relay_ends_when_screen_sharing_closes_while_its_input_is_still_open() {
    let (l, port) = stand_in();
    let server = std::thread::spawn(move || {
        let mut s = accept(&l);
        s.write_all(BANNER).unwrap();
    });
    let (input, held_open) = io::pipe().unwrap();
    let mut out = Vec::new();
    assert_eq!(screen(&enabled(), port, input, &mut out), ExitCode::SUCCESS);
    server.join().unwrap();
    assert_eq!(out, BANNER);
    drop(held_open);
}

/// Standard output whose reader has gone.
struct Gone;

impl Write for Gone {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn when_standard_output_is_gone_the_relay_ends_and_closes_the_connection() {
    let (l, port) = stand_in();
    let server = std::thread::spawn(move || {
        let mut s = accept(&l);
        s.write_all(BANNER).unwrap();
        let mut rest = Vec::new();
        s.read_to_end(&mut rest).map(|_| rest)
    });
    let (input, held_open) = io::pipe().unwrap();
    assert_eq!(
        screen(&enabled(), port, input, &mut Gone),
        ExitCode::FAILURE
    );
    assert!(server.join().unwrap().unwrap().is_empty());
    drop(held_open);
}

/// Input that records whether anything read it.
struct Watched(Arc<AtomicBool>);

impl Read for Watched {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        self.0.store(true, Ordering::SeqCst);
        Ok(0)
    }
}

#[test]
fn nothing_listening_is_refused_before_any_byte_flows() {
    // Dropping a listener does not reserve its port against another test's bind.
    // Exercise the actual refusal path with the dial's explicit OS error instead.
    let port = SCREEN_SHARING_PORT;
    let mut dialed = false;
    let read = Arc::new(AtomicBool::new(false));
    let mut out = Vec::new();
    let code = screen_with_connect(
        &enabled(),
        port,
        Watched(Arc::clone(&read)),
        &mut out,
        |addr, _| {
            assert_eq!(*addr, SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
            dialed = true;
            Err(io::Error::from(io::ErrorKind::ConnectionRefused))
        },
    );
    assert!(dialed, "the enabled policy reached the dial");
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.starts_with("refused: ") && out.ends_with('\n'),
        "{out:?}"
    );
    assert_eq!(out.lines().count(), 1, "{out:?}");
    assert!(out.contains(&format!("127.0.0.1:{port}")), "{out:?}");
    assert!(!read.load(Ordering::SeqCst), "the input was never read");
}

#[test]
fn without_screen_sharing_enabled_in_launchd_nothing_connects() {
    let (l, port) = stand_in();
    let disabled = PRINT_DISABLED.replace(
        "\"com.apple.screensharing\" => enabled",
        "\"com.apple.screensharing\" => disabled",
    );
    let absent = PRINT_DISABLED.replace("\t\t\"com.apple.screensharing\" => enabled\n", "");
    for (name, reply) in [
        ("disabled", Ran::ok(&disabled)),
        ("no override", Ran::ok(&absent)),
        ("unreadable", Ran::exit(1, "launchctl: not privileged")),
        (
            "garbled",
            Ran::ok("Usage: launchctl print-disabled <domain-target>\n"),
        ),
    ] {
        let read = Arc::new(AtomicBool::new(false));
        let input = Watched(Arc::clone(&read));
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let fake = Fake::root();
            fake.on(PRINT_DISABLED_CMD, reply);
            let mut out = Vec::new();
            let code = screen(&fake, port, input, &mut out);
            let _ = done.send((code, out));
        });
        let (code, out) = finished
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{name}: the relay connected and waited"));
        assert_eq!(code, ExitCode::from(super::super::REFUSED), "{name}");
        let out = String::from_utf8(out).unwrap();
        assert!(out.starts_with("refused: screen: "), "{name}: {out:?}");
        assert_eq!(out.lines().count(), 1, "{name}: {out:?}");
        assert!(!read.load(Ordering::SeqCst), "{name}: the input was read");
        assert_eq!(
            aterm_uds::accept(&l, AcceptWait::Ready, AcceptedMode::Blocking)
                .map(|_| ())
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock,
            "{name}: the relay connected"
        );
    }
    let fake = enabled();
    let mut out = Vec::new();
    let (input, held_open) = io::pipe().unwrap();
    let server = std::thread::spawn(move || drop(accept(&l)));
    assert_eq!(screen(&fake, port, input, &mut out), ExitCode::SUCCESS);
    server.join().unwrap();
    drop(held_open);
    assert_eq!(fake.calls(), [PRINT_DISABLED_CMD]);
}
