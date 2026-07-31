// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Native window / tab / pane GUI models — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// TAB NAVIGATION — the GUI per-window tab-strip index machine (`TabIndex` in
/// `aterm-gui`: `{ active, count }`). One window always holds at least one tab and
/// the renderer reads `active` as an index into the live tab list, so the contract
/// is exactly: `count >= 1` AND `active <= count - 1` (with `usize`, `active >= 0`
/// is trivial). This is the smallest faithful abstraction of the four shipping
/// mutators, bounded by `Cap` tabs so `ty` explores a finite space:
///
///   * `NewTab`    ⟵ `TabIndex::add()`: append a tab and switch to it. `count' =
///     count + 1` and the new tab is the new LAST index, `active' =
///     count` (== new `count - 1`). Guarded `count <= Cap - 1`.
///   * `SelectTab` ⟵ `TabIndex::switch_to(i)` for an in-range `i` (Cmd-1..9 /
///     `switch_tab_in`): jump to ANY valid index. The specific `i` is
///     user input, not a function of the scalar projection, so the
///     faithful update is NONDETERMINISTIC: `active' \in 0..count-1`.
///     `ty` checks the whole fan-out, and the real in-range
///     `switch_to` lands on one such admissible value.
///   * `Cycle`     ⟵ `TabIndex::cycle(true)` (Cmd-Shift-]): forward with WRAP.
///     `(active + 1) % count` has no `%` in the macro algebra, but the
///     invariant `active <= count - 1` makes it exactly `active' = IF
///     active + 1 > count - 1 THEN 0 ELSE active + 1`. Guarded
///     `count > 1` (one tab is a no-op).
///   * `Close`     ⟵ `TabIndex::close(i)` for a non-exit close (`count > 1`, so a
///     window keeps >= 1 tab): `count' = count - 1`, then RE-CLAMP the
///     active index into the shrunk range. The worst case for the
///     range invariant is closing the LAST (active) tab, where active
///     must drop to the new last index `count - 2`; the faithful
///     re-clamp is `active' = IF active > count - 2 THEN count - 2 ELSE
///     active` (= `min(active, new_count - 1)`, matching `close`'s
///     `else if active >= count { active = count - 1 }` arm). Guarded
///     `count > 1`.
///
/// **`Buggy` non-vacuity control.** At `Buggy = 0` `ty` PROVES `CountPositive` +
/// `ActiveInRange` over the whole bounded space. The `Buggy` branch in `Close`
/// FORGETS the re-clamp (`active' = active`), so closing the last/active tab leaves
/// `active = count - 1` while the range shrank to `count - 2` — `active > count - 1`
/// after the step — and `ty` at `Buggy = 1` MUST yield a counterexample to
/// `ActiveInRange`. That is the exact "renderer indexes a tab that no longer
/// exists" defect the clamp prevents.
///
/// Hand-built (not via `ty_model!`) because `SelectTab` needs a NONDETERMINISTIC
/// in-range update (`in_range`), which the light-annotation macro does not surface
/// — same reason as [`window_routing_model`].
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn tab_nav_model() -> Model {
    Model {
        name: "TabNav",
        // Bound the tab count so `ty` explores a finite space (a window with up to
        // Cap tabs). `Buggy` flips the Close re-clamp off.
        consts: vec![("Cap", 4), ("Buggy", 0)],
        // A fresh window: one tab (count=1), it is active (active=0).
        vars: vec![
            StateVar {
                name: "count",
                init: 1,
            },
            StateVar {
                name: "active",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            // add(): append a tab and switch to it — the new tab is the new LAST
            // index. All RHS evaluate against the pre-state, so `active' = count`
            // (old count == new `count' - 1`) and `count' = count + 1`.
            Action {
                name: "NewTab",
                guard: Some(le(var("count"), sub(cst("Cap"), int(1)))),
                updates: vec![
                    Update {
                        var: "active",
                        expr: var("count"),
                    },
                    Update {
                        var: "count",
                        expr: add(var("count"), int(1)),
                    },
                ],
            },
            // switch_to(i) for an in-range i (Cmd-1..9 / switch_tab_in): jump to ANY
            // valid index. The specific `i` is user input, not a function of the
            // scalar projection, so the faithful update is NONDETERMINISTIC:
            // `active' \in 0..(count - 1)`. `ty` checks the whole fan-out; the real
            // in-range `switch_to` lands on one such admissible value.
            Action {
                name: "SelectTab",
                guard: Some(gt(var("count"), int(1))),
                updates: vec![Update {
                    var: "active",
                    expr: in_range(int(0), sub(var("count"), int(1))),
                }],
            },
            // cycle(true) (Cmd-Shift-]): forward with WRAP. `(active + 1) % count`
            // has no `%` in this algebra, but the invariant `active <= count - 1`
            // makes it exactly `active' = IF active + 1 > count - 1 THEN 0 ELSE
            // active + 1`. Guarded `count > 1` (one tab is a no-op).
            Action {
                name: "Cycle",
                guard: Some(gt(var("count"), int(1))),
                updates: vec![Update {
                    var: "active",
                    expr: if_(
                        gt(add(var("active"), int(1)), sub(var("count"), int(1))),
                        int(0),
                        add(var("active"), int(1)),
                    ),
                }],
            },
            // close(i) for a non-exit close (count > 1, so the window keeps >= 1 tab):
            // `count' = count - 1`, then RE-CLAMP active into the shrunk range. The
            // worst case for the range invariant is closing the LAST (active) tab,
            // where active must drop to the new last index `count - 2`; the faithful
            // re-clamp is `active' = IF active > count - 2 THEN count - 2 ELSE active`
            // (= min(active, new_count - 1), matching `close`'s `else if active >=
            // count { active = count - 1 }` arm). The `Buggy` branch FORGETS the
            // clamp (`active' = active`), so closing the last/active tab leaves
            // `active = count - 1` while the range shrank to `count - 2`.
            Action {
                name: "Close",
                guard: Some(gt(var("count"), int(1))),
                updates: vec![
                    Update {
                        var: "active",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            var("active"),
                            if_(
                                gt(var("active"), sub(var("count"), int(2))),
                                sub(var("count"), int(2)),
                                var("active"),
                            ),
                        ),
                    },
                    Update {
                        var: "count",
                        expr: sub(var("count"), int(1)),
                    },
                ],
            },
        ],
        invariants: vec![
            // A window always has at least one tab.
            Invariant {
                name: "CountPositive",
                expr: gt(var("count"), int(0)),
            },
            // The active index is always in range for the renderer (active <= count-1).
            Invariant {
                name: "ActiveInRange",
                expr: le(var("active"), sub(var("count"), int(1))),
            },
        ],
    }
}

