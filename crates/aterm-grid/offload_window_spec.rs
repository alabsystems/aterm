// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// SHARED model of the scrollback-offload DETACH WINDOW, `include!`d by BOTH the
// compile-time gate (`build.rs`) and the conformance/lockstep tests — ONE source of
// truth, so the gate and the code-binding cannot check different specs. Requires
// `Model` + `ty_model` in scope at the include site.
//
// The offload detaches the tiered store, rewraps it OFF the lock on a worker, then
// re-attaches. While detached, concurrent steps interleave in ALL orderings:
//   Produce          — the PTY reader scrolls a line off (environment)
//   Erase            — an ED3 / `clear` lands (environment)
//   SetLine*         — a host changes the total line limit; the newest request wins
//   SetBudget*       — a host changes the byte budget; the newest request wins
//   AttachReplacement — reset/recovery installs an authoritative replacement store
//   Reattach         — the worker finishes and re-attaches (clean completion)
//   Abort            — the worker DIED; `abort_reflow_offload` recovers to a bounded
//                      state (window output discarded BY DESIGN when no replacement
//                      exists — loss is accepted on abort, rather than wedging)
//
// NOT IN THIS ALPHABET — the honest scope line, and the one every invariant below
// inherits: a RESIZE that lands while the window is open. It is reachable, and by
// design: once the store is out, `resize_offloading_scrollback` detaches nothing and
// returns `None`, so a further resize is a plain `Grid::resize` running with
// `scrollback_detached_for_reflow == true` (see the "width re-resize while stashed"
// note in `aterm-wasm`'s `resize`, and `cross_resize` in aterm-gui, which rewraps
// off-lock while main/PTY keep taking the same `term` mutex). Such a resize SHEDS
// rows straight into `lazy_buffer` with NO PTY scroll-off:
//   reflow.rs:391 / :425        height shrink — front/back ring rows, gated on
//                               `scrollback.is_some() || detached_for_reflow`
//   reflow.rs:622               viewport overflow in `finalize_reflow` — UNGATED, so
//                               it also fires in the resize that OPENS the window,
//                               i.e. the real grid can ENTER the window with rows
//                               already staged (measured: 9 on a 10x80 grid of
//                               wrapped lines, before any Produce)
//   scrollback_reflow.rs:137    rewrapped-ring overflow past `max_scrollback` — gated
//                               ON the detach flag, so it fires ONLY inside a window
// (measured: a bare height shrink inside an open window stages 14 rows at
// produced == 0.) Adding a resize step to this alphabet is a MODEL CHANGE that must
// be made together with the `StagedWithinProduced` note below — a counterexample
// produced by adding it naively is a modeling artifact, not a code bug.
//
// Invariants (found by the adversarial audit; L1 history-integrity class):
//   NoLoss            (bug B): on a clean Reattach OR replacement-backed Abort,
//                     every line produced during the window is SAFE — still staged
//                     (`retained`) or relocated into the authoritative tiered store
//                     (`relocated`, the shadow counter for the opaque store lane the
//                     real `drain_lazy_buffer` moves lines into). `slack` = W while
//                     the window is open or after a backend-less Abort, 0 after a
//                     clean Reattach/replacement Abort — exact whenever a backend can
//                     preserve output, honestly waived only when the worker/store died.
//   StagedWithinProduced (bug C through the staging buffer): the staging buffer never
//                     holds MORE than the window has produced since the last erase.
//                     SCOPED to this alphabet — it is not a claim about the real
//                     `lazy_buffer` vs PTY scroll-off; read its note at the invariant
//                     itself before deriving anything from it.
//   ErasedStaysErased (bug C): an erase during the window is never resurrected.
//   PendingIffUnresolved: the detach-time settings snapshot exists for exactly the
//                        unresolved window and is consumed by Reattach/Abort.
//   Latest*Observable: getters expose the newest deferred request, including the
//                     nested `None` (encoded as line value 0 = unlimited).
//   Applied*Latest: once a backend is attached, dirty settings are applied
//                  immediately and remain latest-writer-wins. An untouched snapshot
//                  adopts a replacement backend's baseline instead.
//   RingBounded: a backend-less Abort applies a newest LOW finite total to the
//                surviving ring, but a raise/unlimited request never expands it.
//
// `Buggy=0` is the shipped fix; `Buggy=1` reintroduces bugs B + C, stale deferred
// settings, and replacement-Abort staged-output loss (must be CAUGHT — the gate
// also requires a Buggy=1 counterexample, so the invariants can't go vacuous).

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
                                 // (the drain_lazy_buffer RELOCATE at re-attach: still
                                 // SAFE, not loss — the shadow lane for the opaque
                                 // tiered store the projection cannot see into)
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
            // The erase also destroys the ALREADY-STAGED window lines (the real
            // `erase_scrollback` clears the lazy buffer) — that is the user's clear,
            // not loss — so the produced/retained "debt" resets: NoLoss thereafter
            // covers only post-erase output.
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

            // Worker finishes; re-attach applies the fix logic. Fixed: an erase during
            // the window drops the stale pre-erase store — nothing resurrects. Buggy C:
            // re-attaches the pre-erase store even though it was cleared. Clean
            // completion drops `slack` to 0, arming NoLoss exactly here. The newest
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

            // Worker died; `abort_reflow_offload` recovers to a bounded state. Window
            // output is discarded BY DESIGN (retained = 0) and nothing re-attaches
            // when no replacement exists (no resurrection possible). A replacement
            // stays authoritative, receives both newest settings, and drains any
            // rows staged after it attached — so replacement-backed Abort is a
            // LOSSLESS close (`slack = 0`). Without one, a newest LOW finite total
            // tightens the surviving ring; high/unlimited requests cannot expand
            // it, and the backend-only budget is discarded. Either branch consumes
            // the pending snapshot exactly once.
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

            // A produced line is accounted for while staged (`retained`) OR after the
            // re-attach drain relocated it into the tiered store (`relocated`). The
            // AttachReplacement and replacement-backed Abort conservingly transfer
            // staged rows (`relocated' = relocated + retained, retained' = 0`);
            // Reattach may leave safe rows staged or drain them in the full-verbatim
            // production body. A discard in place of that transfer breaks the sum.
            invariant NoLoss: produced <= retained + relocated + slack;
            // The LOWER bound `NoLoss` structurally cannot state (it bounds
            // `produced` from ABOVE, so a LARGER `retained` only makes it easier to
            // satisfy): over THIS alphabet the staging buffer holds exactly the rows
            // produced since the last erase, so it can never hold MORE. Every action
            // that resets the produced debt (`Erase`) or hands the staged rows on
            // (`AttachReplacement`, `Abort`) must empty the buffer in the SAME step.
            // An erase that bumps the clear generation but leaves the staged rows in
            // `lazy_buffer` breaks this — those rows are ERASED history a later drain
            // re-enters into the store, and they are invisible to `NoLoss`.
            //
            // SCOPE — TRUE STATEMENT, read it before deriving from this. It holds
            // over every state reachable by the actions above, and NOT over every
            // production-reachable detach-window state. It is NOT the claim that the
            // real `lazy_buffer` never exceeds the window's PTY scroll-off count:
            // that claim is FALSE, because resize/reflow shedding stages rows with no
            // `Produce` (four sites, with file:line and measured counts, under "NOT
            // IN THIS ALPHABET" in the file header — one of them can even stage rows
            // during the resize that OPENS the window, so the real grid may enter at
            // `retained > 0 == produced`). b71e02c8's commit message called this "one
            // the real code has always guaranteed"; that sentence overclaims and this
            // note supersedes it. What is actually guaranteed is the abstraction:
            // `produced` counts rows that entered the window's staging OBLIGATION,
            // and `Produce` is the only action here that creates one. Two rules follow:
            //   * NEVER lower this to a runtime `debug_assert!` on
            //     `lazy_buffer.len()`. It would fire on a height shrink during a
            //     reflow window — which is the audit-bug-B FIX doing its job, not a
            //     defect.
            //   * A resize/shed action added later MUST be Produce-LIKE: it increments
            //     `produced` AND `retained` in the same step, because a shed row
            //     carries the identical preserve-to-history obligation (that is also
            //     what keeps `NoLoss` honest about it). One that bumps only `retained`
            //     makes ty refute this invariant, and the gate would then prescribe
            //     deleting a real fix.
            invariant StagedWithinProduced: retained <= produced;
            invariant ErasedStaysErased: resurrected <= 0;
            // The window CLOSES: detached and done are never both set. This is the
            // invariant the abort-wedge violates at the MODEL level (an Abort that
            // fails to clear `detached` yields the state (detached=1, done=1)) — the
            // extraction RFC's Tier-A target: a wedged `abort_reflow_offload` derives
            // an Abort action with no `detached' = 0` update, and ty fails the BUILD.
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
