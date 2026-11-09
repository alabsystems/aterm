// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Included in entries::tests to reuse its real signed three-member fixture.

pub(crate) fn host_carry_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "Ordered" {
        next.insert("ordered", 1);
        return next;
    }
    assert_eq!(action, "Read");
    let t = three();
    let mut base = t.log.clone();
    let pair = Audience::Hosts(vec!["m1".into(), "m2".into()]);
    let trio = Audience::Hosts(vec!["m1".into(), "m2".into(), "m9".into()]);
    let epoch = |log: &mut Log, audience: &Audience, generation| {
        let (draft, key) = log
            .draft_epoch(&t.m1, 2_000, audience.clone(), generation)
            .unwrap();
        let record = draft.sign(&t.w1, t.inc1).unwrap();
        let id = *record.id();
        log.insert(record).unwrap();
        for identity in [&t.id1, &t.id2, &t.id9] {
            if identity.recipient() == t.id9.recipient() && audience == &pair {
                continue;
            }
            let record = log
                .draft_wrap(&t.m1, 2_001, id, &key, identity.recipient())
                .unwrap()
                .sign(&t.w1, t.inc1)
                .unwrap();
            log.insert(record).unwrap();
        }
        (id, key)
    };
    let (pair_epoch, pair_key) = epoch(&mut base, &pair, 1);
    let (trio_epoch, trio_key) = epoch(&mut base, &trio, 1);
    let entry = |audience: &Audience, value: &[u8]| Entry {
        name: "host-choice".into(),
        value: SecretBytes::from(value),
        audience: audience.clone(),
        if_absent: false,
        anchor: None,
    };
    let first = base
        .draft_put(
            &t.m1,
            2_100,
            pair_epoch,
            &pair_key,
            &entry(&pair, b"original"),
        )
        .unwrap()
        .sign(&t.w1, t.inc1)
        .unwrap();
    base.insert(first).unwrap();
    let mut left = base.clone();
    let first = left
        .draft_put(
            &t.m1,
            2_200,
            pair_epoch,
            &pair_key,
            &entry(&pair, b"pair-value"),
        )
        .unwrap()
        .sign(&t.w1, t.inc1)
        .unwrap();
    left.insert(first).unwrap();
    let mut right = if state["ordered"] == 1 {
        left.clone()
    } else {
        base
    };
    let second = right
        .draft_put(
            &t.m2,
            2_201,
            trio_epoch,
            &trio_key,
            &entry(&trio, b"trio-value"),
        )
        .unwrap()
        .sign(&t.w2, t.inc2)
        .unwrap();
    right.insert(second).unwrap();
    left.merge(&right).unwrap();
    let selected = |log: &Log| {
        let opened = log
            .open_selection(&t.id1, Some(&["host-choice"]))
            .unwrap()
            .unwrap();
        let selected = &opened.opened.entries[0];
        let key = (selected.audience.clone(), selected.name.clone());
        assert!(opened.puts.contains_key(&key));
        assert!(opened.complete.contains(&selected.audience));
        (
            selected.value.as_slice().to_vec(),
            opened.operations[&key],
            opened.opened.conflicts,
        )
    };
    let before = selected(&left);
    if state["ordered"] == 1 {
        assert_eq!(before.0, b"trio-value");
        assert!(before.2.is_empty());
    } else {
        assert_eq!(before.2.len(), 2);
    }
    // The pair is re-encrypted after both semantic writes are known. Its
    // new wrapper has seen the trio, while its semantic write has not changed.
    let (fresh, key) = epoch(&mut left, &pair, 2);
    let carry = left
        .draft_put(&t.m1, 2_300, fresh, &key, &entry(&pair, b"pair-value"))
        .unwrap()
        .sign(&t.w1, t.inc1)
        .unwrap();
    left.insert(carry).unwrap();
    let after = selected(&left);
    next.insert("done", 1);
    next.insert("value", i64::from(before.0 == after.0));
    next.insert("operation", i64::from(before.1 == after.1));
    next.insert("conflicts", i64::from(before.2 == after.2));
    next
}

#[test]
fn reencrypting_a_host_register_preserves_semantic_precedence() {
    let model = aterm_models_db::host_carry_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = host_carry_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{state:?} -> {actual:?}"
                );
            }
        }
    }
    for ordered in [0, 1] {
        let mut state = model.init_state();
        state.insert("ordered", ordered);
        HOST_WRAPPER_PRECEDENCE.set(true);
        let bad = host_carry_transition(&state, "Read");
        HOST_WRAPPER_PRECEDENCE.set(false);
        assert!(!model.successors("Read", &state).contains(&bad));
        let invariant = if ordered == 0 {
            "ConflictPreserved"
        } else {
            "ValuePreserved"
        };
        assert!(!model.check_invariant(invariant, &bad));
    }
}
