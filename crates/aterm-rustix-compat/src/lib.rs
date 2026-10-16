// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// The API (module paths, names, signatures and flag names) is rustix 0.38.44's
// (https://github.com/bytecodealliance/rustix, Apache-2.0 WITH LLVM-exception
// OR Apache-2.0 OR MIT, used here under Apache-2.0); src/shm.rs's name check is
// derived from its code and says so. Modified by the aterm project in 2026:
// every call goes through aterm-libc, and only what aterm's graph calls is here.

//! `rustix` 0.38: aterm's first-party replacement for the crates.io package of
//! the same name (the manifest says why the directory name differs).
//!
//! Four consumers hold the 0.38 major, all on Linux and all under the
//! windowing stack: calloop 0.13.0, calloop-wayland-source 0.3.0,
//! smithay-client-toolkit 0.19.2 and crates/aterm-window (the absorbed winit,
//! package `winit`). Upstream brought linux-raw-sys 0.4.15 with it, and the
//! two cost every Linux cell 2 packages and 396,195 lines
//! (docs/AUDIT-third-party-2026-10-15.md, LW.2) for these calls, which
//! aterm-libc already declares:
//!
//! ```text
//! io       read write Errno Result ioctl_fionbio   calloop ping sources, cws, winit
//! fs       fcntl_getfl fcntl_setfl OFlags          calloop's set_nonblocking
//!          memfd_create MemfdFlags                 sctk shm pool
//!          fcntl_add_seals SealFlags               sctk shm pool
//! pipe     pipe pipe_with PipeFlags                calloop, sctk offers, winit
//! event    eventfd EventfdFlags                    calloop's Linux ping
//!          poll PollFd PollFlags                   winit's Wayland event loop
//! shm      shm_open shm_unlink ShmOFlags Mode      sctk's memfd fallback
//! thread   gettid                                  winit is_main_thread
//! process  getpid Pid                              winit is_main_thread, _NET_WM_PID
//! system   uname Uname                             winit WM_CLIENT_MACHINE
//! ```
//!
//! `ioctl_fionbio` is reached only from calloop's `cfg(windows)` arm and
//! `pipe` only from calloop's macOS arm and its tests; both are here because
//! they are part of the surface those crates compile against.
//!
//! Off Linux the crate is empty: no consumer compiles there. That includes the
//! BSDs, where aterm-window's rustix edge (every unix but redox, wasm, Android,
//! iOS and macOS) would now find nothing; no cell builds a BSD, so it is left
//! so on purpose.
//!
//! # How it is checked
//!
//! The oracle is rustix 1.1.4, the copy the Linux graph keeps (x11rb, wayland-*,
//! softbuffer, polling, gethostname, zbus and the async-* crates): the patch
//! row would capture a 0.38 dev-dependency and compare this crate with itself.
//!
//! * `tests/abi.rs` holds every flag and errno value here equal to rustix
//!   1.1.4's, which takes them from linux-raw-sys (the kernel's headers) where
//!   these take them from aterm-libc (glibc's, through libc 0.2.186). Each
//!   comparison is a `const` assertion, so a cross type-check for
//!   x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu decides it without
//!   running anything.
//! * `tests/differential.rs` runs each function beside rustix 1.1.4's on a
//!   Linux host and compares what the kernel then reports (descriptor and
//!   status flags, seals, bytes read, readiness, errno values, ids, uname).
//! * `const` blocks in `event.rs` and `system.rs` hold `PollFd` to
//!   `struct pollfd`'s layout, which `poll` relies on, and `Uname`'s copy to
//!   `struct utsname`'s size; any compile for a Linux triple decides them.
//!
//! # Divergences from upstream
//!
//! * **libc calls, not raw syscalls.** Each function calls its glibc wrapper,
//!   so EINTR handling, errno values and results are the kernel's either way.
//!   `gettid` and `memfd_create` go through `syscall(2)`, as upstream's do in
//!   effect: glibc's wrappers exist only since 2.30 and 2.27, and calling them
//!   would raise the glibc a binary needs.
//! * **`fcntl_setfl` does not OR in `O_LARGEFILE`.** F_SETFL ignores the bit
//!   (it is outside the kernel's SETFL_MASK) and glibc's value is 0 on 64-bit.
//! * **Fewer names.** `Errno` declares the values consumers match on and the
//!   ones this crate returns; every other errno is carried by value, and
//!   `raw_os_error`, `kind`, `Display` and `Debug` behave alike for all of
//!   them. The flag types name the bits aterm-libc declares.
//!   `Errno::from_raw_os_error` accepts any value where upstream asserts
//!   `1..4096`.
//! * **`Debug` text of the flag types.** The shape is upstream's
//!   (`PollFlags(IN | HUP)`), not always the text: `bitflags` names a flag
//!   whose bits the value holds if any of them is still unclaimed, in
//!   upstream's declaration order, where this names one only if all of its
//!   bits are, in this crate's order, and bits with no name here print as hex.
//!   So `OFlags::from_bits_retain(3)` prints `OFlags(WRONLY | RDWR)` here and
//!   `OFlags(ACCMODE)` upstream, and the O_LARGEFILE bit `fcntl_getfl` reads
//!   back prints as hex (`0x8000` on x86_64). No consumer formats these flags.
//! * **No feature gates.** Every module is built whatever features are on.
//!
//! This crate is deleted when aterm-window's Linux module moves to the
//! first-party protocol crates (LW.7).

#[cfg(target_os = "linux")]
#[macro_use]
mod flags;

#[cfg(target_os = "linux")]
pub mod event;
#[cfg(target_os = "linux")]
pub mod fs;
#[cfg(target_os = "linux")]
pub mod io;
#[cfg(target_os = "linux")]
pub mod path;
#[cfg(target_os = "linux")]
pub mod pipe;
#[cfg(target_os = "linux")]
pub mod process;
#[cfg(target_os = "linux")]
pub mod shm;
#[cfg(target_os = "linux")]
pub mod system;
#[cfg(target_os = "linux")]
pub mod thread;
