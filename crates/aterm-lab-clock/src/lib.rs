// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real-Claude lab's sleep clock. No seam moves a live aterm's clocks, and
//! the lab never sleeps the Mac, so a sleep is emulated: this library, loaded
//! with `DYLD_INSERT_LIBRARIES` into the lab's OWN private instance and the
//! Claude Code in its tab (`LAB_CLOCK=1 targo --unverified lab up`), adds an
//! offset to the clocks a sleep moves, and `lab sleep TAG --for SECS`
//! (crates/aterm-lab/src/sleep.rs) stops the instance and steps the offset.
//! A forward step is what a sleep of the Mac looks like from inside a
//! process: the wall clock (and the clocks that count a sleep) ahead of
//! uptime. It is never part of what aterm ships, and nothing links it.
//!
//! What it shifts, by the offset:
//!
//! * every WALL clock read: `clock_gettime` and `clock_gettime_nsec_np` on
//!   `CLOCK_REALTIME`, `gettimeofday`, `time` (Rust's `SystemTime` and
//!   JavaScript's `Date.now()` among their callers);
//! * with `LABCLOCK_CONTINUOUS=1` (the lab sets it), the clocks macOS counts a
//!   sleep in too: `CLOCK_MONOTONIC` (Bun's timers read it),
//!   `CLOCK_MONOTONIC_RAW`, `CLOCK_MONOTONIC_RAW_APPROX`,
//!   `mach_continuous_time`, `mach_continuous_approximate_time`;
//! * never `CLOCK_UPTIME_RAW`, `CLOCK_UPTIME_RAW_APPROX` or
//!   `mach_absolute_time` (Rust's `Instant`): a real sleep leaves them where
//!   they were.
//!
//! The offset is decimal seconds in the file `LABCLOCK_FILE` names, written
//! by the lab (outside the jail) by rename. It is read again when the file's
//! inode, size or modification time changes, looked at no more often than
//! every 20 ms of uptime, so every process that loads the library takes a
//! step within 20 ms of its next clock read. A nested call (a function the
//! system calls inside another this library wraps) passes through unshifted,
//! so no read is shifted twice; so does every read before this library's
//! constructor has run (libSystem's own initializers read clocks before a
//! thread-local is usable).
//!
//! With `LABCLOCK_DIR` set, each process writes `<dir>/<pid>.txt` (by rename)
//! at load, at each new offset, and every 2 s of uptime while it reads a
//! clock: its pid, parent, executable, offset and how often it read each
//! clock (`model::render`). The lab reads it to know which of its processes
//! took a step.
//!
//! How: dyld's interposing tuples (`__DATA,__interpose`) and a constructor
//! (`__DATA,__mod_init_func`). An image's own calls are never interposed, so
//! the wrappers call the real functions by name. A process whose binary has a
//! hardened runtime refuses the library: the lab re-signs a copy of Claude
//! Code ad hoc for that. On other systems the library is empty.
//!
//! The port of the C interposer the 2026-10-19 measurement ran
//! (`docs/measured/real-claude-lab-2026-10-15.md`, "A sleep of the Mac inside
//! a limit's try"). Verified by the arithmetic's unit tests here and
//! `tests/live.rs`, which loads the built library into a probe child, steps
//! the offset, and reads each clock move by the step or not at all.

#[cfg(any(target_os = "macos", test))]
mod model;

#[cfg(all(target_os = "macos", not(test)))]
mod interpose;
