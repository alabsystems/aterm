// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `screen`: relays SSH standard input and output to `127.0.0.1:5900` (`screensharingd`) while launchd enables `com.apple.screensharing`.
//!
//! Port 5900 is unprivileged, so the relay connects only when launchd enables `com.apple.screensharing` and holds `*:5900` as root.

use crate::items::person::{SCREEN_SHARING_LABEL, launchd_enabled, print_disabled_cmd};
use crate::model;
use crate::port::System;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

/// screensharingd's port on the host's loopback.
pub const SCREEN_SHARING_PORT: u16 = 5900;

/// Loopback connect timeout for catching a listener that never completes the handshake.
const CONNECT_BUDGET: Duration = Duration::from_secs(5);

/// Copy buffer size in bytes.
const CHUNK: usize = 64 * 1024;

/// Duplicates standard input for the relay's reader thread.
///
/// # Errors
/// `dup`'s.
pub fn own_stdin() -> io::Result<std::fs::File> {
    use std::os::fd::AsFd as _;
    Ok(io::stdin().as_fd().try_clone_to_owned()?.into())
}

/// Checks that launchd enables `com.apple.screensharing` (and therefore holds port 5900).
fn launchd_holds_the_port(sys: &dyn System) -> Result<(), String> {
    let ran = match sys.run(&print_disabled_cmd()) {
        Ok(ran) if ran.success() => ran,
        Ok(ran) => {
            return Err(format!(
                "launchctl print-disabled system: {}",
                ran.describe()
            ));
        }
        Err(e) => return Err(format!("launchctl print-disabled system: {e}")),
    };
    match launchd_enabled(&ran.out(), SCREEN_SHARING_LABEL) {
        model::Read::Value(true) => Ok(()),
        model::Read::Value(false) => Err(format!(
            "Screen Sharing is off (launchd disables {SCREEN_SHARING_LABEL})"
        )),
        model::Read::Absent => Err(format!(
            "Screen Sharing is off (launchd has no override for {SCREEN_SHARING_LABEL}, \
             whose plist ships it disabled)"
        )),
        model::Read::Unusable(why) => {
            Err(format!("whether Screen Sharing is on is unknown: {why}"))
        }
    }
}

/// Relays `input` to `127.0.0.1:<port>` and writes the socket's bytes to `out` until the connection closes, refusing before connecting if Screen Sharing is disabled in launchd.
pub fn screen(
    sys: &dyn System,
    port: u16,
    input: impl Read + Send + 'static,
    out: &mut dyn Write,
) -> ExitCode {
    if let Err(why) = launchd_holds_the_port(sys) {
        return super::refuse(out, &format!("screen: {why}"));
    }
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let socket = match TcpStream::connect_timeout(&addr, CONNECT_BUDGET) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            return super::refuse(
                out,
                &format!("screen: nothing listens on {addr}: Screen Sharing is off"),
            );
        }
        Err(e) => return super::refuse(out, &format!("screen: {addr}: {e}")),
    };
    // Disable Nagle's algorithm so small pointer-move writes do not wait on delayed ACKs.
    let _ = socket.set_nodelay(true);
    match pump(socket, input, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Copy both ways: `input` into `socket` on a detached thread, `socket` into `out` here.
fn pump(
    socket: TcpStream,
    mut input: impl Read + Send + 'static,
    out: &mut dyn Write,
) -> io::Result<()> {
    let mut upstream = socket.try_clone()?;
    std::thread::Builder::new()
        .name("screen-relay-in".into())
        .spawn(move || {
            let _ = io::copy(&mut input, &mut upstream);
            let _ = upstream.shutdown(Shutdown::Write);
        })?;
    let mut socket = socket;
    let mut buf = vec![0_u8; CHUNK];
    loop {
        let n = match socket.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if let Err(e) = out.write_all(&buf[..n]).and_then(|()| out.flush()) {
            let _ = socket.shutdown(Shutdown::Both);
            return Err(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Ran};
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
        let (s, _) = l.accept().unwrap();
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
        let (l, port) = stand_in();
        drop(l);
        let read = Arc::new(AtomicBool::new(false));
        let mut out = Vec::new();
        let code = screen(&enabled(), port, Watched(Arc::clone(&read)), &mut out);
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
        l.set_nonblocking(true).unwrap();
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
                l.accept().map(|_| ()).unwrap_err().kind(),
                io::ErrorKind::WouldBlock,
                "{name}: the relay connected"
            );
        }
        let fake = enabled();
        let mut out = Vec::new();
        let (input, held_open) = io::pipe().unwrap();
        l.set_nonblocking(false).unwrap();
        let server = std::thread::spawn(move || drop(accept(&l)));
        assert_eq!(screen(&fake, port, input, &mut out), ExitCode::SUCCESS);
        server.join().unwrap();
        drop(held_open);
        assert_eq!(fake.calls(), [PRINT_DISABLED_CMD]);
    }
}
