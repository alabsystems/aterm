// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Terminal I/O for a program that draws on its controlling terminal, with no
//! editor knowledge (docs/DESIGN-amacs-2026-10-19.md §5).
//!
//! amacs's terminal face is its first user, but nothing here knows about
//! documents or commands. It is filled slice by slice:
//!
//! - **T1**: [`decode`], a pure byte-to-event decoder for keys (legacy C0,
//!   ESC as Alt, `CSI … ~`, SS3 and kitty CSI-u at flags 1 and 4), SGR mouse
//!   reports, bracketed paste, focus and probe replies, proved by round-trips
//!   through aterm's own encoders;
//! - **T2**: [`tty`] (the terminal a program reads and draws on: the
//!   controlling terminal, or an in-memory one on a virtual clock for tests),
//!   `raw` (raw mode and the private modes, put back on drop, panic, SIGINT,
//!   SIGTERM, SIGHUP and suspension), [`signals`] (SIGWINCH, SIGTSTP and SIGCONT
//!   through a self-pipe), [`probe`] (the capability probe) and [`screen`] (the
//!   diffing writer, checked against aterm's own grid);
//! - **T4**: `windows`, the Windows console arm.

pub mod decode;
pub mod probe;
#[cfg(unix)]
pub mod raw;
pub mod screen;
pub mod signals;
pub mod tty;