/// SPLIT-PANE TREE INTEGRITY — a tab's `PaneTree` (aterm-gui `pane.rs`) always keeps
/// at least one leaf while the tab is open, and the FOCUSED leaf index never leaves
/// the renderer's `0..leaf_count-1` range. This holds the split-pane feature
/// (Cmd-D / Cmd-Shift-D split, Cmd-W / EOF close) to the same Trust bar as tabs:
/// input + the solid cursor never route to a pane that no longer exists.
///
/// `Buggy` gates the Close re-point: at `Buggy = 0` a Close that removes the focused
/// last leaf drops `focused` to the new last index; at `Buggy = 1` it FORGETS the
/// re-point, leaving `focused = leaf_count` one past the shrunk end (the dangling-
/// focus defect). So `ty` PROVES `FocusInRange` (Buggy=0) and CATCHES it (Buggy=1 →
/// counterexample). SCOPE: only the `CloseOutcome::Collapsed` arm (the tab survives);
/// a `LastPane` close is a tab-machine transition (`tab_nav` / `window_routing`).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn pane_tree_model() -> Model {
    Model {
        name: "PaneTree",
        // Bound the leaf count so `ty` explores a finite space (a tab with up to
        // Cap split panes). `Buggy` flips the Close re-point off.
        consts: vec![("Cap", 4), ("Buggy", 0)],
        // A fresh tab: one leaf (leaf_count=1), it is focused (focused=0).
        vars: vec![
            StateVar {
                name: "leaf_count",
                init: 1,
            },
            StateVar {
                name: "focused",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            // split_focused(dir, new): the focused leaf becomes a Split of (original,
            // new); the new pane is the SECOND child -> the new LAST leaf in tree
            // order, and focus moves to it. RHS read the pre-state, so focused' =
            // leaf_count and leaf_count' = leaf_count + 1. Guarded leaf_count <= Cap-1.
            Action {
                name: "Split",
                guard: Some(le(var("leaf_count"), sub(cst("Cap"), int(1)))),
                updates: vec![
                    Update {
                        var: "focused",
                        expr: var("leaf_count"),
                    },
                    Update {
                        var: "leaf_count",
                        expr: add(var("leaf_count"), int(1)),
                    },
                ],
            },
            // close_pane on a SPLIT tab (CloseOutcome::Collapsed, leaf_count > 1):
            // leaf_count' = leaf_count - 1, then RE-POINT focused to a surviving leaf.
            // The real first_leaf/keep-focus index is not a function of the scalar
            // projection, so the faithful update is NONDETERMINISTIC: focused' \in
            // 0..(leaf_count - 2). The in_range MUST be the top-level RHS (the renderer
            // emits focused' \in lo..hi only there; nesting in an IF renders a SET as
            // an = RHS, a type error), so the Buggy flag is folded into the UPPER
            // BOUND: Buggy=0 caps at the new last index leaf_count-2; Buggy=1 stretches
            // the cap to leaf_count-1, one past the end (the forgot-to-re-point defect,
            // where closing the focused last leaf leaves focused = leaf_count - 1).
            Action {
                name: "Close",
                guard: Some(gt(var("leaf_count"), int(1))),
                updates: vec![
                    Update {
                        var: "focused",
                        expr: in_range(
                            int(0),
                            if_(
                                eq(cst("Buggy"), int(1)),
                                sub(var("leaf_count"), int(1)),
                                sub(var("leaf_count"), int(2)),
                            ),
                        ),
                    },
                    Update {
                        var: "leaf_count",
                        expr: sub(var("leaf_count"), int(1)),
                    },
                ],
            },
        ],
        invariants: vec![
            // The tab's tree is never empty while the tab is open (>= 1 leaf).
            Invariant {
                name: "TreeNonEmpty",
                expr: gt(var("leaf_count"), int(0)),
            },
            // Exactly-one in-range focused leaf: focused <= leaf_count - 1.
            Invariant {
                name: "FocusInRange",
                expr: le(var("focused"), sub(var("leaf_count"), int(1))),
            },
        ],
    }
}

