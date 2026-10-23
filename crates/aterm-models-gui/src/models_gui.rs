// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Native window, tab and pane models (re-exported from this crate's root).

use aterm_spec::derive::*;

/// Session-pool refcount: a pooled session's entry exists exactly while at least one window view references it.
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

/// Tab-strip parity: the native macOS titlebar tab strip (`toolbar.rs`'s `NSSegmentedControl`) never desyncs from the tab model.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn tab_strip_model() -> Model {
    let reclamp = || {
        if_(
            gt(var("active"), sub(var("count"), int(2))),
            sub(var("count"), int(2)),
            var("active"),
        )
    };
    Model {
        name: "TabStrip",
        consts: vec![("Cap", 4), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "count",
                init: 1,
            },
            StateVar {
                name: "active",
                init: 0,
            },
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
        invariants: vec![Invariant {
            name: "StripMirrorsTruth",
            kind: InvariantKind::Claim,
            expr: and_(
                and_(
                    eq(var("seg_count"), var("count")),
                    eq(var("selected"), var("active")),
                ),
                le(var("selected"), sub(var("seg_count"), int(1))),
            ),
        }],
    }
}

/// The global control-socket `ActiveHandle` mirror (`active_handle` in aterm-gui's `App`).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn active_handle_model() -> Model {
    Model {
        name: "ActiveHandle",
        consts: vec![("MaxId", 4), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "truth",
                init: 1,
            },
            StateVar {
                name: "handle",
                init: 1,
            },
            StateVar {
                name: "next",
                init: 2,
            },
        ],
        fn_vars: vec![],
        actions: vec![
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
        invariants: vec![Invariant {
            name: "HandleMirrorsFront",
            kind: InvariantKind::Claim,
            expr: eq(var("handle"), var("truth")),
        }],
    }
}

/// The cross-process `@<sid>` proxy forward is acyclic and terminates (`control.rs` `proxy_forward_plan` / `try_proxy_forward`).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn proxy_forward_model() -> Model {
    Model {
        name: "ProxyForward",
        consts: vec![("MaxDepth", 2), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "depth",
                init: 0,
            },
            StateVar {
                name: "active",
                init: 1,
            },
        ],
        fn_vars: vec![],
        actions: vec![Action {
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
        }],
        invariants: vec![Invariant {
            name: "OneHopNoCycle",
            kind: InvariantKind::Claim,
            expr: le(var("depth"), int(1)),
        }],
    }
}

/// OSC 52's blocking platform-writer handoff is a one-slot, latest-value mailbox (`aterm-gui/src/spawn.rs`).
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

/// The native-document disk worker has four bounded waiting slots plus one executing slot (`aterm-gui/src/app_documents.rs`).
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
