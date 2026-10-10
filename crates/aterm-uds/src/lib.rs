// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-uds: the portable stream type behind aterm's control socket, plus the
//! platform helpers the control channel needs (the `latest` alias, CSPRNG bytes,
//! pid liveness and, on Unix, `SCM_RIGHTS` descriptor passing authenticated by
//! the peer pid).
//!
//! On Unix, [`CtlStream`]/[`CtlListener`] are type aliases to
//! [`std::os::unix::net::UnixStream`]/[`UnixListener`].
//!
//! On Windows they are AF_UNIX sockets over winsock (`afunix.sys`, Windows 10
//! 1803+) via direct `ws2_32` FFI, with the same socket-file naming. Four
//! operations differ from Unix (see [`win`]):
//!
//! * `try_clone` shares one socket via `Arc` (afunix does not reliably support
//!   `WSADuplicateSocketW`); clones behave like Unix `dup`s: shutdown and
//!   timeouts affect all clones, and the socket closes when the last one drops.
//! * read/write timeouts and shutdown wakeups are `WSAEventSelect`-based;
//!   expiry reads as `WouldBlock`, as on Unix.
//! * the `latest` alias is a regular pointer file (NTFS symlinks need
//!   privilege); see [`latest`].
//! * there is no peer-uid primitive, so callers rely on the per-launch token
//!   and disclose the reduced posture. afunix carries no ancillary data, so
//!   [`fdpass`] refuses there.

// Registers the `trust` tool namespace under the Trust verifier so the
// `trust::skip` opt-out on the FFI CSPRNG wrapper resolves.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

#[cfg(unix)]
pub type CtlStream = std::os::unix::net::UnixStream;
#[cfg(unix)]
pub type CtlListener = std::os::unix::net::UnixListener;

/// The Windows AF_UNIX-over-winsock implementation of [`CtlStream`] /
/// [`CtlListener`] (afunix.sys; raw `ws2_32` FFI, no external deps).
#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub use win::{CtlListener, CtlStream};

/// A deadline on a call that can block forever: in this process ([`deadline::Gate`],
/// the shared [`deadline::TRUST`] gate) or in a child ([`deadline::output_until`]).
/// Native-only: it exists for macOS's `trustd` and child processes, and starts
/// threads, which the wasm32 build never does.
#[cfg(not(target_arch = "wasm32"))]
pub mod deadline;
/// A kernel witness of another process's death, with its `wait(2)` status (the
/// PTY keeper's death classifier; lifted from the seamless handoff), and the
/// moment of it ([`exitwatch::ExitMoment`], `aterm ctl subscribe`'s redial).
pub mod exitwatch;
pub mod fdpass;
/// Whether a connected stream's peer has closed entirely (a lane's hangup check).
pub mod hangup;
/// Which live processes of this uid hold a descriptor for one device (the PTY
/// keeper's holder scan), or use a path under a set of directories (the disk
/// watch's check before it removes a build directory whole).
pub mod holders;
pub mod latest;
/// Unix-only: the descriptor a throwaway `--headless` instance watches so it ends
/// with the process that started it — both ends. See [`lifeline`].
#[cfg(unix)]
pub mod lifeline;
/// Which listening socket a path names, so a hang-up is judged by the listener
/// that accepted the stream and never by the pid alone.
pub mod listener;
/// Who holds a file's owner lock (the PTY keeper reads a dead window's crash
/// marker with it).
#[cfg(unix)]
pub mod ownerlock;
/// The kernel's record of a socket peer: its uid and (Darwin) audit token.
pub mod peer;
pub mod process;
/// `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`: a child that inherits
/// nothing but a `/dev/null` stdio (the PTY keeper's one spawn).
pub mod pspawn;
pub mod rand;
/// Which Windows console a child process gets: a WINDOWLESS one when this
/// process has none (the window), so no console flashes on screen, and nothing
/// changed when it has one or off Windows. [`spawnfd::command`] applies it to
/// every child; [`spawn::set_flags`] is the one creation-flags setter. See
/// [`spawn`].
pub mod spawn;
/// Start a child holding two descriptors at fixed numbers and nothing else above
/// stdio — the fabric bridge's launch — the inheritance strip for a spawn that
/// must fork (the PTY child's), [`spawnfd::command`], the `Command` every
/// other child is built with (on Darwin one `posix_spawn`, no fork), and
/// [`spawnfd::transparent`], the pass-throughs' spawn, whose child inherits this
/// process's whole table whatever std is linked. See [`spawnfd`].
#[cfg(unix)]
pub mod spawnfd;
/// Off Unix, [`spawnfd::command`] (there: the Windows console rule of [`spawn`]),
/// [`spawnfd::transparent`] (the same), [`spawnfd::InheritNothing`] and `std`'s
/// `Command`, `Stdio` and `Child` under spawnfd's names, so a shared spawn site
/// reads the same on every platform.
#[cfg(not(unix))]
#[path = "spawnfd_nonunix.rs"]
pub mod spawnfd;

/// The per-user directory holding aterm's control socket and token, resolved
/// identically for the server and every client. `None` only when the
/// environment names no per-user base.
///
/// **Windows:** `%TMP%`/`%TEMP%\aterm` (else `%LOCALAPPDATA%\Temp\aterm`). It
/// stays outside `%APPDATA%`: a OneDrive-style filter there lets afunix `bind`
/// succeed while `connect` fails with `WSAEINVAL`.
///
/// **Unix:** `$XDG_RUNTIME_DIR/aterm` when set, else
/// `$HOME/Library/Application Support/aterm`.
#[must_use]
pub fn control_socket_dir() -> Option<std::path::PathBuf> {
    control_socket_dir_inner()
}

/// Whether [`control_socket_dir`] is private: `$XDG_RUNTIME_DIR` is set, so it
/// is not the per-user default under `$HOME`. Always `false` on Windows. The
/// registry there lists only the instances that publish into that directory.
#[must_use]
pub fn control_socket_dir_is_private() -> bool {
    #[cfg(windows)]
    {
        false
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_RUNTIME_DIR").is_some()
    }
}

fn control_socket_dir_inner() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    #[cfg(windows)]
    {
        for var in ["TMP", "TEMP"] {
            if let Some(v) = std::env::var_os(var).filter(|s| !s.is_empty()) {
                return Some(PathBuf::from(v).join("aterm"));
            }
        }
        let local = std::env::var_os("LOCALAPPDATA").filter(|s| !s.is_empty())?;
        Some(PathBuf::from(local).join("Temp").join("aterm"))
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
            return Some(PathBuf::from(xdg).join("aterm"));
        }
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("aterm"),
        )
    }
}
