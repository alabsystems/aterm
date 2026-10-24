// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-vz-agent`: the Linux lane's guest agent (docs/DESIGN-vz-guest-2026-10-23.md §6).
//!
//! It runs as root in the lane's VM, started by the unit the bake writes, from the
//! read-only boot share. It listens on vsock port [`COMMAND_PORT`] for the host's
//! commands (CID 2 only), one connection each, and runs each `Exec` as the lane user
//! with exactly the host's environment, reporting exactly one of `Exited`, `Signaled` or
//! `NotStarted`. At start it dials the host's [`WATCH_PORT`] and says `Hello` (its own
//! SHA-256), `Booting` while systemd boots, `Ready` once the boot is over and the lane
//! user's manager answers, then a `Beat` every [`watch::TICK`].
//!
//! It is std-only plus aterm-digest, because aterm-libc has no musl cell: its few libc
//! calls are declared in its one `sys` module, and its one child constructor is
//! [`serve::command`]. The [`system::System`] port holds everything it asks of the
//! guest's OS, so the host's tests drive the serve loop and the watch connection with a
//! Fake ([`fake::Fake`]) over unix sockets; on macOS the bin itself serves on unix sockets
//! ([`seam`]), the test seam only.

pub mod boot;
#[cfg(any(test, debug_assertions))]
pub mod fake;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod mounts;
#[cfg(not(target_os = "linux"))]
pub mod seam;
pub mod serve;
#[cfg(any(test, debug_assertions))]
pub mod stand_in;
mod sys;
pub mod system;
pub mod user;
pub mod watch;
pub mod wire;

use std::io::{self, Read as _};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

/// The host's CID.
pub const HOST_CID: u32 = 2;

/// The vsock port the agent listens on for commands.
pub const COMMAND_PORT: u32 = 1024;

/// The vsock port of the host's listener the agent dials for its watch connection.
pub const WATCH_PORT: u32 = 1025;

/// The SHA-256 of the file at `path`, read in pieces.
///
/// # Errors
/// It could not be read.
pub fn sha256_of(path: &Path) -> io::Result<[u8; 32]> {
    let mut f = std::fs::File::open(path)?;
    let mut h = aterm_digest::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => return Ok(h.finalize()),
            Ok(n) => h.update(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// The agent's `Hello`: the protocol, the SHA-256 of its executable at `exe`
/// (`/proc/self/exe` in the guest), and the guest's boot.
///
/// # Errors
/// The executable could not be read.
pub fn hello(system: &dyn system::System, exe: &Path) -> io::Result<wire::Hello> {
    let facts = system.facts();
    Ok(wire::Hello {
        proto: wire::PROTO,
        agent_sha256: sha256_of(exe)?,
        boot_id: facts.boot_id,
        kernel: facts.kernel,
        uptime_ms: system.uptime_ms(),
    })
}

/// Speak the watch connection `watch` on its own thread and serve `listener`'s commands
/// on this one, until the listener fails.
#[must_use]
pub fn serve_with_watch<L: serve::Listener, W: serve::Stream>(
    listener: &L,
    watch: W,
    system: &Arc<dyn system::System>,
    hello: &wire::Hello,
    tick: Duration,
) -> ExitCode {
    let watching = Arc::clone(system);
    let hello = hello.clone();
    let spoke = std::thread::Builder::new()
        .name("vz-agent watch".into())
        .spawn(move || match watch::run(watch, &watching, &hello, tick) {
            Ok(()) => eprintln!("aterm-vz-agent: the host closed the watch connection"),
            Err(e) => eprintln!("aterm-vz-agent: the watch connection failed: {e}"),
        });
    if let Err(e) = spoke {
        eprintln!("aterm-vz-agent: the watch connection could not start: {e}");
        return ExitCode::FAILURE;
    }
    match serve::serve(listener, system) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aterm-vz-agent: the command listener failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The usage line of this host's bin.
#[cfg(not(target_os = "linux"))]
const USAGE: &str = "aterm-vz-agent --listen <unix socket> --watch <unix socket> [--tick-ms N] \
                     (the macOS test seam; the agent itself runs in the Linux guest)";

// STOCK EXCEPTION: the guest's agent is cross-built for aarch64-unknown-linux-musl on
// rustup's stable (`lab linux vz-agent`), because Trust ships no Linux std; its Linux
// code is linted on stable by `verify cells --cell linux-arm` (the vz design's §6).
/// The bin: in the guest, the agent ([`linux::run`]), which takes no argument.
#[cfg(target_os = "linux")]
#[must_use]
pub fn main(args: &[std::ffi::OsString]) -> ExitCode {
    if !args.is_empty() {
        eprintln!("aterm-vz-agent: takes no argument (its unit starts it)");
        return ExitCode::from(2);
    }
    linux::run()
}

/// The bin: off Linux, the unix-socket seam ([`seam::run`]).
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn main(args: &[std::ffi::OsString]) -> ExitCode {
    let (mut listen, mut watch, mut tick) = (None, None, watch::TICK);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next();
        match (flag.to_str(), value) {
            (Some("--listen"), Some(v)) => listen = Some(std::path::PathBuf::from(v)),
            (Some("--watch"), Some(v)) => watch = Some(std::path::PathBuf::from(v)),
            (Some("--tick-ms"), Some(v)) => match v.to_str().and_then(|v| v.parse().ok()) {
                Some(ms) => tick = Duration::from_millis(ms),
                None => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    match (listen, watch) {
        (Some(listen), Some(watch)) => seam::run(&listen, &watch, tick),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
