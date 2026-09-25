// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SPIN CONFORMANCE — an idle instance must stay idle.
//!
//! WHY THIS SUITE EXISTS (2026-08 responsiveness audit, item A3). The
//! freeze-safety gate (`tools/freeze-safety-gate`) watches for loops that
//! BLOCK. Nothing in this repo watched for a loop that RUNS TOO MUCH, and so a
//! 200 kHz event-loop spin read to every existing gate as vigor.
//!
//! MEASURED LIVE on the owner's session before the fix: 31,913 stale deadline
//! re-arms per second, 913 MILLION banked, the process pinned at 79% CPU while
//! IDLE, and input latency p50 7.9 ms but p95 100 ms / p99 335 ms. The tail was
//! the lag they felt. The trigger shape is their daily one: a Claude-shaped
//! client emitting OUTPUT WITHOUT GRID MOVEMENT — a spinner repainting in
//! place — which armed a deadline already behind its own observation gate, on
//! every pass, forever.
//!
//! THE WITNESS is `past_deadline_arms` from `ctl metrics`. It counts exactly
//! the contradiction the spin is made of: a wake armed at an instant the gate
//! that judges it has already passed. Healthy idle banks EXACTLY ZERO of them
//! across this suite's window; the same window on a build with the fix reverted
//! banks 7929. That is why this counter, and not CPU% or a frame rate, is what
//! the gate asserts — it is the contradiction itself, counted.
//!
//! THE MATRIX (each row a test):
//!   1. a steady repainter arriving on the instance, 6 s → ~0 past_deadline_arms
//!   2. a bare /bin/sh prompt, nothing running, 6 s   → ~0 past_deadline_arms
//!
//! THE FIXTURE IS THE GATE. Row 1 does NOT reuse the paint matrix's
//! `fake_claude.py`: that client's spinner glyph and counter change every
//! frame, so every repaint is REAL grid movement and the movement clock never
//! goes stale. Measured against a build with the fix reverted, `fake_claude.py`
//! banked exactly 0 past-deadline arms — a green gate over a broken build.
//! `tools/spin-conformance/idle_repainter.py` rewrites BYTE-IDENTICAL content
//! ~6x/s forever: the bytes keep arriving (so `classify` sustains
//! phase=Running) while honest damage accounting sees nothing change (so
//! `owed_wake`'s movement clock goes stale), which is the two-clock split the
//! spin was made of.
//!
//! AND SO IS THE WINDOW'S PLACEMENT. The counter is zeroed BEFORE the client
//! starts, and the measured window opens with its arrival. Headless, the whole
//! episode is a burst in the ~2 s after the client reaches the alt screen,
//! after which the status FSM's Running phase ages out and the loop goes quiet
//! again; a probe that settles the client first and resets afterwards measures
//! the calm on the far side of the fire and reports 0. (On the owner's live
//! WINDOWED session fresh turns kept arriving, so the same defect never ran out
//! of fuel and sustained 31,913 arms/sec indefinitely.)
//!
//! PROVEN RED, 2026-08-24. Commit 420e4164 ("the event loop stops promising
//! itself work it cannot do") was reverted in a scratch worktree and the
//! RELEASE binary rebuilt; the healthy arm is the same worktree restored.
//!
//!   row 1 (steady repainter)   broken: 7929 arms / 1321 per sec  FAIL
//!                             healthy:    0 arms /    0 per sec  PASS
//!   row 2 (bare prompt)        broken:    0 arms                 PASS
//!                             healthy:    0 arms                 PASS
//!
//! Row 2 does not go red on the broken build, and that is correct and stated
//! rather than hidden: it is the FLOOR, not the regression's shape. Row 1 is
//! the falsifiable row.
//!
//! WHAT THIS INSTRUMENT PERTURBS (docs/RELEASE-PROOF-DISCIPLINE.md, the
//! OBSERVER RULE): during the measurement window the probe issues NO control
//! verbs — the two `ctl metrics` calls bracket the window from outside it.
//! Deliberately NOT `ctl video`: a recording drives a present loop, which is
//! the very idleness under test (and its motion pin is what blinded the paint
//! matrix for three releases). The counter is zeroed with `ctl metrics reset`
//! before the window, so what is asserted is a RATE over a known interval and
//! startup's own arming is never charged to the steady state.
//!
//! THE CEILING, calibrated 2026-08-24 (RELEASE profile, headless): BOTH rows
//! bank exactly 0 arms over a 6 s window on a healthy build, and row 1 banks
//! 7929 on the broken one. The gate's ceiling is 100 per window (≈16/s) —
//! infinitely above a healthy reading of zero, 79x below the measured
//! regression, and far enough off both to survive a loaded machine.
//!
//! WIRING: `tools/spin_guard.sh`, in the `guards` lane of `xtask gate lint` —
//! beside `paint_guard`, and fingerprinted the same way, so a run that does not
//! touch the event loop costs one content hash. NOTHING RUNS THAT LANE
//! AUTOMATICALLY: `.githooks/pre-push` was demoted to advisory on 2026-08-24
//! (its body is one printf and `exit 0`), and `tools/verify.sh --fast` — the
//! merge contract — runs `grep_guard.sh` and the license sweep as stages of its
//! own but never `run_repo_guards`, so it never reaches this script or
//! `paint_guard`. So this guard runs when a human runs
//! `cargo run -p xtask -- gate lint`, and at no other time. The fingerprint is
//! still worth having, because that is the run it makes cheap.
//!
//! The binary under test is RELEASE profile. Without an override, the shared
//! conformance helper freshens `target/conformance-release/release/aterm`, a
//! dedicated target reused by paint so nested builds do not feature-thrash the
//! outer test target. `ATERM_SPIN_BIN` (or `ATERM_PAINT_BIN`) drives an existing
//! artifact.
//!
//! THE TESTS LIVE IN `measuring` (2026-09-23). Every row counts deadline arms over a
//! measured window, so a busy machine can turn one red without the code changing.
//! The merge gate runs every test named `measuring::…` ALONE, in its exclusive
//! `measuring tests` stage, and skips them in the parallel test run
//! (`aterm_verify::stages::MEASURING_TESTS`); `targo test` by hand still runs them.
//!
//! UNIX-ONLY LANE: the probe drives a POSIX shell sandbox (`mktemp -d`,
//! `/bin/sh`, a unix-socket control path), so the whole file is
//! `#![cfg(unix)]` and this target holds no tests on Windows.
#![cfg(unix)]

mod support;

#[path = "spin/measuring.rs"]
mod measuring;