/// SESSION-POOL REFCOUNT ACCOUNTING — a pooled session's bookkeeping entry exists
/// exactly while ≥1 window view references it. `refcount` is the live view count
/// (`SessionPool::views`); `closed` is whether the entry has been retired. The
/// invariant `ClosedIffEmpty` (`closed = 1  <=>  refcount = 0`) is the pool's
/// allocation discipline: a session is retired the instant — and only the instant —
/// its last viewer detaches, so the Cmd-Shift-O two-windows-one-session path
/// (refcount 2) never retires early and a fully-detached session never leaks an entry.
///
/// `Buggy = 1` retires on EVERY Release (closes while a co-viewer remains) → `ty`
/// catches the premature-retire counterexample; `Buggy = 0` retires only at
/// refcount 0. The Tier-1 conformance makes the iff NON-VACUOUS by projecting the
/// two variables from TWO INDEPENDENT real signals: `refcount` from the actual count
/// of canonical terminal view edges (recomputed across every live tab/split, including
/// multiple shared views migrated into one window) and `closed` from pool membership
/// (`views(sid).is_none()`).
/// So a pool that RETIRES a session — dropping its `Session`, closing the PTY — while
/// a canonical view still references it projects to `[refcount>0, closed=1]`, which `ty`
/// rejects (the use-after-free-on-the-pooled-session hazard). (PTY-fd liveness past
/// the pool entry is a further `SinkWriter`-Arc concern, def2bac — out of scope here.)
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_pool_model() -> Model {
    Model {
        name: "SessionPool",
        consts: vec![("Cap", 4), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "refcount",
                init: 1,
            },
            StateVar {
                name: "closed",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            // attach (a 2nd+ window views the same session, Cmd-Shift-O): refcount+1,
            // guarded not-yet-closed and below the model bound.
            Action {
                name: "Acquire",
                guard: Some(and_(
                    eq(var("closed"), int(0)),
                    le(var("refcount"), sub(cst("Cap"), int(1))),
                )),
                updates: vec![Update {
                    var: "refcount",
                    expr: add(var("refcount"), int(1)),
                }],
            },
            // detach (a window stops viewing): refcount-1; retire (closed=1) IFF that
            // was the last viewer. Buggy retires on every detach.
            Action {
                name: "Release",
                guard: Some(gt(var("refcount"), int(0))),
                updates: vec![
                    Update {
                        var: "refcount",
                        expr: sub(var("refcount"), int(1)),
                    },
                    Update {
                        var: "closed",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            int(1),
                            if_(
                                eq(sub(var("refcount"), int(1)), int(0)),
                                int(1),
                                var("closed"),
                            ),
                        ),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "ClosedIffEmpty",
            expr: and_(
                or_(eq(var("closed"), int(0)), eq(var("refcount"), int(0))),
                or_(eq(var("closed"), int(1)), gt(var("refcount"), int(0))),
            ),
        }],
    }
}

/// TAB-STRIP PARITY — the NATIVE macOS titlebar tab strip (`toolbar.rs`'s
/// `NSSegmentedControl`) can never DESYNC from the proven tab model. The strip is a
/// pure MIRROR of the tab set: its `segmentCount` must always equal the tab `count`
/// and its `selectedSegment` the `active` tab, and — since AppKit will index it — the
/// selection must stay in range (`selected <= seg_count - 1`). The sink maintaining
/// the mirror is `set_window_tabs(handle, titles, active)`, driven from
/// `App::refresh_window_tabs` after EVERY tab mutation.
///
/// Two-lane self-composition (cf. [`coalesce_model`]): one event stream drives BOTH a
/// TRUTH lane `(count, active)` (the `TabIndex` machine of [`tab_nav_model`]) and a
/// STRIP lane `(seg_count, selected)` (the control), re-synced to mirror the truth
/// after each action. `ty` PROVES `StripMirrorsTruth` @Buggy=0; the Buggy branch in
/// Close DROPS the strip re-sync (a missed `refresh_window_tabs`), freezing BOTH strip
/// vars stale — so the strip shows an extra segment with an out-of-range selection,
/// caught @Buggy=1.
///
/// Tier-1: bound by `tab_strip_conformance` (aterm-gui), which projects the TRUTH lane
/// `(count, active)` from `ws.tabs` AND the STRIP lane `(seg_count, selected)` from
/// `WindowState::strip_shadow` — a faithful record of what `refresh_window_tabs` last
/// pushed to the native `NSSegmentedControl`. The two signals are INDEPENDENT (a tab
/// mutation that forgets to re-sync a window's strip leaves the shadow stale), so the
/// load-bearing case — closing a tab in a NON-FRONT window — is a real, ty-rejected
/// desync unless `close_tab_at` re-syncs THAT window's strip (the fix this drove).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn tab_strip_model() -> Model {
    // Re-clamp of the active/selected index after a Close shrinks the count by one:
    // min(idx, new_count - 1) = IF idx > count - 2 THEN count - 2 ELSE idx (RHS reads
    // the PRE-state count, so new_count - 1 = count - 2). Used for the TRUTH lane's
    // active' and, in the correct path, the STRIP lane's selected'.
    let reclamp = || {
        if_(
            gt(var("active"), sub(var("count"), int(2))),
            sub(var("count"), int(2)),
            var("active"),
        )
    };
    Model {
        name: "TabStrip",
        // Bound the tab count so `ty` explores a finite space. `Buggy` flips the Close
        // strip re-sync off (the forgot-to-refresh defect).
        consts: vec![("Cap", 4), ("Buggy", 0)],
        vars: vec![
            // TRUTH lane (the TabIndex machine).
            StateVar {
                name: "count",
                init: 1,
            },
            StateVar {
                name: "active",
                init: 0,
            },
            // STRIP lane (the NSSegmentedControl mirror).
            StateVar {
                name: "seg_count",
                init: 1,
            },
            StateVar {
                name: "selected",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            // open_tab_in -> TabIndex::add(): append + switch, then refresh re-syncs the
            // strip verbatim. active' = count (the new last index); strip mirrors.
            Action {
                name: "NewTab",
                guard: Some(le(var("count"), sub(cst("Cap"), int(1)))),
                updates: vec![
                    Update {
                        var: "active",
                        expr: var("count"),
                    },
                    Update {
                        var: "count",
                        expr: add(var("count"), int(1)),
                    },
                    Update {
                        var: "selected",
                        expr: var("count"),
                    },
                    Update {
                        var: "seg_count",
                        expr: add(var("count"), int(1)),
                    },
                ],
            },
            // switch_tab_in / cycle_tab: move active, then refresh re-syncs the strip to
            // the NEW active — both lanes move in LOCKSTEP. Modelled as the deterministic
            // cycle(true) wrap (a genuine shipping transition); selected' mirrors the
            // SAME pre-state expression so the lanes never diverge spuriously.
            Action {
                name: "SelectTab",
                guard: Some(gt(var("count"), int(1))),
                updates: vec![
                    Update {
                        var: "active",
                        expr: if_(
                            gt(add(var("active"), int(1)), sub(var("count"), int(1))),
                            int(0),
                            add(var("active"), int(1)),
                        ),
                    },
                    Update {
                        var: "selected",
                        expr: if_(
                            gt(add(var("active"), int(1)), sub(var("count"), int(1))),
                            int(0),
                            add(var("active"), int(1)),
                        ),
                    },
                ],
            },
            // close_tab_at -> TabIndex::close(i), non-exit (count > 1): count' = count-1,
            // active re-clamped; the strip is then re-synced (seg_count' = count',
            // selected' = active'). The Buggy branch DROPS that re-sync (the close forgot
            // refresh_window_tabs on a non-front window), freezing BOTH strip vars at
            // their stale pre-close values — so seg_count outlives the tab it counted and
            // selected points past the new end when the last/active tab was closed.
            Action {
                name: "Close",
                guard: Some(gt(var("count"), int(1))),
                updates: vec![
                    Update {
                        var: "active",
                        expr: reclamp(),
                    },
                    Update {
                        var: "count",
                        expr: sub(var("count"), int(1)),
                    },
                    Update {
                        var: "seg_count",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            var("seg_count"),
                            sub(var("count"), int(1)),
                        ),
                    },
                    Update {
                        var: "selected",
                        expr: if_(eq(cst("Buggy"), int(1)), var("selected"), reclamp()),
                    },
                ],
            },
        ],
        invariants: vec![
            // The native strip can never desync from the proven tab model: its segment
            // count mirrors the tab count, its selection mirrors the active tab, and the
            // selection is a valid segment index AppKit can highlight.
            Invariant {
                name: "StripMirrorsTruth",
                expr: and_(
                    and_(
                        eq(var("seg_count"), var("count")),
                        eq(var("selected"), var("active")),
                    ),
                    le(var("selected"), sub(var("seg_count"), int(1))),
                ),
            },
        ],
    }
}

