// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The aterm-models-core models aterm-spec's own unit tests run the engine on (`ring_model`, `cursor_model`, `subscribe_model`),
//! re-exported from this crate's root like every other. aterm-spec `#[path]`-mounts this file
//! (`src/test_models.rs`), so it holds only these and names only `aterm_spec::…` paths: an edit
//! to the rest of this crate's models then rebuilds no aterm-spec test and does not seed
//! aterm-spec, and with it every crate that depends on aterm-spec, into `verify`'s change cone.

use aterm_spec::derive::*;

/// The bounded event-log ring as a derived model — the single source the spec in
/// `Evict.tla` hand-encodes, scalar-projected to `<<seq, lo>>`. `Push` advances
/// `seq` and evicts the oldest live event (`lo`) exactly when the live window
/// would exceed `Cap`. `MaxSeq` bounds the state space so `ty check` is
/// exhaustive + terminating; `Cap` is the ring capacity. Action name is `Push`
/// (not `Append`, which clashes with ty's Sequences builtin).
///
/// `Buggy = 1` evicts one push late, so the ring holds `Cap + 1` live events —
/// the shape of the alt-screen archive undercharge (`a4c4d3eaa`: a row charged
/// as `len + 32` let a 4 MiB budget hold 5.23 MiB, and admitted a fourth row into
/// a three-row budget, the negative control `alt_archive_tests.rs` binds against
/// this model). `LenBounded` is its counterexample.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn ring_model() -> Model {
    Model {
        name: "Ring",
        consts: vec![("MaxSeq", 6), ("Cap", 3), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            },
            StateVar {
                name: "lo",
                init: 1,
            },
        ],
        fn_vars: vec![],
        actions: vec![Action {
            name: "Push",
            guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))), // seq <= MaxSeq - 1
            updates: vec![
                Update {
                    var: "seq",
                    expr: add(var("seq"), int(1)),
                },
                Update {
                    var: "lo",
                    // IF (seq + 1) - lo + 1 > Cap + Buggy THEN lo + 1 ELSE lo
                    expr: if_(
                        gt(
                            add(sub(add(var("seq"), int(1)), var("lo")), int(1)),
                            add(cst("Cap"), cst("Buggy")),
                        ),
                        add(var("lo"), int(1)),
                        var("lo"),
                    ),
                },
            ],
        }],
        invariants: vec![Invariant {
            name: "LenBounded",
            kind: InvariantKind::Claim,
            // seq - lo + 1 <= Cap
            expr: le(add(sub(var("seq"), var("lo")), int(1)), cst("Cap")),
        }],
    }
}

/// A second derived model — a writer/subscriber cursor — chosen because it
/// exercises derivation paths the ring does not: TWO actions (so `Next` is a
/// disjunction) and PARTIAL updates (so each action emits an `UNCHANGED` clause
/// for the variable it leaves alone). `Grow` advances the writer `seq`; `Deliver`
/// catches the reader `cursor` up to `seq`. Invariant: the reader never passes the
/// writer (`cursor <= seq`). This is the Subscribe/Kernel family in miniature, and
/// it proves the derivation engine generalizes beyond the single-action ring.
///
/// `Buggy = 1` is the off-by-one class a subscriber cursor can ship with (the
/// non-vacuity witness for `CursorBounded`, not a defect the code is known to have
/// had): a `Deliver` that parks the cursor one PAST the head it delivered, so the
/// next event written is silently skipped. `CursorBounded` catches it on the first
/// delivery. Tier-1: aterm-buffer's `tests/conformance_cursor.rs` drives the real
/// `Surface::subscribe`/`poll` — the library's read face, which no product crate
/// calls today — and reads the cursor's position back through the real `poll`
/// itself, which is what catches an overshoot `subscribe_model`'s bind (a
/// caller-tracked cursor) cannot see.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn cursor_model() -> Model {
    Model {
        name: "Cursor",
        consts: vec![("MaxSeq", 4), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            },
            StateVar {
                name: "cursor",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Grow", // writer appends; cursor is UNCHANGED
                guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
                updates: vec![Update {
                    var: "seq",
                    expr: add(var("seq"), int(1)),
                }],
            },
            Action {
                name: "Deliver", // reader catches up; seq is UNCHANGED
                guard: Some(gt(var("seq"), var("cursor"))),
                // cursor' = IF Buggy = 1 THEN seq + 1 ELSE seq  (Buggy overshoots)
                updates: vec![Update {
                    var: "cursor",
                    expr: if_(
                        eq(cst("Buggy"), int(1)),
                        add(var("seq"), int(1)),
                        var("seq"),
                    ),
                }],
            },
        ],
        invariants: vec![Invariant {
            name: "CursorBounded",
            kind: InvariantKind::Claim,
            expr: le(var("cursor"), var("seq")), // cursor <= seq
        }],
    }
}

