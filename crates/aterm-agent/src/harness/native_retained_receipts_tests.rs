// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        RetainedIssuerJournal {
            const Buggy = 0;
            var stage = 0;
            var possible = 0;
            var noexec = 0;
            action Prepare when(stage == 0){stage=1;}
            action MayExecute when(stage == 1){stage=2;possible=1;}
            action Seal when(stage == 2){stage=3;}
            action NoExec when(Buggy == 1 && stage == 2){stage=3;noexec=1;}
            invariant Bounds: stage <= 3 && possible <= 1 && noexec <= 1;
            invariant NoRetainedNoExec: noexec == 0;
        }
    }
}

#[test]
fn retained_journal_model_and_native_replay_conform_without_a_noexec_transition() {
    let model = model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let mut before = model.init_state();
    for (stage, (action, event)) in [
        ("Prepare", Event::Prepare),
        ("MayExecute", Event::MayExecute),
        ("Seal", Event::Seal),
    ]
    .into_iter()
    .enumerate()
    {
        let (_home, paths, _, _) = super::super::tests::fixture();
        let key = write_key(&paths).unwrap();
        let actual = described(
            &key,
            &"c9".repeat(16),
            b"actual native replay recipe digest",
        );
        for predecessor in [Event::Prepare, Event::MayExecute, Event::Seal]
            .into_iter()
            .take(stage)
        {
            append_at(&paths, &actual, predecessor, LIMITS).unwrap();
        }
        assert!(append_at(&paths, &actual, Event::NoExec, LIMITS).is_err());
        assert!(model.action_enabled(action, &before));
        append_at(&paths, &actual, event, LIMITS).unwrap();
        let (_, view) = open_existing(&paths, &key, LIMITS).unwrap();
        let entry = &view.entries[&actual.receipt.operation];
        let (next, possible) = match entry.state {
            IssuerState::Prepared => (1, 0),
            IssuerState::MayExecute => (2, 1),
            IssuerState::Sealed { may_execute: true } => (3, 1),
            other => panic!("unexpected retained journal state {other:?}"),
        };
        let after = [("stage", next), ("possible", possible), ("noexec", 0)]
            .into_iter()
            .collect();
        assert!(model.successors(action, &before).contains(&after));
        assert!(model.check_invariant("NoRetainedNoExec", &after));
        before = after;
    }
}

fn described(key: &Key, id: &str, recipe: &[u8]) -> RetainedExecution {
    RetainedExecution {
        receipt: Receipt {
            operation: key.operation(id),
            prepare: key.digest(b"retained-exec-recipe/1\0", recipe),
            capability: key.digest(
                b"retained-exec-capability/1\0",
                b"original native capability",
            ),
        },
        epoch: key.digest(b"retained-exec-epoch/1\0", b"original service/start owner"),
    }
}

#[test]
fn retained_execution_reserves_shared_closing_space_and_never_admits_noexec() {
    let (_home, paths, restart, binding) = super::super::tests::fixture();
    let key = write_key(&paths).unwrap();
    let actual = described(&key, &"a7".repeat(16), b"same original raw command hash");
    // Before Prepare, neither possible execution nor closure is admitted.
    for event in [Event::MayExecute, Event::Seal, Event::NoExec, Event::Reject] {
        assert!(append_at(&paths, &actual, event, LIMITS).is_err());
    }
    let first = append_at(&paths, &actual, Event::Prepare, LIMITS).unwrap();
    let (_, view) = open_existing(&paths, &key, LIMITS).unwrap();
    assert_eq!(view.reserved_records, 2);
    assert_eq!(
        append_at(&paths, &actual, Event::Prepare, LIMITS).unwrap(),
        first
    );
    assert!(append_at(&paths, &actual, Event::Seal, LIMITS).is_err());
    let changed = described(&key, &"a7".repeat(16), b"other valid command hash");
    assert!(append_at(&paths, &changed, Event::Prepare, LIMITS).is_err());
    assert!(append_at(&paths, &changed, Event::MayExecute, LIMITS).is_err());
    let may = append_at(&paths, &actual, Event::MayExecute, LIMITS).unwrap();
    assert_eq!(
        append_at(&paths, &actual, Event::MayExecute, LIMITS).unwrap(),
        may
    );
    assert!(append_at(&paths, &actual, Event::NoExec, LIMITS).is_err());
    let (_, view) = open_existing(&paths, &key, LIMITS).unwrap();
    assert_eq!(view.reserved_records, 1);
    // Ordinary Restart admission shares the SAME journal and capacity, while
    // its preparation domain and existing matching semantics remain unchanged.
    record_event_at(&paths, &restart, &binding, Some(Event::Prepare)).unwrap();
    let (_, view) = open_existing(&paths, &key, LIMITS).unwrap();
    assert_eq!(view.reserved_records, 3);
    let closed = append_at(&paths, &actual, Event::Seal, LIMITS).unwrap();
    assert_eq!(
        append_at(&paths, &actual, Event::Seal, LIMITS).unwrap(),
        closed
    );
    assert!(append_at(&paths, &actual, Event::MayExecute, LIMITS).is_err());
    let (_, view) = open_existing(&paths, &key, LIMITS).unwrap();
    assert_eq!(view.reserved_records, 2);
    assert_eq!(
        view.entries[&actual.receipt.operation].state,
        IssuerState::Sealed { may_execute: true }
    );
}

#[test]
fn retained_execution_cannot_consume_another_operations_reserved_capacity() {
    let (_home, paths, _, _) = super::super::tests::fixture();
    let key = write_key(&paths).unwrap();
    let first = described(&key, &"a7".repeat(16), b"first");
    let second = described(&key, &"b8".repeat(16), b"second");
    let limits = Limits {
        records: 3,
        ..LIMITS
    };
    append_at(&paths, &first, Event::Prepare, limits).unwrap();
    assert!(append_at(&paths, &second, Event::Prepare, limits).is_err());
    append_at(&paths, &first, Event::MayExecute, limits).unwrap();
    append_at(&paths, &first, Event::Seal, limits).unwrap();
    assert!(
        append_at(&paths, &second, Event::Prepare, limits).is_err(),
        "committed history and reserved future closes both count"
    );
}
