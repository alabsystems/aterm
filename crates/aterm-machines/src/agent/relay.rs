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
    screen_with_connect(sys, port, input, out, TcpStream::connect_timeout)
}

fn screen_with_connect(
    sys: &dyn System,
    port: u16,
    input: impl Read + Send + 'static,
    out: &mut dyn Write,
    connect: impl FnOnce(&SocketAddr, Duration) -> io::Result<TcpStream>,
) -> ExitCode {
    if let Err(why) = launchd_holds_the_port(sys) {
        return super::refuse(out, &format!("screen: {why}"));
    }
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let socket = match connect(&addr, CONNECT_BUDGET) {
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
#[path = "relay_tests.rs"]
mod tests;
