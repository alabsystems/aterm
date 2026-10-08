// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Shared model of the scrollback-offload detach window, `include!`d by both the
// compile-time gate (`build.rs`) and the conformance/lockstep tests, so both
// check the same spec. Requires `Model` + `ty_model` in scope at the include
// site.
//
// The offload detaches the tiered store, rewraps it off the lock on a worker,
// then re-attaches. While detached, concurrent steps interleave in any order:
//   Produce          — the PTY reader scrolls a line off (environment)
//   Erase            — an ED3 / `clear` lands (environment)
//   SetLine*         — a host changes the total line limit; the newest request wins
//   SetBudget*       — a host changes the byte budget; the newest request wins
//   AttachReplacement — reset/recovery installs an authoritative replacement store
//   Reattach         — the worker finishes and re-attaches (clean completion)
//   Abort            — the worker died; `abort_reflow_offload` recovers to a
//                      bounded state (window output is discarded when no
//                      replacement exists, rather than wedging)
//
// Not in this alphabet: a resize landing while the window is open. Once the
// store is out, `resize_offloading_scrollback` detaches nothing, so a further
// resize is a plain `Grid::resize` with `scrollback_detached_for_reflow` set
// (e.g. `aterm-wasm`'s `resize`, aterm-gui's `cross_resize`). Such a resize
// stages rows into `lazy_buffer` with no PTY scroll-off: the height-shrink
// front/back ring pushes in `adjust_row_count`, the viewport overflow in
// `finalize_reflow` (ungated, so it can fire in the resize that opens the
// window), and the rewrapped-ring overflow in `prepend_ring_scrollback_lines`
// (only inside a window). Adding a resize step is a model change that must be
// made together with the `StagedWithinProduced` note below.
//
// Invariants:
//   NoLoss            on a clean Reattach or replacement-backed Abort, every
//                     line produced during the window is still staged
//                     (`retained`) or relocated into the authoritative tiered
//                     store (`relocated`, the shadow counter for what
//                     `drain_lazy_buffer` moves). `slack` = W while the window is
//                     open or after a backend-less Abort, 0 after a clean
//                     Reattach / replacement Abort.
//   StagedWithinProduced: the staging buffer never holds more than the window
//                     has produced since the last erase. Scoped to this
//                     alphabet; see the note at the invariant.
//   ErasedStaysErased: an erase during the window is never resurrected.
//   PendingIffUnresolved: the detach-time settings snapshot exists for exactly
//                        the unresolved window and is consumed by Reattach/Abort.
//   Latest*Observable: getters expose the newest deferred request, including the
//                     nested `None` (encoded as line value 0 = unlimited).
//   Applied*Latest: once a backend is attached, dirty settings are applied
//                  immediately and stay latest-writer-wins; an untouched
//                  snapshot adopts a replacement backend's baseline.
//   RingBounded: a backend-less Abort applies a newest low finite total to the
//                surviving ring; a raise/unlimited request never expands it.
//
// `Buggy=0` is the shipped logic; `Buggy=1` reintroduces lost window output,
// resurrected erased history, stale deferred settings, and replacement-Abort
// staged-output loss. The gate requires a Buggy=1 counterexample so the
// invariants cannot go vacuous.