/// The GLOBAL control-socket `ActiveHandle` mirror (the `active_handle` in aterm-gui's
/// `App`). The control socket has ONE global handle that introspection/drive verbs
/// (`text`/`feed`/`signal`) resolve through (`resolve_active`); it MUST always name the
/// session the user is actually looking at — the FRONTMOST window's active tab's
/// focused pane (the TRUTH lane). The window analog of [`tab_strip_model`]'s
/// per-window strip mirror: same two-lane (truth/mirror) parity discipline, but for the
/// PROCESS-WIDE control target rather than the native chrome.
///
/// `ty` PROVES `HandleMirrorsFront` at `Buggy=0` — every path that moves the front
/// window's active session ALSO re-points the global handle (the
/// `resync_active_or_window` -> `sync_active_session` discipline), so the two lanes
/// never diverge under ANY interleaving of front-active changes — and CATCHES the
/// "swallow class" at `Buggy=1` (a close-collapse / new-window path that re-mirrors only
/// the PER-WINDOW state via `sync_window` and forgets the global re-point) ->
/// counterexample on `HandleMirrorsFront`. That is exactly the defect class fixed by
/// routing `apply_close_outcome` / `create_window_internal` / `push_stub_tab` through
/// `resync_active_or_window`: without it the control socket keeps driving a stale, or
/// just-closed, session — and `Owner`/aterm-ctl verbs bypass the per-request edge gate,
/// so they hit whatever the stale handle points at.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn active_handle_model() -> Model {
    Model {
        name: "ActiveHandle",
        // Bound the fresh-session id space so `ty` explores a finite, terminating space.
        // `Buggy` flips the global re-sync OFF on the close/new-window lane (the swallow).
        consts: vec![("MaxId", 4), ("Buggy", 0)],
        vars: vec![
            // TRUTH lane: the frontmost window's CURRENT active-tab focused-pane session.
            StateVar {
                name: "truth",
                init: 1,
            },
            // MIRROR lane: the global control `ActiveHandle`'s target session.
            StateVar {
                name: "handle",
                init: 1,
            },
            // A strictly-increasing fresh-session allocator, so each change moves the
            // front active session to a DISTINCT id (a stale handle is then observable).
            StateVar {
                name: "next",
                init: 2,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            // The ALWAYS-CORRECT lockstep path (open_tab_in / switch_tab / move_tab):
            // the front active session moves to a fresh session and the global handle is
            // re-pointed in lockstep (`if frontmost { sync_active_session }`). Both lanes
            // move together — this path was never buggy.
            Action {
                name: "SwitchActive",
                guard: Some(le(var("next"), sub(cst("MaxId"), int(1)))),
                updates: vec![
                    Update {
                        var: "truth",
                        expr: var("next"),
                    },
                    Update {
                        var: "handle",
                        expr: var("next"),
                    },
                    Update {
                        var: "next",
                        expr: add(var("next"), int(1)),
                    },
                ],
            },
            // The SWALLOW-PRONE path (apply_close_outcome's pane-collapse / tab-close and
            // create_window_internal's new front window): the front active session moves
            // to a fresh session. The FIX re-points the global handle too
            // (`resync_active_or_window` -> `sync_active_session`); the Buggy branch
            // re-mirrors only the per-window state (`sync_window`) and LEAVES THE GLOBAL
            // HANDLE STALE on the just-closed / previous session.
            Action {
                name: "CloseOrNewFront",
                guard: Some(le(var("next"), sub(cst("MaxId"), int(1)))),
                updates: vec![
                    Update {
                        var: "truth",
                        expr: var("next"),
                    },
                    Update {
                        var: "handle",
                        expr: if_(eq(cst("Buggy"), int(1)), var("handle"), var("next")),
                    },
                    Update {
                        var: "next",
                        expr: add(var("next"), int(1)),
                    },
                ],
            },
        ],
        invariants: vec![
            // The global control handle always names the session the user is actually
            // looking at in the frontmost window — so a control verb never drives a
            // stale or just-closed session.
            Invariant {
                name: "HandleMirrorsFront",
                expr: eq(var("handle"), var("truth")),
            },
        ],
    }
}

