// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Spin conformance: an idle instance must stay idle.
//!
//! The freeze-safety gate watches for loops that block; this suite watches for
//! a loop that runs too much. The shape: a client emitting output without grid
//! movement (a spinner repainting in place) must not make the event loop
//! re-arm a deadline that is already behind its own observation gate.
//!
//! The witness is `past_deadline_arms` from `ctl metrics`: a wake armed at an
//! instant its gate has already passed. Healthy idle banks zero.
//!
//! The matrix:
//!   1. a steady repainter arriving on the instance, 10 s → ~0
//!      past_deadline_arms and at most 12 timer wakes
//!   2. a bare /bin/sh prompt, nothing running, 6 s → ~0 past_deadline_arms
//!
//! Row 1's fixture, `tools/spin-conformance/idle_repainter.py`, warms with one
//! second of real writes (so `classify` reaches phase=Running), then writes
//! ~6x/s bytes that mark no cell (a DEC-2026 bracket around a bare cursor
//! position): output keeps `Running` alive while `owed_wake`'s movement clock
//! goes stale. Content marks count as movement even when bytes are identical,
//! so the fixture must not rewrite text. The probe checks `ctl status` reads
//! `output_activity` at the end of the window.
//!
//! The counter is zeroed before the client starts and the window opens with
//! its arrival: headless, the episode is a burst in the ~2 s after the client
//! reaches the alt screen, after which Running ages out.
//!
//! Row 1 also bounds timer wakes: the clamp in `StatusObserver::next_wake`
//! turns a stale `owed_wake` into a poll at the observation rate, which arms
//! nothing in the past, so `past_deadline_arms` alone cannot see that
//! regression. Row 2 is the floor, not the regression's shape.
//!
//! Observer rule (docs/RELEASE-PROOF-DISCIPLINE.md): the probe issues no
//! control verbs inside the window; `ctl metrics reset` and `ctl metrics`
//! bracket it. No `ctl video`, whose present loop would break the idleness
//! under test. The ceiling is 100 arms per window, far above a healthy zero
//! and far below the regression.
//!
//! The merge gate's test run skips every `measuring::…` test
//! (`aterm_verify::stages::MEASURING_TESTS`); they are the MEASURE tier
//! (`tools/verify.sh --measure`, `--full`), run alone in the exclusive
//! `measuring tests` stage, and a release cut requires them green. A probe
//! that decides nothing (exit 2) is recorded as COULD NOT RUN. The binary is
//! release profile, from `ATERM_SPIN_BIN` (or `ATERM_PAINT_BIN`); without
//! either a row says `not measured` (`support::release_bin`).
//!
//! Unix only: the probe drives a POSIX shell sandbox (`mktemp -d`, `/bin/sh`,
//! a unix-socket control path).
#![cfg(unix)]

mod support;

#[path = "spin/measuring.rs"]
mod measuring;
