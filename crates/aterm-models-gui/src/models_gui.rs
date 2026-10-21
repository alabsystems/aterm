// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Native window, tab and pane models (re-exported from this crate's root).

use aterm_spec::derive::*;

/// Session-pool refcount: a pooled session's entry exists exactly while at least one window
/// view references it. `refcount` is the live view count (`SessionPool::views`), `closed`
/// whether the entry is retired; `ClosedIffEmpty` says a session is retired when, and only
/// when, its last viewer detaches, so two windows on one session (Cmd-Shift-O) never retire
/// early and a detached session never leaks an entry. `Buggy = 1` retires on every Release.
///
/// Tier-1 projects the two variables from independent signals: `refcount` from the count of
/// canonical terminal view edges across every live tab and split, `closed` from pool
/// membership (`views(sid).is_none()`). A pool that retires a session (dropping its `Session`,
/// closing the PTY) while a view still references it projects to `[refcount>0, closed=1]`.
/// PTY-fd liveness past the pool entry (the `SinkWriter` Arc) is out of scope.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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
            // attach (another window views the session, Cmd-Shift-O): refcount+1.
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
            // detach: refcount-1; retire iff that was the last viewer. `Buggy` retires on
            // every detach.
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
            kind: InvariantKind::Claim,
            expr: and_(
                or_(eq(var("closed"), int(0)), eq(var("refcount"), int(0))),
                or_(eq(var("closed"), int(1)), gt(var("refcount"), int(0))),
            ),
        }],
    }
}

