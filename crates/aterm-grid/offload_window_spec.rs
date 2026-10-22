// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Model of the scrollback-offload detach window, `include!`d by
// tests/conformance_offload.rs, which proves it and drives the real `Grid`
// against it. Requires `Model` + `ty_model` in scope at the include site.
//
// The offload detaches the tiered store, rewraps it off the lock on a worker,
// then re-attaches. While detached, these steps interleave in any order:
//   Produce             — the PTY reader scrolls a line off
//   Erase               — an ED3 / `clear` lands
//   SetLine*/SetBudget* — a host changes the line limit / byte budget; newest wins
//   AttachReplacement   — reset/recovery installs an authoritative replacement store
//   Reattach            — the worker finishes and re-attaches
//   Abort               — the worker died; `abort_reflow_offload` recovers to a
//                         bounded state instead of wedging
//
// Not modelled: a resize while the window is open. With the store out,
// `resize_offloading_scrollback` detaches nothing, so it is a plain `Grid::resize`
// that stages rows into `lazy_buffer` with no PTY scroll-off (see
// `StagedWithinProduced`).
//
// `Buggy=0` is the shipped logic; `Buggy=1` loses window output, resurrects
// erased history, exposes stale deferred settings and loses staged output on a
// replacement-backed Abort. The gate requires a `Buggy=1` counterexample so the
// invariants cannot go vacuous.