/// A third derived model — the subscriber's NO-SILENT-LOSS / gap discipline, the
/// kernel family's most important correctness property: a reader that has fallen
/// behind the live ring window MUST receive a Gap (resync) and must NEVER be
/// silently delivered events as if nothing was lost. Scalar projection over
/// `<<seq, lo, cursor, lost>>`: `Grow` advances the writer and evicts the oldest
/// when over `Cap`; `PollGap` resyncs a fallen-behind reader (`lo > cursor + 1`);
/// `PollDeliver` delivers while the reader is still within the live window.
///
/// The `Buggy` constant flips `PollDeliver`'s guard: with `Buggy = 0` (committed)
/// it is correctly guarded and `lost` stays 0; with `Buggy = 1` it fires even when
/// the reader is behind, silently skipping evicted events — so `lost` becomes 1
/// and `NoSilentLoss` is violated. Thus `ty` both PROVES the property (Buggy=0)
/// and, via a `Buggy=1` cfg, shows it genuinely CATCHES the silent-loss bug.
/// Exercises the `Expr` disjunction (`\/`) and equality (`=`) operators.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn subscribe_model() -> Model {
    Model {
        name: "Subscribe",
        consts: vec![("MaxSeq", 4), ("Cap", 2), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            },
            StateVar {
                name: "lo",
                init: 1,
            },
            StateVar {
                name: "cursor",
                init: 0,
            },
            StateVar {
                name: "lost",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Grow", // writer appends + evicts oldest when over Cap
                guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
                updates: vec![
                    Update {
                        var: "seq",
                        expr: add(var("seq"), int(1)),
                    },
                    Update {
                        var: "lo",
                        expr: if_(
                            gt(
                                add(sub(add(var("seq"), int(1)), var("lo")), int(1)),
                                cst("Cap"),
                            ),
                            add(var("lo"), int(1)),
                            var("lo"),
                        ),
                    },
                ], // cursor, lost UNCHANGED
            },
            Action {
                name: "PollGap", // reader fell behind (lo > cursor + 1): resync, no loss
                guard: Some(gt(var("lo"), add(var("cursor"), int(1)))),
                updates: vec![Update {
                    var: "cursor",
                    expr: var("seq"),
                }], // seq, lo, lost UNCHANGED
            },
            Action {
                name: "PollDeliver", // deliver; correct iff the reader is still in window
                // Buggy = 1 \/ lo =< cursor + 1  (Buggy removes the in-window guard)
                guard: Some(or_(
                    eq(cst("Buggy"), int(1)),
                    le(var("lo"), add(var("cursor"), int(1))),
                )),
                updates: vec![
                    Update {
                        var: "cursor",
                        expr: var("seq"),
                    },
                    // lost' = IF lo > cursor + 1 THEN 1 ELSE lost  (records a silent skip)
                    Update {
                        var: "lost",
                        expr: if_(
                            gt(var("lo"), add(var("cursor"), int(1))),
                            int(1),
                            var("lost"),
                        ),
                    },
                ], // seq, lo UNCHANGED
            },
        ],
        invariants: vec![Invariant {
            name: "NoSilentLoss",
            kind: InvariantKind::Claim,
            expr: eq(var("lost"), int(0)), // lost = 0
        }],
    }
}