/// Tab-strip parity: the native macOS titlebar tab strip (`toolbar.rs`'s `NSSegmentedControl`)
/// never desyncs from the tab model. Its `segmentCount` equals the tab `count`, its
/// `selectedSegment` the `active` tab, and since AppKit indexes it the selection stays in
/// range. `set_window_tabs(handle, titles, active)` maintains it, driven from
/// `App::refresh_window_tabs` after every tab mutation.
///
/// Two lanes on one event stream (as in `recording_model`): a truth lane `(count, active)`,
/// the `TabIndex` machine of [`tab_nav_model`], and a strip lane `(seg_count, selected)`
/// re-synced after each action. `Buggy=1` drops the re-sync in Close (a missed
/// `refresh_window_tabs`), so the strip shows an extra segment with an out-of-range selection.
///
/// Tier-1: aterm-gui's `tab_strip_conformance` projects the truth lane from `ws.tab_set` and the
/// strip lane from `WindowState::strip_shadow`, the record of what `refresh_window_tabs` last
/// pushed. The two signals are independent, so closing a tab in a non-front window is a
/// rejected desync unless `close_tab_at` re-syncs that window's strip.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn tab_strip_model() -> Model {
    // After a Close: min(idx, new_count - 1), with RHS reading the pre-state count.
    let reclamp = || {
        if_(
            gt(var("active"), sub(var("count"), int(2))),
            sub(var("count"), int(2)),
            var("active"),
        )
    };
    Model {
        name: "TabStrip",
        // `Cap` bounds the tab count; `Buggy` drops the Close strip re-sync.
        consts: vec![("Cap", 4), ("Buggy", 0)],
        vars: vec![
            // Truth lane (the TabIndex machine).
            StateVar {
                name: "count",
                init: 1,
            },
            StateVar {
                name: "active",
                init: 0,
            },
            // Strip lane (the NSSegmentedControl mirror).
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
            // open_tab_in -> TabIndex::add(): append and switch; refresh re-syncs the strip.
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
            // switch_tab_in / cycle_tab: both lanes move in lockstep, modelled as the
            // deterministic cycle(true) wrap.
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
            // close_tab_at -> TabIndex::close(i), count > 1: count' = count - 1, active
            // re-clamped, then the strip re-synced. `Buggy` drops the re-sync, freezing both
            // strip vars at their pre-close values.
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
            // The strip mirrors the tab count and active tab, and its selection is a segment
            // index AppKit can highlight.
            Invariant {
                name: "StripMirrorsTruth",
                kind: InvariantKind::Claim,
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

/// The global control-socket `ActiveHandle` mirror (`active_handle` in aterm-gui's `App`).
/// Introspection and drive verbs (`text`/`feed`/`signal`) resolve through it
/// (`resolve_active`), so it must always name the session the user is looking at: the frontmost
/// window's active tab's focused pane. The process-wide analog of [`tab_strip_model`]'s
/// per-window strip mirror, with the same truth/mirror lanes.
///
/// At `Buggy=0` every path that moves the front session also re-points the handle
/// (`resync_active_or_window` -> `sync_active_session`). `Buggy=1` is a close-collapse or
/// new-window path that re-mirrors only the per-window state (`sync_window`), refuted on
/// `HandleMirrorsFront`. A stale handle matters because Owner-scope verbs bypass the
/// per-request edge gate and hit whatever it names. Every seam that creates the new front
/// re-points in its install half, after the PTY spawn (`install_new_tab`, `install_split_pane`,
/// `install_window_state_with_parent`), not in each caller.
///
/// Tier-1: aterm-gui's `active_handle_conformance` drives a headless `App` through every seam
/// (the install halves with a stub session, since the spawn needs an event loop), reads `truth`
/// from the canonical tab model and `handle` from the real `active_handle`, and validates each
/// front move against this model; the stale-handle step is the one it rejects.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn active_handle_model() -> Model {
    Model {
        name: "ActiveHandle",
        // `MaxId` bounds the fresh-session ids; `Buggy` drops the global re-sync on the
        // close/new-window path.
        consts: vec![("MaxId", 4), ("Buggy", 0)],
        vars: vec![
            // Truth lane: the frontmost window's active tab's focused-pane session.
            StateVar {
                name: "truth",
                init: 1,
            },
            // Mirror lane: the global control `ActiveHandle`'s target session.
            StateVar {
                name: "handle",
                init: 1,
            },
            // A strictly increasing allocator, so each change moves the front session to a
            // distinct id and a stale handle is observable.
            StateVar {
                name: "next",
                init: 2,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            // The lockstep path (open_tab_in / switch_tab / move_tab): the front session and the
            // handle move together (`if frontmost { sync_active_session }`).
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
            // The close and new-window path (apply_close_outcome's pane collapse or tab close,
            // install_window_state_with_parent): `resync_active_or_window` re-points the handle too;
            // `Buggy` re-mirrors only the per-window state (`sync_window`) and leaves it stale.
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
            // A control verb never drives a stale or just-closed session.
            Invariant {
                name: "HandleMirrorsFront",
                kind: InvariantKind::Claim,
                expr: eq(var("handle"), var("truth")),
            },
        ],
    }
}

/// The cross-process `@<sid>` proxy forward is acyclic and terminates (`control.rs`
/// `proxy_forward_plan` / `try_proxy_forward`). There is no hop counter; a hop ends at its
/// target: the forward keeps `@<sid>` and presents the hosting instance's own token, and the
/// discovery entry it followed was published by the instance at that socket, so the sid is
/// hosted there, or the entry is stale and names the host's own socket, which the self-dial
/// guard refuses. A nested child is such a host too.
///
/// So a chain is at most one cross-process hop and no A→B→A ping-pong can form. `Buggy=1` is a
/// target that forwards on, refuted on `OneHopNoCycle`.
///
/// Tier-1 is aterm-gui's `control_tests.rs::sibling_forward_plan_conforms_to_proxy_forward_one_hop`,
/// on every platform: it takes the real first line the forward presents (`proxy_forward_plan`'s
/// socket and the line, with the token beside that socket), strips its handshake, asks the real
/// router in the host's context (its own socket as self, its own store) with Owner scope whether
/// it would forward on, for a sid the host serves and for a stale entry, and validates the hop as
/// `Forward`, which `Buggy = 1` must reject.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn proxy_forward_model() -> Model {
    Model {
        name: "ProxyForward",
        // `MaxDepth` bounds the exploration; the invariant asserts depth 1. `Buggy` turns the
        // `@.` rewrite off.
        consts: vec![("MaxDepth", 2), ("Buggy", 0)],
        vars: vec![
            // Cross-process hops taken by the in-flight forward chain so far.
            StateVar {
                name: "depth",
                init: 0,
            },
            // A request still in flight and eligible to forward onward.
            StateVar {
                name: "active",
                init: 1,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            // One hop, which ends at its target (active' = 0). `Buggy` is a target that
            // forwards on (the original selector relayed to a child), so the chain continues.
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
            // At most one cross-process hop: no cycle exhausting relay threads and fds.
            Invariant {
                name: "OneHopNoCycle",
                kind: InvariantKind::Claim,
                expr: le(var("depth"), int(1)),
            },
        ],
    }
}

/// OSC 52's blocking platform-writer handoff is a one-slot, latest-value mailbox
/// (`aterm-gui/src/spawn.rs`). A producer may replace either destination while the worker is
/// busy but never appends another retained payload: the slot holds at most one and, when
/// occupied, names the newest clipboard and PRIMARY generations independently.
///
/// `Buggy=1` is an unbounded channel: every publish grows the retained count and leaves the
/// first payload in place.
#[cfg_attr(trust_verify, trust::skip)]
pub fn clipboard_mailbox_model() -> Model {
    aterm_spec::ty_model! {
        ClipboardMailbox {
            const MaxGeneration = 3;
            const Buggy = 0;
            var pending = 0;
            var clipboard_generation = 0;
            var pending_clipboard = 0;
            var pending_clipboard_present = 0;
            var primary_generation = 0;
            var pending_primary = 0;
            var pending_primary_present = 0;
            action PublishClipboard when (clipboard_generation <= MaxGeneration - 1) {
                pending = if Buggy == 1 { pending + 1 } else { 1 };
                clipboard_generation = clipboard_generation + 1;
                pending_clipboard = if Buggy == 1 {
                    if pending_clipboard_present == 0 {
                        clipboard_generation + 1
                    } else {
                        pending_clipboard
                    }
                } else {
                    clipboard_generation + 1
                };
                pending_clipboard_present = 1;
            }
            action PublishPrimary when (primary_generation <= MaxGeneration - 1) {
                pending = if Buggy == 1 { pending + 1 } else { 1 };
                primary_generation = primary_generation + 1;
                pending_primary = if Buggy == 1 {
                    if pending_primary_present == 0 {
                        primary_generation + 1
                    } else {
                        pending_primary
                    }
                } else {
                    primary_generation + 1
                };
                pending_primary_present = 1;
            }
            action Consume when (pending > 0) {
                pending = 0;
                pending_clipboard_present = 0;
                pending_primary_present = 0;
            }
            invariant OneLatestPendingWrite:
                pending <= 1
                && (
                    (
                        pending == 0
                        && pending_clipboard_present == 0
                        && pending_primary_present == 0
                    )
                    || (
                        pending > 0
                        && (
                            pending_clipboard_present > 0
                            || pending_primary_present > 0
                        )
                    )
                )
                && (
                    pending_clipboard_present == 0
                    || pending_clipboard == clipboard_generation
                )
                && (
                    pending_primary_present == 0
                    || pending_primary == primary_generation
                );
        }
    }
}

/// The native-document disk worker has four bounded waiting slots plus one
/// executing slot (`aterm-gui/src/app_documents.rs`). A full journal submission
/// coalesces to an ID-only reducer intent; `WorkerStarts` releases one waiting
/// slot and its UI wake can admit that intent through `RetryAccepted`. The
/// scalar retry variables project membership for one observed document; they do
/// not count the runtime's whole ID set.
///
/// `Buggy=1` accepts a fifth waiting image, drops the reducer intent on `Full`, and releases
/// capacity without a retry wake; the invariants catch each independently.
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_document_queue_model() -> Model {
    aterm_spec::ty_model! {
        NativeDocumentQueue {
            const QueueCapacity = 4;
            const RetainedCapacity = 5;
            const Buggy = 0;
            var queued = 0;
            var executing = 0;
            var deferred_intent = 0;
            var retry_pending = 0;
            var drain_edge = 0;
            action SubmitAccepted when (
                queued <= if Buggy == 1 {
                    QueueCapacity
                } else {
                    QueueCapacity - 1
                }
            ) {
                queued = queued + 1;
            }
            action SubmitDeferred when (queued > QueueCapacity - 1) {
                deferred_intent = 1;
                retry_pending = if Buggy == 1 { 0 } else { 1 };
            }
            action WorkerStarts when (queued > 0 && executing == 0) {
                queued = queued - 1;
                executing = 1;
                drain_edge = if Buggy == 1 || Buggy == 2 { 0 } else { 1 };
            }
            action WorkerCompletes when (executing == 1) {
                executing = 0;
            }
            action RetryAccepted when (
                deferred_intent == 1
                && retry_pending == 1
                && queued <= QueueCapacity - 1
                && drain_edge == 1
            ) {
                queued = queued + 1;
                deferred_intent = 0;
                retry_pending = 0;
                drain_edge = 0;
            }
            action RetryStillFull when (
                deferred_intent == 1
                && retry_pending == 1
                && queued > QueueCapacity - 1
                && drain_edge == 1
            ) {
                drain_edge = 0;
            }
            action ConsumeDrainEdge when (
                retry_pending == 0 && drain_edge == 1
            ) {
                drain_edge = 0;
            }
            invariant BoundedFullImageJobs:
                queued <= QueueCapacity
                && executing <= 1
                && queued + executing <= RetainedCapacity;
            invariant DeferredIntentRetained:
                deferred_intent <= 1
                && retry_pending <= 1
                && deferred_intent <= retry_pending;
            // A freed slot wakes a pending retry (dial 2 frees one with no wake, while the
            // retry is still owed).
            invariant OpenSlotHasRetryEdge:
                drain_edge <= 1
                && (
                    retry_pending == 0
                    || queued > QueueCapacity - 1
                    || drain_edge == 1
                );
        }
    }
}