/// The detach window as a bounded state machine (see file comment above).
fn offload_window_model() -> Model {
    ty_model! {
        ScrollbackOffloadWindow {
            const W = 3;         // bounded lines that scroll off during the window
            const Buggy = 0;     // 0 = shipped logic; 1 = the seeded bugs
            const RingCap = 8;   // construction-bounded emergency ring
            const LineLow = 5;   // finite total below RingCap
            const LineHigh = 20; // finite total above RingCap
            const BudgetLow = 2; // bounded budget codes (MiB in Tier-1)
            const BudgetHigh = 3;
            const ReplacementBudget = 4;
            const InitialBudget = 8;
            var detached = 1;    // store detached for the off-thread reflow (window open)
            var produced = 0;    // lines that scrolled off during the window
            var retained = 0;    // ... of those, kept (staged to lazy_buffer)
            var relocated = 0;   // ... of those, drained lazy_buffer -> attached store
            var cleared = 0;     // a `clear` / ED3 landed during the window
            var done = 0;        // window closed (Reattach or Abort)
            var aborted = 0;     // closed via Abort (worker death) — loss accepted
            var resurrected = 0; // erased history came back after the window
            var slack = 3;       // = W while loss is excusable; 0 after clean Reattach
            var pending = 1;     // detach settings snapshot exists iff unresolved
            var backend = 0;     // a replacement or reflowed store is attached
            var replacement = 0; // the attached backend came from reset/recovery
            var ring_limit = 8;  // surviving hot-ring cap (never exceeds RingCap)
            var line_latest = 0; // newest effective total; 0 encodes unlimited
            var line_observed = 0; // public getter projection
            var line_applied = 0;  // effective total installed on attached backend
            var line_dirty = 0;  // host changed the detach-time line snapshot
            var budget_latest = 8; // newest effective budget code
            var budget_observed = 8; // public getter projection; 0 = no backend
            var budget_applied = 0; // budget installed on attached backend
            var budget_dirty = 0; // host changed the detach-time budget snapshot

            // PTY output scrolls a line off during the window: staged to the lazy
            // buffer (Buggy drops it).
            action Produce when (detached > 0 && done <= 0 && produced <= W - 1) {
                produced = produced + 1;
                retained = if Buggy > 0 { retained } else { retained + 1 };
            }

            // A `clear` / ED3 lands during the window. The real `erase_scrollback`
            // also clears the lazy buffer: that is the user's clear, not loss, so the
            // produced/retained debt resets and NoLoss covers only later output.
            action Erase when (detached > 0 && done <= 0 && cleared <= 0) {
                cleared = 1;
                produced = 0;
                retained = 0;
            }

            // Line-limit requests: 0 is an explicit unlimited request, not "no
            // request". With a backend attached they apply at once; a low total also
            // tightens the hot ring, and raises never grow it back.
            action SetLineLow when (detached > 0 && done <= 0) {
                line_latest = LineLow;
                line_observed = if Buggy > 0 { line_observed } else { LineLow };
                line_applied = if backend > 0 {
                    if Buggy > 0 { line_applied } else { LineLow }
                } else { line_applied };
                ring_limit = if backend > 0 { LineLow } else { ring_limit };
                line_dirty = 1;
            }

            action SetLineHigh when (detached > 0 && done <= 0) {
                line_latest = LineHigh;
                line_observed = if Buggy > 0 { line_observed } else { LineHigh };
                line_applied = if backend > 0 {
                    if Buggy > 0 { line_applied } else { LineHigh }
                } else { line_applied };
                line_dirty = 1;
            }

            action SetLineUnlimited when (detached > 0 && done <= 0) {
                line_latest = 0;
                line_observed = if Buggy > 0 { line_observed } else { 0 };
                line_applied = if backend > 0 {
                    if Buggy > 0 { line_applied } else { 0 }
                } else { line_applied };
                line_dirty = 1;
            }

            action SetBudgetLow when (detached > 0 && done <= 0) {
                budget_latest = BudgetLow;
                budget_observed = if Buggy > 0 { budget_observed } else { BudgetLow };
                budget_applied = if backend > 0 {
                    if Buggy > 0 { budget_applied } else { BudgetLow }
                } else { budget_applied };
                budget_dirty = 1;
            }

            action SetBudgetHigh when (detached > 0 && done <= 0) {
                budget_latest = BudgetHigh;
                budget_observed = if Buggy > 0 { budget_observed } else { BudgetHigh };
                budget_applied = if backend > 0 {
                    if Buggy > 0 { budget_applied } else { BudgetHigh }
                } else { budget_applied };
                budget_dirty = 1;
            }

            // A replacement store is authoritative at once and takes every line
            // staged so far. Untouched settings adopt its baseline; dirty host
            // requests are applied to it. The pending snapshot stays live until the
            // stale worker resolves, so later setters still win.
            action AttachReplacement when (
                detached > 0 && done <= 0 && replacement <= 0
            ) {
                backend = 1;
                replacement = 1;
                relocated = relocated + retained;
                retained = 0;
                line_latest = if line_dirty > 0 { line_latest } else { LineHigh };
                line_observed = if line_dirty > 0 { line_latest } else { LineHigh };
                line_applied = if line_dirty > 0 { line_latest } else { LineHigh };
                ring_limit = if (
                    line_dirty > 0 && line_latest > 0 &&
                    line_latest <= RingCap - 1
                ) { line_latest } else { ring_limit };
                budget_latest = if budget_dirty > 0 {
                    budget_latest
                } else { ReplacementBudget };
                budget_observed = if budget_dirty > 0 {
                    budget_latest
                } else { ReplacementBudget };
                budget_applied = if budget_dirty > 0 {
                    budget_latest
                } else { ReplacementBudget };
            }

            // Worker finishes and re-attaches. An erase during the window drops the
            // stale pre-erase store, so nothing resurrects (Buggy re-attaches it).
            // The newest pending settings are installed before staged output drains.
            action Reattach when (detached > 0 && done <= 0) {
                detached = 0;
                done = 1;
                resurrected = if Buggy > 0 { cleared } else { 0 };
                slack = 0;
                pending = 0;
                backend = 1;
                line_observed = line_latest;
                line_applied = line_latest;
                ring_limit = if (
                    line_dirty > 0 && line_latest > 0 &&
                    line_latest <= ring_limit - 1
                ) { line_latest } else { ring_limit };
                line_dirty = 0;
                budget_observed = budget_latest;
                budget_applied = budget_latest;
                budget_dirty = 0;
            }

            // Worker died; `abort_reflow_offload` recovers to a bounded state.
            // Without a replacement, window output is discarded and nothing
            // re-attaches; a newest low finite total tightens the surviving ring
            // (high/unlimited cannot expand it) and the budget is dropped. A
            // replacement stays authoritative, receives the newest settings and
            // drains the rows staged since it attached, so that Abort is lossless.
            action Abort when (detached > 0 && done <= 0) {
                detached = 0;
                done = 1;
                aborted = 1;
                relocated = if replacement > 0 {
                    if Buggy > 0 { relocated } else { relocated + retained }
                } else { relocated };
                retained = 0;
                resurrected = 0;
                slack = if replacement > 0 { 0 } else { 3 };
                pending = 0;
                backend = replacement;
                ring_limit = if (
                    replacement <= 0 && line_dirty > 0 && line_latest > 0 &&
                    line_latest <= ring_limit - 1
                ) { line_latest } else { ring_limit };
                line_observed = if replacement > 0 {
                    line_latest
                } else {
                    if (
                        line_dirty > 0 && line_latest > 0 &&
                        line_latest <= ring_limit - 1
                    ) { line_latest } else { ring_limit }
                };
                line_applied = if replacement > 0 { line_latest } else { 0 };
                line_dirty = 0;
                budget_observed = if replacement > 0 { budget_latest } else { 0 };
                budget_applied = if replacement > 0 { budget_latest } else { 0 };
                budget_dirty = 0;
            }

            // A produced line is accounted for while staged (`retained`) or after a
            // drain relocated it into the tiered store (`relocated`). AttachReplacement
            // and replacement-backed Abort transfer staged rows; a discard in place of
            // that transfer breaks the sum.
            invariant NoLoss: produced <= retained + relocated + slack;
            // The lower bound NoLoss cannot state: over this alphabet the staging
            // buffer holds only rows produced since the last erase. Every action that
            // resets the produced debt (`Erase`) or hands staged rows on
            // (`AttachReplacement`, `Abort`) empties the buffer in the same step.
            //
            // This alphabet only: resize shedding stages rows without `Produce`, so
            // the real `lazy_buffer` can exceed the window's scroll-off. Do not lower
            // this to a `debug_assert!` on `lazy_buffer.len()` (a height shrink inside
            // a window trips it while correct). A resize step added here must bump
            // `produced` and `retained` together; bumping only `retained` refutes this.
            invariant StagedWithinProduced: retained <= produced;
            invariant ErasedStaysErased: resurrected <= 0;
            // The window closes: an Abort that forgets `detached' = 0` leaves both set.
            invariant WindowCloses: detached + done <= 1;
            invariant PendingIffUnresolved: pending == detached;
            invariant DirtyOnlyWhilePending:
                line_dirty + budget_dirty <= pending + pending;
            invariant LatestLineObservable:
                line_observed == if (pending > 0 || backend > 0) {
                    line_latest
                } else { ring_limit };
            invariant LatestBudgetObservable:
                budget_observed == if (pending > 0 || backend > 0) {
                    budget_latest
                } else { 0 };
            invariant AppliedLineLatest:
                line_applied == if backend > 0 { line_latest } else { 0 };
            invariant AppliedBudgetLatest:
                budget_applied == if backend > 0 { budget_latest } else { 0 };
            invariant ReplacementHasBackend: replacement <= backend;
            invariant RingBounded: ring_limit > 0 && ring_limit <= RingCap;
            invariant SettingsBounded:
                line_latest <= LineHigh && line_observed <= LineHigh &&
                line_applied <= LineHigh && budget_latest <= InitialBudget &&
                budget_observed <= InitialBudget && budget_applied <= InitialBudget;
        }
    }
}