/// The cross-process `@<child>` proxy FORWARD is acyclic and TERMINATES (control.rs
/// `proxy_forward_plan` / `try_proxy_forward`). The recursion-topology refactor REMOVED
/// the explicit hop-counter cap, leaving ONE structural invariant as the sole guard
/// against a forward loop: the parent rewrites the child's own selector to `@.` (run on
/// self) before relaying, so the child resolves the verb LOCALLY and never re-enters
/// `try_proxy_forward`. A forward chain is therefore at most ONE cross-process hop — a
/// child is never in its own proxy table, and the `@.` rewrite means it can't forward
/// onward — so no A→B→A ping-pong (or unbounded relay-thread/fd growth) can form.
///
/// `ty` PROVES `OneHopNoCycle` at Buggy=0 — the rewrite-to-`@.` discipline caps the
/// chain at depth 1 under any interleaving over the bounded space — and CATCHES the
/// loop class at Buggy=1 (a forward that relays the ORIGINAL cross-selector instead of
/// `@.`, so the child re-forwards and the chain grows past one hop) -> counterexample on
/// `OneHopNoCycle`. This locks in the safety the removed hop-cap used to provide: if the
/// `@.` rewrite ever regresses, the exhaustive check fails.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn proxy_forward_model() -> Model {
    Model {
        name: "ProxyForward",
        // MaxDepth bounds `ty`'s exploration; the SAFETY bound the invariant asserts is 1.
        // `Buggy` flips the `@.` rewrite off (relay the original cross-selector → re-forward).
        consts: vec![("MaxDepth", 2), ("Buggy", 0)],
        vars: vec![
            // Cross-process hops taken by the in-flight forward chain so far.
            StateVar {
                name: "depth",
                init: 0,
            },
            // Is a request still in flight and eligible to forward onward?
            StateVar {
                name: "active",
                init: 1,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            // One forward hop: the parent dials the child and relays. The FIX rewrites
            // the child's selector to `@.`, so the child runs the verb on ITSELF and the
            // chain ENDS (active' = 0). The Buggy branch relays the original cross
            // selector, so the child re-forwards and the chain CONTINUES (active' = 1).
            Action {
                name: "Forward",
                guard: Some(and_(
                    eq(var("active"), int(1)),
                    le(var("depth"), sub(cst("MaxDepth"), int(1))),
                )),
                updates: vec![
                    Update {
                        var: "depth",
                        expr: add(var("depth"), int(1)),
                    },
                    Update {
                        var: "active",
                        expr: if_(eq(cst("Buggy"), int(1)), int(1), int(0)),
                    },
                ],
            },
        ],
        invariants: vec![
            // A forward chain is at most one cross-process hop — never a cycle or
            // unbounded recursion (which would exhaust relay threads / fds).
            Invariant {
                name: "OneHopNoCycle",
                expr: le(var("depth"), int(1)),
            },
        ],
    }
}