/// The detach window as a bounded state machine (see file comment above).
fn offload_window_model() -> Model {
    ty_model! {
        ScrollbackOffloadWindow {
            const W = 3;         // bounded lines that scroll off during the window
            const Buggy = 0;     // 0 = shipped fix; 1 = pre-fix bugs B + C
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
            var relocated = 0;   // ... of those, moved lazy_buffer -> re-attached store
                                 // (the drain_lazy_buffer relocation at re-attach:
                                 // safe, not loss; the shadow lane for the opaque
                                 // tiered store)
            var cleared = 0;     // a `clear` / ED3 landed during the window
            var done = 0;        // window closed (Reattach or Abort)
            var aborted = 0;     // closed via Abort (worker death) — loss accepted
            var resurrected = 0; // erased history came back after the window (bug C)
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

            // PTY output scrolls a line off during the window. Fixed: stage to the
            // lazy buffer (retained++). Buggy B: drop it once the ring is full.
            action Produce when (detached > 0 && done <= 0 && produced <= W - 1) {
                produced = produced + 1;
                retained = if Buggy > 0 { retained } else { retained + 1 };
            }

            // A `clear` / ED3 lands during the window (bumps the clear generation).
            // It also destroys the already-staged window lines (the real
            // `erase_scrollback` clears the lazy buffer), which is the user's
            // clear, not loss, so the produced/retained debt resets and NoLoss
            // then covers only post-erase output.
            action Erase when (detached > 0 && done <= 0 && cleared <= 0) {
                cleared = 1;
                produced = 0;
                retained = 0;
            }

            // Deferred line-limit changes are nested state: 0 is an explicit
            // unlimited request, not "no request". Setters keep the newest value.
            // If a replacement is already attached, apply immediately; a LOW total
            // also tightens the hot ring, while raises never grow it back.
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

            // A replacement store becomes authoritative immediately. Untouched
            // settings adopt its finite total / budget baseline; dirty host
            // requests remain authoritative and are applied to it immediately.
            // The pending snapshot remains live so setters after replacement still
            // replace earlier values until the stale worker resolves. Attaching
            // also drains every line staged so far into the replacement backend.
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

            // Worker finishes and re-attaches. An erase during the window drops
            // the stale pre-erase store, so nothing resurrects (Buggy re-attaches
            // it). Clean completion drops `slack` to 0, arming NoLoss. The newest
            // pending settings are installed before staged output drains, then the
            // snapshot/dirty bits are consumed.
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
            // Without a replacement, window output is discarded (retained = 0) and
            // nothing re-attaches. A replacement stays authoritative, receives both
            // newest settings, and drains rows staged after it attached, so that
            // Abort is lossless (`slack = 0`). Without one, a newest low finite
            // total tightens the surviving ring; high/unlimited requests cannot
            // expand it, and the backend-only budget is discarded. Either branch
            // consumes the pending snapshot once.
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

            // A produced line is accounted for while staged (`retained`) or after
            // the re-attach drain relocated it into the tiered store
            // (`relocated`). AttachReplacement and replacement-backed Abort
            // transfer staged rows (`relocated' = relocated + retained,
            // retained' = 0`); Reattach may leave safe rows staged or drain them.
            // A discard in place of that transfer breaks the sum.
            invariant NoLoss: produced <= retained + relocated + slack;
            // The lower bound NoLoss cannot state (NoLoss bounds `produced` from
            // above, so a larger `retained` only helps it): over this alphabet the
            // staging buffer holds exactly the rows produced since the last erase.
            // Every action that resets the produced debt (`Erase`) or hands staged
            // rows on (`AttachReplacement`, `Abort`) must empty the buffer in the
            // same step; an erase leaving staged rows in `lazy_buffer` would let a
            // later drain re-enter erased history, invisibly to NoLoss.
            //
            // Scope: this holds over states reachable by the actions above, not
            // over every production detach-window state. The real `lazy_buffer`
            // can exceed the window's PTY scroll-off, because resize shedding
            // stages rows without `Produce` (see the file header; the grid can even
            // enter the window with `retained > 0 == produced`). `produced` counts
            // rows that entered the window's staging obligation, and `Produce` is
            // the only action here that creates one. So:
            //   * never lower this to a runtime `debug_assert!` on
            //     `lazy_buffer.len()`: a height shrink during a reflow window would
            //     trip it while doing the right thing;
            //   * a resize/shed action added later must be Produce-like, incrementing
            //     `produced` and `retained` together (a shed row carries the same
            //     preserve-to-history obligation); bumping only `retained` would make
            //     ty refute this invariant and point at deleting a real fix.
            invariant StagedWithinProduced: retained <= produced;
            invariant ErasedStaysErased: resurrected <= 0;
            // The window closes: detached and done are never both set. An Abort
            // that fails to clear `detached` yields (detached=1, done=1); a
            // `abort_reflow_offload` that forgets `detached' = 0` fails the build.
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
