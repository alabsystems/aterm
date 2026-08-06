---------------------------- MODULE ResizeThrottle ----------------------------
\* Proof-Carrying Performance: the live-resize throttle (v0.5.4).
\*
\* MODELS the leading-edge throttle + trailing settle in
\* crates/aterm-gui/src/app_config.rs (`on_resize_throttled` / `flush_pending_resize`)
\* and crates/aterm-gui/src/main.rs (`RESIZE_THROTTLE` / `next_resize_settle`). A live
\* window width/corner drag emits a `Resized` per ~cell-width, and each WIDTH change
\* rewraps the entire off-screen scrollback (`reflow_scrollback_lines`) on the
\* event-loop thread — so without throttling the drag hitches.
\*
\* THE BUG (old code, Buggy = 1): apply (reflow) EVERY resize immediately — so two
\* reflows can land arbitrarily close together (the per-cell-width hitch).
\*
\* THE FIX (Buggy = 0): apply the FIRST resize of a drag immediately, then coalesce
\* further ones into a `pending` size and apply only the latest on a trailing settle
\* armed at `lastApply + Throttle`. The leading-edge apply is gated on
\* `now - lastApply >= Throttle`; the settle fires at `now >= lastApply + Throttle`.
\*
\* THE EFFICIENCY THEOREM (`BoundedReflowRate`): no two LIVE-RESIZE-driven reflows (the
\* `WindowEvent::Resized` path through `on_resize_throttled`) are applied within one
\* `Throttle` window — so a big-scrollback width drag cannot hitch on a per-cell-width
\* rewrap. Proven by exhaustive BFS at Buggy = 0; a counterexample (two such reflows too
\* close) is caught at Buggy = 1 (the Buggy-constant prove-and-catch convention).
\*
\* HONEST SCOPE (do not overclaim):
\*   - LIVE-RESIZE ONLY. The bound is over the `WindowEvent::Resized` reflow path. OUT-OF-
\*     BAND reflows that legitimately bypass the throttle call `on_resize` /
\*     `apply_term_resize` directly WITHOUT updating `last_resize_at`, and are NOT
\*     throttled (nor should they be); they can land within a Throttle window of a drag
\*     reflow. Out of scope: the control-socket `resize` verb (`apply_term_resize`), the
\*     HUD/panel-toggle re-grid (`app_config.rs:1036`), a scale-factor change
\*     (`:1124`), and a config/font reload (`:1303`). (`apply_term_resize` clears the
\*     drag's `pending_resize` on such a reflow so a STALE size is never re-applied — a
\*     separate correctness property, not this rate bound.)
\*   - RATE, not COST. This bounds reflows to <= 1 per `Throttle` window; it does NOT bound
\*     the per-reflow cost of `reflow_scrollback_lines` over a huge scrollback (a single
\*     reflow can still be expensive).
\*   - PER-WINDOW, conservative. The model treats every gate-passing REFLOWING `Resized`
\*     as a reflow; the code's `apply_term_resize` early-returns on an unchanged grid
\*     (sub-cell drag), so the model OVER-counts reflows — the bound is a safe upper bound.
\*   - REFLOW IS A WIDTH CHANGE. `on_resize_throttled` gates on `resize_changes_columns`:
\*     rewrapping is what reflow MEANS, and only a column change rewraps. A row-only drag
\*     (the bottom edge) adds or drops grid rows and rewraps nothing, so it is applied
\*     immediately rather than coalesced — it made the terminal body trail the window edge
\*     by up to a Throttle window for no work saved. The model carries that distinction as
\*     two `Resized` disjuncts — `ResizedReflowing` and `ResizedRowOnly`, either available
\*     at any step, as a real drag mixes them — so the theorem still binds exactly the
\*     events that carry a rewrap.
\*
\*     BE CLEAR ABOUT WHAT THAT SPLIT DOES AND DOES NOT BUY. `ResizedRowOnly` adds NO
\*     proof obligation. It is unconditionally enabled and every one of its effects
\*     makes `BoundedReflowRate` strictly easier to satisfy — it leaves `gapViolated`
\*     UNCHANGED, advances `lastApply` (blocking the reflowing apply arm via `TooSoon`),
\*     and clears `pending` (disabling `Settle`). So it EXTENDS the model's vocabulary
\*     to the new code path without CHECKING it: this file proves the rate bound still
\*     holds in the presence of row-only applies, and nothing more. The properties the
\*     row-only path actually risks — that a coalesced width size is not dropped, and
\*     that the active-tab-only scope's `panes_stale` debt always has a payer — are
\*     about `resize_live_drag` / `panes_stale` / `pending_resize`, which this model has
\*     no variables for. They are held by code and by the `aterm-gui` tests, NOT by this
\*     certificate. A future revision wanting real coverage needs a `debtOutstanding`
\*     variable and an invariant that it cannot survive a cleared settle.
\*
\*     A non-reflowing apply DOES advance
\*     `lastApply` (the code stamps `last_resize_at` on every applied resize), which can
\*     only DELAY the next reflow — conservative for this bound, never permissive.
\*
\* Bounded vocabulary: discrete time `now` in 0..MaxTime, `Throttle` abstract units. The
\* bound is on the time DIFFERENCE `now - lastApply`, so it is shift-invariant and the
\* small horizon is adequate; the check is exhaustive over that bounded set.

