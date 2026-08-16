// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-uds** — the portable stream type behind the aterm introspection
//! CONTROL SOCKET, plus the small platform helpers the control channel needs
//! (the `latest` alias, CSPRNG bytes, pid liveness, and — Unix only —
//! `SCM_RIGHTS` descriptor passing with the peer pid that authenticates it).
//!
//! On Unix, [`CtlStream`]/[`CtlListener`] are **pure type aliases** to
//! [`std::os::unix::net::UnixStream`]/[`UnixListener`], so every Unix call
//! site is byte-identical to plain std — zero behavioral change, and
//! `impl Read for &CtlStream` etc. come straight from std.
//!
//! On Windows they are AF_UNIX sockets over winsock (`afunix.sys`,
//! Windows 10 1803+), implemented with direct `ws2_32` FFI — the same
//! socket-file naming/discovery model carries over unchanged. Four operations
//! are deliberately redesigned rather than emulated 1:1 (see [`win`]):
//!
//! * `try_clone` shares one socket via `Arc` (afunix does not reliably
//!   support `WSADuplicateSocketW`); clones behave like Unix `dup`s — shared
//!   file description, shutdown/timeouts affect all clones, the socket closes
//!   when the last clone drops.
//! * read/write timeouts are `WSAPoll`-based (per-provider `SO_RCVTIMEO`
//!   support on afunix is uncertain); expiry reads as `WouldBlock`, matching
//!   Unix `UnixStream` behavior.
//! * the `latest` alias is a regular pointer FILE (NTFS symlinks need
//!   privilege/dev-mode) — see [`latest`].
//! * there is no peer-uid primitive (`SO_PEERCRED`/`getpeereid` have no
//!   AF_UNIX-on-Windows analog); callers keep the mandatory per-launch token
//!   and must disclose the reduced posture (they do — one-line startup
//!   notice in `aterm-gui`). For the same reason afunix carries no ancillary
//!   data at all, so [`fdpass`]'s descriptor transport refuses there rather
//!   than emulating anything.

// Under the Trust verifier, register the `trust` tool namespace so the
// `#[cfg_attr(trust_verify, trust::skip)]` opt-out on the FFI CSPRNG wrapper
// resolves; plain rustc never sets `trust_verify`, so this is inert off-Trust.
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

pub mod fdpass;
pub mod latest;
pub mod process;
pub mod rand;
/// trust-mc proofs for [`rand::hex_encode`] (compiled only under `cfg(kani)`).
mod rand_kani_proofs;

/// The per-user base directory holding aterm's control socket + token, resolved
/// IDENTICALLY for the server (`aterm-gui`) and every client (`aterm-ctl`) so the
/// two can never dial different directories. `None` only when the per-user base
/// cannot be resolved from the environment (should not happen interactively).
///
/// **Windows:** `%TMP%`/`%TEMP%\aterm` (falling back to `%LOCALAPPDATA%\Temp\aterm`).
/// This MUST live OUTSIDE the OneDrive-managed `%APPDATA%` subtree: on a machine with
/// OneDrive Known-Folder-Move (or a similar filesystem filter over AppData), afunix
/// `bind` writes the socket's reparse point fine but `connect` cannot open it and
/// fails with `WSAEINVAL` (10022) — so a socket under `%LOCALAPPDATA%\aterm` binds but
/// is never reachable and every `aterm-ctl` call fails. `%TEMP%` (typically
/// `%LOCALAPPDATA%\Temp`) is excluded from that filter and connects. (Empirically
/// mapped on a OneDrive machine via both aterm-uds and raw `ws2_32`; the exact filter
/// is inferred, but relocating off AppData is the load-bearing fix.) Well under the
/// 108-byte `sun_path` limit.
///
/// **Unix:** `$XDG_RUNTIME_DIR/aterm` when set, else
/// `$HOME/Library/Application Support/aterm`.
#[must_use]
pub fn control_socket_dir() -> Option<std::path::PathBuf> {
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