EXTENDS Naturals

CONSTANTS
  Buggy,     \* 0 = throttle (leading-edge + trailing settle); 1 = old code (reflow every resize)
  Throttle,  \* RESIZE_THROTTLE — the minimum time between two applied reflows
  MaxTime    \* BFS time horizon (keeps the reachable-state set finite)

VARIABLES
  now,          \* current discrete time
  lastApply,    \* time of the last APPLIED resize (a scrollback reflow)
  applied,      \* has any resize been applied yet? (the first apply has no predecessor)
  pending,      \* a coalesced resize awaits the trailing settle (throttle path only)
  settle,       \* the trailing-settle deadline (meaningful iff `pending`)
  gapViolated   \* TRUE iff some apply landed within `Throttle` of the previous one

vars == << now, lastApply, applied, pending, settle, gapViolated >>

\* A reflow applied now would land too soon after the previous one (rate-bound break).
\* The FIRST apply (applied = FALSE) has no predecessor, so it is always allowed.
TooSoon == applied /\ (now - lastApply < Throttle)

Init ==
  /\ now = 0
  /\ lastApply = 0
  /\ applied = FALSE
  /\ pending = FALSE
  /\ settle = 0
  /\ gapViolated = FALSE

\* A live window-resize event arrives at `now` and WOULD CHANGE THE COLUMN COUNT — a
\* rewrap, i.e. a reflow. This is the `resize_changes_columns = TRUE` arm of
\* `on_resize_throttled`, and the only arm the rate bound is about.
ResizedReflowing ==
  IF Buggy = 1
    THEN \* OLD CODE: reflow every resize immediately — no throttle.
      /\ gapViolated' = (gapViolated \/ TooSoon)
      /\ applied' = TRUE
      /\ lastApply' = now
      /\ pending' = FALSE
      /\ UNCHANGED << now, settle >>
    ELSE \* THROTTLE: leading-edge apply iff one Throttle has elapsed (or it's the
         \* first), else COALESCE into `pending` and arm the trailing settle.
      IF ~applied \/ now - lastApply >= Throttle
        THEN /\ gapViolated' = (gapViolated \/ TooSoon) \* the guard makes this FALSE on apply
             /\ applied' = TRUE
             /\ lastApply' = now
             /\ pending' = FALSE
             /\ UNCHANGED << now, settle >>
        ELSE /\ pending' = TRUE
             /\ settle' = lastApply + Throttle
             /\ UNCHANGED << now, lastApply, applied, gapViolated >>

\* A live window-resize event that leaves the COLUMN COUNT alone — a bottom-edge /
\* height-only drag step. It rewraps nothing, so it is not a reflow: it is applied
\* immediately under both Buggy settings and contributes nothing to the reflow rate
\* (`gapViolated` UNCHANGED). It does stamp `lastApply`, exactly as the code stamps
\* `last_resize_at` on every applied resize — which can only DELAY the next reflow,
\* so it is conservative for this bound and never permissive.
\*
\* Clearing `pending` here mirrors the code and loses no width: `resize_changes_columns`
\* compares against the grid ACTUALLY APPLIED, so "columns unchanged" means the window's
\* live width already agrees with the current grid, and the coalesced older size it
\* discards is stale. (That a stale size is never re-applied is a separate correctness
\* property, as the out-of-band note above already records; dropping reflows can only
\* lower a rate, so it cannot break this upper bound either way.)
ResizedRowOnly ==
  /\ applied' = TRUE
  /\ lastApply' = now
  /\ pending' = FALSE
  /\ UNCHANGED << now, settle, gapViolated >>

\* The trailing settle fires: apply the coalesced (final) size once `now` reaches it.
\* It fires at `now >= settle = lastApply + Throttle`, so the gap is always respected.
\* (The real `flush_pending_resize` re-samples `Instant::now()` at apply time rather than
\* reusing the gate instant, so the actual gap is `>= Throttle` — conservative, fail-safe.)
Settle ==
  /\ pending
  /\ now >= settle
  /\ gapViolated' = (gapViolated \/ TooSoon) \* now >= lastApply + Throttle ⇒ FALSE
  /\ applied' = TRUE
  /\ lastApply' = now
  /\ pending' = FALSE
  /\ UNCHANGED << now, settle >>

Tick ==
  /\ now < MaxTime
  /\ now' = now + 1
  /\ UNCHANGED << lastApply, applied, pending, settle, gapViolated >>

\* A drag mixes width and height steps freely, so the two `Resized` shapes are plain
\* disjuncts and BFS covers every interleaving of them.
Next == ResizedReflowing \/ ResizedRowOnly \/ Settle \/ Tick

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ now \in 0..MaxTime
  /\ lastApply \in 0..MaxTime
  /\ applied \in BOOLEAN
  /\ pending \in BOOLEAN
  /\ settle \in 0..(MaxTime + Throttle)
  /\ gapViolated \in BOOLEAN

\* THE EFFICIENCY THEOREM: no two LIVE-RESIZE-driven reflows are applied within one
\* `Throttle` window — the live-resize reflow RATE is bounded (out-of-band reflows and
\* per-reflow cost are out of scope; see the header). Holds at Buggy = 0 (the throttle);
\* violated (counterexample: two reflows too close) at Buggy = 1 (the old per-resize code).
BoundedReflowRate == ~gapViolated
=============================================================================
