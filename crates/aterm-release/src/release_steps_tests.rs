// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// A held shared slice budget in the build child's process tree opens the production tree gate.
/// One continuously held cold parent selects the bounded serial escape.
/// NEGATIVE CONTROLS: queued shared, foreign, undersized, lower-ranked and non-accounting
/// rows do not authorize either schedule, and the old Pending answer for a held cold parent
/// would strand both slices behind the all-token barrier.
#[test]
fn a_held_descendant_shared_slice_budget_opens_the_tree_gate() {
    let record = |pid, what: &str, held: bool, mem: u64| aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::Holder,
        pid,
        start_ms: 1,
        seq: 1,
        checkout: String::new(),
        need: aterm_capacity::Need::new(what)
            .rank(aterm_capacity::Rank::Release)
            .mem(mem)
            .margin(1),
        timing: aterm_capacity::TimingEvidence::default(),
        held: held.then_some(aterm_capacity::Held {
            admitted_ms: 2,
            cores: 0,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    };
    let me = 10;
    let child = crate::interrupt::Proc {
        pid: 20,
        parent: me,
        pgid: 20,
        name: "build".into(),
    };
    let foreign = crate::interrupt::Proc {
        pid: 30,
        parent: 1,
        pgid: 30,
        name: "other".into(),
    };
    let serial = crate::slice_units::serial_parent_bytes(40_u64 << 30);
    for (held, bytes, expected) in [
        (true, serial, super::SliceMemory::Serial),
        (false, serial, super::SliceMemory::Pending),
        // An older cutter's 40 GiB serial row cannot cover the now-explicit
        // 40 GiB private pool and its live driver.
        (true, 40_u64 << 30, super::SliceMemory::StaleBudget),
        (true, 35_u64 << 30, super::SliceMemory::StaleBudget),
        (true, 1_u64 << 30, super::SliceMemory::Pending),
    ] {
        let packed = record(20, crate::slice_units::SERIAL_PARENT, held, bytes);
        assert_eq!(
            super::slices_hold_memory_in(
                u32::try_from(me).unwrap(),
                44_u64 << 30,
                &[packed],
                std::slice::from_ref(&child)
            ),
            expected
        );
    }
    let shared = record(
        20,
        "release slice memory budget",
        true,
        crate::slice_units::MIN_SHARED,
    );
    assert_eq!(
        super::slices_hold_memory_in(
            u32::try_from(me).unwrap(),
            crate::slice_units::MIN_SHARED,
            std::slice::from_ref(&shared),
            std::slice::from_ref(&child)
        ),
        super::SliceMemory::Held
    );
    let calibrated_minimum = 55 * (1_u64 << 30);
    assert_eq!(
        super::slices_hold_memory_in(
            u32::try_from(me).unwrap(),
            calibrated_minimum,
            std::slice::from_ref(&shared),
            std::slice::from_ref(&child),
        ),
        super::SliceMemory::StaleBudget,
        "a held parent sized from an older book must release the barrier to build alone"
    );
    let calibrated = record(20, "release slice memory budget", true, calibrated_minimum);
    assert_eq!(
        super::slices_hold_memory_in(
            u32::try_from(me).unwrap(),
            calibrated_minimum,
            std::slice::from_ref(&calibrated),
            std::slice::from_ref(&child),
        ),
        super::SliceMemory::Held
    );
    assert_eq!(
        super::slices_hold_memory_in(
            u32::try_from(me).unwrap(),
            calibrated_minimum,
            &[record(
                u32::try_from(me).unwrap(),
                "release slice memory budget",
                true,
                calibrated_minimum,
            )],
            &[],
        ),
        super::SliceMemory::Held,
        "NEGATIVE CONTROL: the slice child owns its parent claim directly"
    );
    for malformed in [
        aterm_capacity::Record {
            need: aterm_capacity::Need::new("release slice memory budget")
                .rank(aterm_capacity::Rank::Release)
                .mem(calibrated_minimum),
            ..calibrated.clone()
        },
        aterm_capacity::Record {
            held: calibrated.held.map(|held| aterm_capacity::Held {
                alone: true,
                ..held
            }),
            ..calibrated.clone()
        },
    ] {
        assert_eq!(
            super::slices_hold_memory_in(
                u32::try_from(me).unwrap(),
                calibrated_minimum,
                std::slice::from_ref(&malformed),
                std::slice::from_ref(&child),
            ),
            super::SliceMemory::Pending,
            "a shared parent must carry the anti-ALONE margin and admission"
        );
    }
    assert_eq!(
        super::slices_hold_memory_in(
            u32::try_from(me).unwrap(),
            crate::slice_units::MIN_SHARED,
            &[record(
                20,
                "release slice memory budget",
                false,
                crate::slice_units::MIN_SHARED,
            )],
            std::slice::from_ref(&child)
        ),
        super::SliceMemory::Pending,
        "a queued shared parent has reserved no memory yet"
    );
    assert_eq!(
        super::slices_hold_memory_in(
            u32::try_from(me).unwrap(),
            crate::slice_units::MIN_SHARED,
            &[record(
                30,
                "release slice memory budget",
                true,
                crate::slice_units::MIN_SHARED,
            )],
            &[child.clone(), foreign]
        ),
        super::SliceMemory::Pending,
        "another release's held budget cannot open this one's tree gate"
    );
    for mem in [calibrated_minimum - 1, 1] {
        assert_eq!(
            super::slices_hold_memory_in(
                u32::try_from(me).unwrap(),
                calibrated_minimum,
                &[record(20, "release slice memory budget", true, mem)],
                std::slice::from_ref(&child)
            ),
            if mem >= crate::slice_units::MIN_SHARED {
                super::SliceMemory::StaleBudget
            } else {
                super::SliceMemory::Pending
            },
            "a shared-label claim below the production floor cannot open the tree gate"
        );
    }
    assert_eq!(
        super::slices_hold_memory_in(
            u32::try_from(me).unwrap(),
            crate::slice_units::MIN_SHARED,
            &[aterm_capacity::Record {
                need: aterm_capacity::Need::new("release slice memory budget")
                    .mem(crate::slice_units::MIN_SHARED),
                ..shared.clone()
            }],
            std::slice::from_ref(&child)
        ),
        super::SliceMemory::Pending,
        "a lower-ranked lookalike cannot open the release tree gate"
    );
    assert_eq!(
        super::slices_hold_memory_in(
            u32::try_from(me).unwrap(),
            crate::slice_units::MIN_SHARED,
            &[aterm_capacity::Record {
                accounting: aterm_capacity::Accounting::Whole,
                ..shared.clone()
            }],
            std::slice::from_ref(&child)
        ),
        super::SliceMemory::Pending,
        "a non-accounting lookalike cannot open the release tree gate"
    );

    let cold = aterm_capacity::Record {
        need: crate::slice_units::cold_parent_need(),
        held: Some(aterm_capacity::Held {
            admitted_ms: 2,
            cores: 0,
            alone: true,
        }),
        ..record(
            20,
            crate::slice_units::COLD_PARENT,
            true,
            crate::slice_units::DRIVER,
        )
    };
    let serial = super::slices_hold_memory_in(
        u32::try_from(me).unwrap(),
        crate::slice_units::MIN_SHARED,
        std::slice::from_ref(&cold),
        std::slice::from_ref(&child),
    );
    assert_eq!(
        serial,
        super::SliceMemory::Serial,
        "one held cold parent opens the bounded serial escape"
    );
    assert_eq!(
        super::slices_hold_memory_in(
            u32::try_from(me).unwrap(),
            crate::slice_units::MIN_SHARED,
            &[aterm_capacity::Record {
                pid: u32::try_from(me).unwrap(),
                ..cold.clone()
            }],
            &[],
        ),
        super::SliceMemory::Serial,
        "NEGATIVE CONTROL: the slice child may own its cold parent directly"
    );
    for malformed in [
        record(
            20,
            crate::slice_units::COLD_PARENT,
            true,
            crate::slice_units::DRIVER,
        ),
        aterm_capacity::Record {
            held: cold.held.map(|held| aterm_capacity::Held {
                alone: false,
                ..held
            }),
            ..cold.clone()
        },
        aterm_capacity::Record {
            need: aterm_capacity::Need::new(crate::slice_units::COLD_PARENT)
                .rank(aterm_capacity::Rank::Release)
                .mem(crate::slice_units::DRIVER)
                .margin(1),
            ..cold.clone()
        },
        aterm_capacity::Record {
            need: aterm_capacity::Need::new(crate::slice_units::COLD_PARENT)
                .rank(aterm_capacity::Rank::Release)
                .mem(crate::slice_units::DRIVER - 1)
                .margin(1)
                .alone(),
            ..cold.clone()
        },
        aterm_capacity::Record {
            need: aterm_capacity::Need::new(crate::slice_units::COLD_PARENT)
                .mem(crate::slice_units::DRIVER)
                .margin(1)
                .alone(),
            ..cold.clone()
        },
        aterm_capacity::Record {
            need: aterm_capacity::Need::new(crate::slice_units::COLD_PARENT)
                .rank(aterm_capacity::Rank::Release)
                .mem(crate::slice_units::DRIVER)
                .alone(),
            ..cold.clone()
        },
        aterm_capacity::Record {
            accounting: aterm_capacity::Accounting::Whole,
            ..cold.clone()
        },
        aterm_capacity::Record {
            pid: 30,
            ..cold.clone()
        },
    ] {
        assert_eq!(
            super::slices_hold_memory_in(
                u32::try_from(me).unwrap(),
                crate::slice_units::MIN_SHARED,
                std::slice::from_ref(&malformed),
                std::slice::from_ref(&child),
            ),
            super::SliceMemory::Pending,
            "the cold parent and its held state must carry the exact exclusive envelope"
        );
    }
    let waiting = aterm_capacity::Record {
        held: None,
        ..cold.clone()
    };
    assert_eq!(
        super::slices_hold_memory_in(
            u32::try_from(me).unwrap(),
            crate::slice_units::MIN_SHARED,
            std::slice::from_ref(&waiting),
            std::slice::from_ref(&child),
        ),
        super::SliceMemory::Pending,
        "a queued cold parent reserves no memory yet"
    );

    let ended = std::cell::Cell::new(false);
    let released = std::cell::Cell::new(false);
    super::await_slice_memory(
        &|| ended.get(),
        || serial,
        || released.set(true),
        || {
            assert!(
                released.get(),
                "learning tokens return before its build wait"
            );
            ended.set(true);
        },
    );
    assert!(ended.get(), "learning admission waits for build completion");

    // Bind the production detector and wait ordering to QualifyFallbackAdmission. The parent
    // remains held across native and compat; either keeping the barrier or dropping the parent
    // between them breaks the model's laws.
    let model = aterm_models_capacity::qualify_fallback_admission_model();
    let mut state = model.init_state();
    assert!(model.fire("ColdParentHolds", &mut state));
    assert_eq!(
        serial == super::SliceMemory::Serial,
        model.action_enabled("SerialEscape", &state)
    );
    assert!(!model.action_enabled("TreeStarts", &state));
    assert!(model.fire("SerialEscape", &mut state));
    assert!(model.fire("NativeSliceEnds", &mut state));
    assert_eq!(state["parent"], 1, "the parent spans the slice handoff");
    assert!(!model.action_enabled("TreeStarts", &state));
    assert!(model.fire("CompatSliceEnds", &mut state));
    assert!(model.fire("ColdParentEnds", &mut state));
    assert!(model.action_enabled("TreeStarts", &state));
    assert!(model.fire("TreeStarts", &mut state));
    assert_eq!(state["tree"], 1);
    assert!(aterm_spec::interp::bmc(&model).is_ok());
    assert!(
        aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err(),
        "KeepBarrier, DropParentBetweenSlices or TreeStartsEarly must break the fallback laws"
    );
}

/// Only the verifier descendant's covering, Release-rank whole-stage Tippy reservation that
/// reserves memory opens the build-first compiler-token barrier once held, beside the slice
/// parent or alone. A queued valid row selects the bounded serial escape. Its size is the
/// verifier's to measure, so none is pinned here. NEGATIVE CONTROLS: empty, lower-ranked,
/// non-covering or wrongly accounted exact rows refuse the tree after transferring its tokens to
/// build; foreign and merely similarly named rows authorize no path.
#[test]
fn only_an_exact_held_descendant_tippy_claim_opens_the_compiler_barrier() {
    let shapes = super::tree_lint_claim_shapes().expect("release plan has Tippy");
    let row = |pid, what: &str, held: bool, mem: u64| aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::Holder,
        pid,
        start_ms: 1,
        seq: 1,
        checkout: String::new(),
        need: aterm_capacity::Need::new(what)
            .rank(aterm_capacity::Rank::Release)
            .mem(mem)
            .covers(),
        timing: aterm_capacity::TimingEvidence::default(),
        held: held.then_some(aterm_capacity::Held {
            admitted_ms: 2,
            cores: 0,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    };
    // Any measured size: the verifier sizes the claim, the detector authenticates its shape.
    const CLAIM: u64 = 12 << 30;
    let me = 10_u32;
    let verifier = crate::interrupt::Proc {
        pid: 20,
        parent: i32::try_from(me).unwrap(),
        pgid: 20,
        name: "aterm-verify".into(),
    };
    let foreign = crate::interrupt::Proc {
        pid: 30,
        parent: 1,
        pgid: 30,
        name: "aterm-verify".into(),
    };
    for shape in &shapes {
        let valid = row(20, &shape.label, true, CLAIM);
        assert_eq!(
            super::tree_lint_claim_in(
                me,
                std::slice::from_ref(&valid),
                std::slice::from_ref(&verifier),
                &shapes,
            ),
            Some(super::TreeLintClaim::Held)
        );
        assert_eq!(
            super::tree_lint_claim_in(
                me,
                &[row(20, &shape.label, false, CLAIM)],
                std::slice::from_ref(&verifier),
                &shapes,
            ),
            Some(super::TreeLintClaim::Waiting),
            "a valid queued claim selects the bounded serial escape, not concurrent admission"
        );
    }

    let whole = &shapes[0];
    assert_eq!(
        super::tree_lint_claim_in(
            me,
            &[row(20, &whole.label, true, 0)],
            std::slice::from_ref(&verifier),
            &shapes,
        ),
        Some(super::TreeLintClaim::Invalid),
        "NEGATIVE CONTROL: a whole-stage Tippy claim that reserves no memory is unsafe"
    );
    assert_eq!(
        super::tree_lint_claim_in(
            me,
            &[row(20, &whole.label, true, 1)],
            std::slice::from_ref(&verifier),
            &shapes,
        ),
        Some(super::TreeLintClaim::Held),
        "a claim of any size the verifier measured opens the barrier"
    );

    let valid = row(20, &whole.label, true, CLAIM);
    let similarly_named = row(20, "verify tippy lint (-p aterm)", true, CLAIM);
    assert!(
        super::tree_lint_claim_in(
            me,
            std::slice::from_ref(&similarly_named),
            std::slice::from_ref(&verifier),
            &shapes,
        )
        .is_none(),
        "a merely similar label is not this release's claim"
    );
    for invalid in [
        aterm_capacity::Record {
            need: aterm_capacity::Need::new(&whole.label).mem(CLAIM).covers(),
            ..valid.clone()
        },
        aterm_capacity::Record {
            need: aterm_capacity::Need::new(&whole.label)
                .rank(aterm_capacity::Rank::Release)
                .mem(CLAIM),
            ..valid.clone()
        },
        aterm_capacity::Record {
            accounting: aterm_capacity::Accounting::Whole,
            ..valid.clone()
        },
    ] {
        assert_eq!(
            super::tree_lint_claim_in(
                me,
                std::slice::from_ref(&invalid),
                std::slice::from_ref(&verifier),
                &shapes,
            ),
            Some(super::TreeLintClaim::Invalid),
            "invalid exact claim must refuse the tree: {invalid:?}"
        );
    }

    let alone = aterm_capacity::Record {
        need: aterm_capacity::Need::new(&whole.label)
            .rank(aterm_capacity::Rank::Release)
            .mem(CLAIM)
            .covers()
            .alone(),
        ..valid.clone()
    };
    assert_eq!(
        super::tree_lint_claim_in(
            me,
            &[aterm_capacity::Record {
                held: None,
                ..alone.clone()
            }],
            std::slice::from_ref(&verifier),
            &shapes,
        ),
        Some(super::TreeLintClaim::Waiting),
        "a lint never measured here queues to run alone: the bounded serial escape"
    );
    assert_eq!(
        super::tree_lint_claim_in(
            me,
            &[aterm_capacity::Record {
                held: Some(aterm_capacity::Held {
                    admitted_ms: 2,
                    cores: 0,
                    alone: true,
                }),
                ..alone
            }],
            std::slice::from_ref(&verifier),
            &shapes,
        ),
        Some(super::TreeLintClaim::Held),
        "a claim held alone admits no memory claim beside it, the strongest bound"
    );

    let waiting = row(20, &whole.label, false, CLAIM);
    for records in [
        vec![waiting.clone(), valid.clone()],
        vec![valid.clone(), waiting.clone()],
    ] {
        assert_eq!(
            super::tree_lint_claim_in(me, &records, std::slice::from_ref(&verifier), &shapes,),
            Some(super::TreeLintClaim::Held),
            "a held duplicate dominates a queued duplicate in either file order"
        );
    }
    let malformed = aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::Whole,
        ..valid.clone()
    };
    for (dominant, expected) in [
        (valid.clone(), super::TreeLintClaim::Held),
        (waiting, super::TreeLintClaim::Waiting),
    ] {
        for records in [
            vec![malformed.clone(), dominant.clone()],
            vec![dominant.clone(), malformed.clone()],
        ] {
            assert_eq!(
                super::tree_lint_claim_in(me, &records, std::slice::from_ref(&verifier), &shapes,),
                Some(expected),
                "NEGATIVE CONTROL: a valid duplicate dominates a malformed duplicate in \
                 either file order"
            );
        }
    }
    assert!(
        super::tree_lint_claim_in(
            me,
            &[row(30, &whole.label, true, CLAIM)],
            &[verifier, foreign],
            &shapes,
        )
        .is_none(),
        "another release's verifier cannot release this pool"
    );

    let model = aterm_models_capacity::qualify_memory_model();
    let mut state = model.init_state();
    assert!(model.fire("ParentAdmit", &mut state));
    assert!(model.action_enabled("WholeLintAdmit", &state));
    assert!(model.fire("WholeLintAdmit", &mut state));
    assert!(model.action_enabled("TreeStarts", &state));
    assert!(aterm_spec::interp::bmc(&model).is_ok());
    assert!(
        aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 2)).is_err(),
        "NEGATIVE CONTROL: the old delayed-admission ordering must break its scheduling law"
    );
}

fn queued_tree_record() -> aterm_capacity::Record {
    let shapes = super::tree_lint_claim_shapes().unwrap();
    aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::Holder,
        pid: 20,
        start_ms: 10,
        seq: 1,
        checkout: "queued-tree-test".into(),
        need: aterm_capacity::Need::new(shapes[0].label.clone())
            .rank(aterm_capacity::Rank::Release)
            .mem(12 << 30)
            .covers(),
        timing: aterm_capacity::TimingEvidence::default(),
        held: None,
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

#[test]
fn a_shared_tree_cpu_proof_rejects_replaced_changed_and_alone_claims() {
    let waiting = queued_tree_record();
    let proof = super::SharedTreeMemory::new(&waiting, super::TreeLintClaim::Waiting).unwrap();
    let mut held = waiting.clone();
    held.held = Some(aterm_capacity::Held {
        admitted_ms: 30,
        cores: 0,
        alone: false,
    });
    assert_eq!(proof.held_in(std::slice::from_ref(&held)), held.held);
    assert!(proof.held_in(std::slice::from_ref(&waiting)).is_none());
    assert!(proof.held_in(&[]).is_none());
    let mutations: [fn(&mut aterm_capacity::Record); 8] = [
        |row: &mut aterm_capacity::Record| row.pid += 1,
        |row: &mut aterm_capacity::Record| row.start_ms += 1,
        |row: &mut aterm_capacity::Record| row.seq += 1,
        |row: &mut aterm_capacity::Record| row.checkout.push_str("-other"),
        |row: &mut aterm_capacity::Record| row.need.mem += 1,
        |row: &mut aterm_capacity::Record| row.need.covers = false,
        |row: &mut aterm_capacity::Record| row.need.alone = true,
        |row: &mut aterm_capacity::Record| row.held.as_mut().unwrap().alone = true,
    ];
    for mutate in mutations {
        let mut changed = held.clone();
        mutate(&mut changed);
        assert!(proof.held_in(&[changed]).is_none());
    }
    let mut alone = waiting.clone();
    alone.need.alone = true;
    assert!(super::SharedTreeMemory::new(&alone, super::TreeLintClaim::Waiting).is_none());
    assert!(super::SharedTreeMemory::new(&waiting, super::TreeLintClaim::Invalid).is_none());
    let admitted = super::SharedTreeMemory::new(&held, super::TreeLintClaim::Held).unwrap();
    assert_eq!(admitted.held_in(std::slice::from_ref(&held)), held.held);
    assert!(super::SharedTreeMemory::new(&held, super::TreeLintClaim::Waiting).is_none());
    assert!(super::SharedTreeMemory::new(&waiting, super::TreeLintClaim::Held).is_none());
    held.held.as_mut().unwrap().alone = true;
    assert!(super::SharedTreeMemory::new(&held, super::TreeLintClaim::Held).is_none());
}

#[test]
fn an_initially_admitted_tree_borrows_free_cores_and_stops_when_its_claim_ends() {
    use std::cell::{Cell, RefCell};
    let dir = aterm_tempfile::tempdir().unwrap();
    let jobs =
        aterm_capacity::BranchShares::new(&dir.path().join("jobs"), 18, 4, [true, true]).unwrap();
    let live_build = [
        jobs.pool(false).try_acquire().unwrap().unwrap(),
        jobs.pool(false).try_acquire().unwrap().unwrap(),
    ];
    let live_tree = RefCell::new(vec![jobs.pool(true).try_acquire().unwrap().unwrap()]);
    let mut record = queued_tree_record();
    record.held = Some(aterm_capacity::Held {
        admitted_ms: 30,
        cores: 0,
        alone: false,
    });
    let claim = super::SharedTreeMemory::new(&record, super::TreeLintClaim::Held).unwrap();
    let observed = Cell::new(record.held);
    let pauses = Cell::new(0);
    let event = super::await_tree_cpu_lending(
        &jobs,
        &claim,
        &|| false,
        || observed.get(),
        || {
            while let Some(token) = jobs.pool(true).try_acquire().unwrap() {
                live_tree.borrow_mut().push(token);
            }
            match pauses.get() {
                0 => {
                    assert_eq!(live_tree.borrow().len(), 7);
                    jobs.finish(2);
                }
                1 => {
                    assert_eq!(live_tree.borrow().len(), 9);
                    observed.set(None);
                }
                _ => panic!("a removed admitted claim must stop the observer"),
            }
            pauses.set(pauses.get() + 1);
        },
    )
    .unwrap();
    assert_eq!(pauses.get(), 2);
    assert_eq!(event["transferred_cores"].as_u64(), Some(8));
    assert_eq!(event["transfers"].as_array().unwrap().len(), 2);
    assert_eq!(
        event["observer_residence_is_starvation"].as_bool(),
        Some(false)
    );
    let mut free_build = Vec::new();
    while let Some(token) = jobs.pool(false).try_acquire().unwrap() {
        free_build.push(token);
    }
    assert_eq!(
        live_build.len() + live_tree.borrow().len() + free_build.len(),
        18
    );
    drop((live_tree, live_build, free_build));
    jobs.build_compilers_drained();
    jobs.finish(0);
    jobs.finish(1);
}

#[test]
fn a_waiting_shared_tree_borrows_nothing_until_admission_then_keeps_the_build_floor() {
    use std::cell::Cell;
    let dir = aterm_tempfile::tempdir().unwrap();
    let jobs =
        aterm_capacity::BranchShares::new(&dir.path().join("jobs"), 6, 1, [true, true]).unwrap();
    jobs.reserve_initial().unwrap().release_to_build().unwrap();
    let live_build = [
        jobs.pool(false).try_acquire().unwrap().unwrap(),
        jobs.pool(false).try_acquire().unwrap().unwrap(),
    ];
    let record = queued_tree_record();
    let proof = super::SharedTreeMemory::new(&record, super::TreeLintClaim::Waiting).unwrap();
    let polls = Cell::new(0);
    let stopped = Cell::new(false);
    let event = super::await_tree_cpu_lending(
        &jobs,
        &proof,
        &|| stopped.get(),
        || {
            polls.set(polls.get() + 1);
            (polls.get() >= 3).then_some(aterm_capacity::Held {
                admitted_ms: 30,
                cores: 0,
                alone: false,
            })
        },
        || {
            if polls.get() < 3 {
                assert!(jobs.pool(true).try_acquire().unwrap().is_none());
            } else {
                stopped.set(true);
            }
        },
    )
    .unwrap();
    assert_eq!(polls.get(), 3);
    assert_eq!(event["transferred_cores"].as_u64(), Some(2));
    let mut tree = Vec::new();
    while let Some(token) = jobs.pool(true).try_acquire().unwrap() {
        tree.push(token);
    }
    let free_build = jobs.pool(false).try_acquire().unwrap().unwrap();
    assert!(jobs.pool(false).try_acquire().unwrap().is_none());
    assert_eq!(tree.len(), 2);
    assert_eq!(live_build.len() + 1, 3);
    drop((live_build, free_build, tree));
    jobs.build_compilers_drained();
    for branch in [0, 1, 2] {
        jobs.finish(branch);
    }
}

#[test]
fn shared_tree_lending_cancels_and_never_lends_on_unavailable_or_alone_observations() {
    use std::cell::Cell;
    let record = queued_tree_record();
    let proof = super::SharedTreeMemory::new(&record, super::TreeLintClaim::Waiting).unwrap();
    for observed in [
        None,
        Some(aterm_capacity::Held {
            admitted_ms: 30,
            cores: 0,
            alone: true,
        }),
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let jobs = aterm_capacity::BranchShares::new(&dir.path().join("jobs"), 6, 0, [true, true])
            .unwrap();
        jobs.reserve_initial().unwrap().release_to_build().unwrap();
        let stopped = Cell::new(false);
        assert!(
            super::await_tree_cpu_lending(
                &jobs,
                &proof,
                &|| stopped.get(),
                || observed,
                || stopped.set(true),
            )
            .is_none()
        );
        assert!(jobs.serial_tree_waiting());
        assert!(jobs.pool(true).try_acquire().unwrap().is_none());
    }
}

#[test]
fn a_queued_tree_gets_a_core_only_after_its_shared_claim_is_held() {
    use std::cell::Cell;
    let dir = aterm_tempfile::tempdir().unwrap();
    let jobs =
        aterm_capacity::BranchShares::new(&dir.path().join("jobs"), 6, 1, [true, true]).unwrap();
    jobs.reserve_initial().unwrap().release_to_build().unwrap();
    let first = jobs.pool(false).try_acquire().unwrap().unwrap();
    let second = jobs.pool(false).try_acquire().unwrap().unwrap();
    let waiting = queued_tree_record();
    let proof = super::SharedTreeMemory::new(&waiting, super::TreeLintClaim::Waiting).unwrap();
    let polls = Cell::new(0);
    let pauses = Cell::new(0);
    let resumed = super::await_tree_cpu_resumption(
        &jobs,
        &proof,
        &|| false,
        || {
            polls.set(polls.get() + 1);
            (polls.get() == 3).then_some(aterm_capacity::Held {
                admitted_ms: 30,
                cores: 0,
                alone: false,
            })
        },
        || {
            assert!(jobs.pool(true).try_acquire().unwrap().is_none());
            pauses.set(pauses.get() + 1);
        },
    )
    .unwrap();
    assert_eq!(pauses.get(), 2);
    assert_eq!(resumed["transferred_cores"].as_u64(), Some(1));
    let tree = jobs.pool(true).try_acquire().unwrap().unwrap();
    let mut build = Vec::new();
    while let Some(token) = jobs.pool(false).try_acquire().unwrap() {
        build.push(token);
    }
    let live = [first, second, tree];
    assert_eq!(
        build.len() + live.len() + 1,
        6,
        "live build clients, tree and export stay within the grant"
    );
    drop((live, build));
    assert!(!jobs.serial_tree_waiting());
}

#[test]
fn a_queued_tree_observer_cancels_and_keeps_the_alone_and_one_core_paths() {
    use std::cell::Cell;
    let waiting = queued_tree_record();
    let proof = super::SharedTreeMemory::new(&waiting, super::TreeLintClaim::Waiting).unwrap();
    for alone in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let jobs = aterm_capacity::BranchShares::new(&dir.path().join("jobs"), 3, 0, [true, true])
            .unwrap();
        jobs.reserve_initial().unwrap().release_to_build().unwrap();
        let ended = Cell::new(false);
        assert!(
            super::await_tree_cpu_resumption(
                &jobs,
                &proof,
                &|| ended.get(),
                || alone.then_some(aterm_capacity::Held {
                    admitted_ms: 30,
                    cores: 0,
                    alone: true
                }),
                || {
                    assert!(jobs.pool(true).try_acquire().unwrap().is_none());
                    ended.set(true);
                },
            )
            .is_none()
        );
        assert!(jobs.serial_tree_waiting());
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let jobs =
        aterm_capacity::BranchShares::new(&dir.path().join("jobs"), 1, 0, [true, true]).unwrap();
    jobs.reserve_initial().unwrap().release_to_build().unwrap();
    assert!(
        super::await_tree_cpu_resumption(
            &jobs,
            &proof,
            &|| false,
            || Some(aterm_capacity::Held {
                admitted_ms: 30,
                cores: 0,
                alone: false
            }),
            || jobs.build_compilers_drained(),
        )
        .is_none()
    );
    assert!(!jobs.serial_tree_waiting());
    assert!(jobs.pool(true).try_acquire().unwrap().is_some());
}

/// Capacity inspection is never a liveness dependency of the all-token barrier. An unread
/// detector, a held continuous cold parent and a stale shared budget all release the tokens and
/// wait for build completion before returning; an unread tree detector returns immediately
/// because the slice parent is already protected. NEGATIVE CONTROLS: treating either slice escape as
/// Pending reaches the injected pause before release, and returning before build completion
/// violates the final assertion.
#[test]
fn unread_admission_detectors_take_the_bounded_escape_without_deadlock() {
    use std::cell::Cell;

    for escape in [
        super::SliceMemory::Unavailable,
        super::SliceMemory::Serial,
        super::SliceMemory::StaleBudget,
    ] {
        let build_ended = Cell::new(false);
        let released = Cell::new(false);
        let pauses = Cell::new(0_u8);
        super::await_slice_memory(
            &|| build_ended.get(),
            || escape,
            || released.set(true),
            || {
                assert!(released.get(), "tokens return before the serial build wait");
                pauses.set(pauses.get() + 1);
                build_ended.set(true);
            },
        );
        assert!(released.get());
        assert!(build_ended.get());
        assert_eq!(pauses.get(), 1, "the serial escape waited for build end");
    }

    let tree_pauses = Cell::new(0_u8);
    assert!(super::await_tree_memory(
        &|| false,
        || super::TreeLintObservation::Unavailable,
        || panic!("an unread tree detector keeps the original branch shares"),
        || tree_pauses.set(tree_pauses.get() + 1),
    ));
    assert_eq!(
        tree_pauses.get(),
        0,
        "an unread tree detector cannot retain every compiler token"
    );

    for (claim, must_serialize, tree_may_run) in [
        (super::TreeLintClaim::Held, false, true),
        (super::TreeLintClaim::Waiting, true, true),
        (super::TreeLintClaim::Invalid, true, false),
    ] {
        let serialized = Cell::new(false);
        let admitted = super::await_tree_memory(
            &|| false,
            || super::TreeLintObservation::Claim(claim),
            || serialized.set(true),
            || panic!("an observed exact Tippy claim needs no further scan"),
        );
        assert_eq!(
            serialized.get(),
            must_serialize,
            "a queued or malformed Tippy claim transfers every token to build"
        );
        assert_eq!(
            admitted, tree_may_run,
            "NEGATIVE CONTROL: a malformed envelope must stop the tree"
        );
    }
}

/// Bind both production admission seams to the real private FIFOs. NEGATIVE CONTROLS:
/// replacing the transfer with an ordinary barrier drop yields the initial 2/1 split;
/// replacing it with a no-op yields 0/0 while the reservation remains held.
#[test]
fn serial_admission_seams_transfer_the_exact_bounded_allocation_to_build() {
    use std::cell::Cell;

    fn drain(pool: &aterm_capacity::Jobserver) -> Vec<aterm_capacity::Token<'_>> {
        let mut tokens = Vec::new();
        while let Some(token) = pool.try_acquire().unwrap() {
            tokens.push(token);
        }
        tokens
    }

    let assert_serial = |tree: bool| {
        let dir = aterm_tempfile::tempdir().unwrap();
        let shares =
            aterm_capacity::BranchShares::new(&dir.path().join("qualify.jobs"), 4, 1, [true, true])
                .unwrap();
        let barrier = std::sync::Mutex::new(Some(shares.reserve_initial().unwrap()));
        if tree {
            assert!(super::await_tree_memory_with_barrier(
                &barrier,
                &|| false,
                || super::TreeLintObservation::Claim(super::TreeLintClaim::Waiting),
                || panic!("an exact queued Tippy claim needs no further scan"),
            ));
        } else {
            let ended = Cell::new(false);
            super::await_slice_memory_with_barrier(
                &barrier,
                &|| ended.get(),
                || super::SliceMemory::Serial,
                || ended.set(true),
            );
            assert!(
                ended.get(),
                "slice serialization waits for build completion"
            );
        }
        assert!(barrier.lock().unwrap().is_none());
        let build = drain(shares.pool(false));
        let tree = drain(shares.pool(true));
        assert_eq!(
            (build.len(), tree.len()),
            (3, 0),
            "serial escape transfers every non-export token to build"
        );
        drop((build, tree));

        shares.finish(2);
        let build = drain(shares.pool(false));
        assert_eq!(build.len(), 4, "export completion stays on serial build");
        assert!(drain(shares.pool(true)).is_empty());
        drop(build);
        shares.finish(0);
        assert!(drain(shares.pool(false)).is_empty());
        assert_eq!(
            drain(shares.pool(true)).len(),
            4,
            "build completion transfers the same bounded allocation to tree"
        );
    };
    assert_serial(false);
    assert_serial(true);

    let dir = aterm_tempfile::tempdir().unwrap();
    let shares =
        aterm_capacity::BranchShares::new(&dir.path().join("concurrent.jobs"), 4, 1, [true, true])
            .unwrap();
    let barrier = std::sync::Mutex::new(Some(shares.reserve_initial().unwrap()));
    assert!(super::await_tree_memory_with_barrier(
        &barrier,
        &|| false,
        || super::TreeLintObservation::Claim(super::TreeLintClaim::Held),
        || panic!("an exact held Tippy claim needs no further scan"),
    ));
    drop(barrier.lock().unwrap().take());
    assert_eq!(
        (
            drain(shares.pool(false)).len(),
            drain(shares.pool(true)).len()
        ),
        (2, 1),
        "held Tippy preserves build-first width and one tree compiler"
    );
}

/// A resume may have only one branch. NEGATIVE CONTROL: the former all-compiler allocation
/// gives a build-only one-slot plan's sole token to tree.
#[test]
fn qualify_pool_allocates_only_to_branches_in_the_resume_plan() {
    fn drain(pool: &aterm_capacity::Jobserver) -> Vec<aterm_capacity::Token<'_>> {
        let mut tokens = Vec::new();
        while let Some(token) = pool.try_acquire().unwrap() {
            tokens.push(token);
        }
        tokens
    }

    type ResumeCase<'a> = (u32, &'a [Branch], u32, (usize, usize));
    let cases: &[ResumeCase<'_>] = &[
        (1, &[Branch::Build], 0, (1, 0)),
        (4, &[Branch::Tree], 0, (0, 4)),
        (4, &[Branch::Build, Branch::Export], 1, (3, 0)),
        (4, &[Branch::Tree, Branch::Export], 1, (0, 3)),
        (4, &[Branch::Export], 4, (0, 0)),
    ];
    for &(slots, branches, expected_export, expected_compilers) in cases {
        let dir = aterm_tempfile::tempdir().unwrap();
        let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
        let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, slots));
        let pool = super::qualify_pool(&dir.path().join("resume.jobs"), branches, |need| {
            aterm_capacity::admit_with(
                &ledger,
                &machine,
                need.cores(slots),
                aterm_capacity::Admit::Now,
                aterm_capacity::Pace::default(),
                &mut |_| {},
            )
        })
        .unwrap();
        assert_eq!(pool.export, expected_export);
        let barrier = pool.jobs.reserve_initial().unwrap();
        assert_eq!(
            barrier.len(),
            usize::try_from(slots - expected_export).unwrap()
        );
        drop(barrier);
        let build = drain(pool.jobs.pool(false));
        let tree = drain(pool.jobs.pool(true));
        assert_eq!((build.len(), tree.len()), expected_compilers);
        assert_eq!(
            build.len() + tree.len() + usize::try_from(expected_export).unwrap(),
            usize::try_from(slots).unwrap()
        );
    }

    let dir = aterm_tempfile::tempdir().unwrap();
    let old = aterm_capacity::BranchShares::new(
        &dir.path().join("old-all-compiler.jobs"),
        1,
        0,
        [true, true],
    )
    .unwrap();
    drop(old.reserve_initial().unwrap());
    assert_eq!(
        (drain(old.pool(false)).len(), drain(old.pool(true)).len()),
        (0, 1),
        "NEGATIVE CONTROL: an all-compiler allocation starves a build-only one-slot resume"
    );
}

#[test]
fn a_published_supersession_refusal_names_the_fix_hop_remedy() {
    let previous = super::Line::start("0.189.0", "0123456789abcdef0123456789abcdef01234567", 1);
    let refusal =
        super::SupersedeCheck::Present("the line has a release object; cannot supersede".into())
            .refusal(Some(&previous));
    assert_eq!(refusal.remedy, crate::release::fix_remedy(previous.head()));
}

#[test]
fn the_staging_classifier_allows_replacement_of_the_same_version() {
    assert!(!super::staged_ahead_of_version("0.188.0", "0.189.0").unwrap());
    assert!(!super::staged_ahead_of_version("0.189.0", "0.189.0").unwrap());
    assert!(super::staged_ahead_of_version("0.190.0", "0.189.0").unwrap());
    assert!(super::staged_ahead_of_version("not-a-version", "0.189.0").is_err());
}

#[test]
fn exact_supersession_reread_uses_the_complete_record() {
    use super::*;

    let previous = Line::start("0.189.0", &"1".repeat(40), 1);
    let replacement = Line::start("0.189.0", &"2".repeat(40), 2);
    let requested = supersession_record(previous.clone(), replacement.clone());
    assert!(matches!(
        classify_supersession_reread(&requested, Ok(Some(requested.clone())), true),
        SupersedeResult::Committed(_)
    ));

    let mut different_reason = requested.clone();
    different_reason.reason.push_str(" mutated");
    assert!(matches!(
        classify_supersession_reread(&requested, Ok(Some(different_reason)), true),
        SupersedeResult::Refused {
            retry: false,
            winner: Some(line),
            ..
        } if line == replacement
    ));
    let mut different_previous = requested.clone();
    different_previous.previous.started += 1;
    assert!(matches!(
        classify_supersession_reread(&requested, Ok(Some(different_previous)), true),
        SupersedeResult::Refused {
            winner: Some(_),
            ..
        }
    ));
    assert!(matches!(
        classify_supersession_reread(&requested, Ok(None), true),
        SupersedeResult::Refused {
            retry: true,
            winner: None,
            ..
        }
    ));
    let earlier = supersession_record(Line::start("0.189.0", &"0".repeat(40), 0), previous);
    assert!(matches!(
        classify_supersession_reread(&requested, Ok(Some(earlier.clone())), true),
        SupersedeResult::Refused {
            retry: true,
            winner: None,
            ..
        }
    ));
    assert!(matches!(
        classify_claim_stop(&requested, "claim stopped".into(), Ok(Some(earlier))),
        SupersedeResult::Refused { why, retry: false, winner: None }
            if why == "claim stopped"
    ));
    assert!(matches!(
        classify_supersession_reread(&requested, Err("unreadable".into()), true),
        SupersedeResult::Unknown(why) if why == "unreadable"
    ));
}

/// A claim stop alone restores nothing. NEGATIVE CONTROLS: an exact record commits, a
/// different record returns its winner, and an unreadable source remains unknown; only a
/// proved-empty reread preserves the claim's original refusal.
#[test]
fn claim_stop_is_decided_by_the_exact_source_record() {
    use super::*;

    let previous = Line::start("0.189.0", &"1".repeat(40), 1);
    let replacement = Line::start("0.189.0", &"2".repeat(40), 2);
    let requested = supersession_record(previous, replacement.clone());
    assert!(matches!(
        classify_claim_stop(
            &requested,
            "claim stopped".into(),
            Ok(Some(requested.clone()))
        ),
        SupersedeResult::Committed(_)
    ));
    let mut competing = requested.clone();
    competing.replacement = Line::start("0.189.0", &"3".repeat(40), 3);
    assert!(matches!(
        classify_claim_stop(
            &requested,
            "claim stopped".into(),
            Ok(Some(competing.clone()))
        ),
        SupersedeResult::Refused {
            winner: Some(line),
            ..
        } if line == competing.replacement
    ));
    assert!(matches!(
        classify_claim_stop(&requested, "claim stopped".into(), Ok(None)),
        SupersedeResult::Refused {
            why,
            winner: None,
            ..
        } if why == "claim stopped"
    ));
    assert!(matches!(
        classify_claim_stop(&requested, "claim stopped".into(), Err("unreadable".into())),
        SupersedeResult::Unknown(why) if why == "unreadable"
    ));
}

/// A public-surface refusal can arrive after an aged claim was adopted elsewhere. NEGATIVE
/// CONTROL: the durable competing record wins over the stale scan result; treating that scan
/// as authoritative would return its refusal with no winner.
#[test]
fn public_state_stop_is_decided_by_the_competing_record() {
    use super::*;

    let previous = Line::start("0.189.0", &"1".repeat(40), 1);
    let replacement = Line::start("0.189.0", &"2".repeat(40), 2);
    let requested = supersession_record(previous.clone(), replacement);
    let mut competing = requested.clone();
    competing.replacement = Line::start("0.189.0", &"3".repeat(40), 3);
    let result = settle_post_claim_public_state(
        Err(SupersedeCheck::Present(
            "the line has a release object; cannot supersede".into(),
        )),
        &previous,
        |why| classify_claim_stop(&requested, why, Ok(Some(competing.clone()))),
    )
    .expect_err("the stale public-state stop is classified by the durable record");
    assert!(matches!(
        result,
        SupersedeResult::Refused {
            winner: Some(line),
            retry: false,
            ..
        } if line == competing.replacement
    ));
}

/// A competing record cannot move local state unless its replacement is itself a verified
/// release line. NEGATIVE CONTROL: the same structured winner with a nonexistent base is
/// classified as unknown instead of being handed to the transaction's adoption path.
#[test]
fn an_unverified_competing_supersession_is_not_a_winner() {
    use super::*;

    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = GitCli::new(&repo);
    crate::ledger::git_ok(&git, &["init", "-q"]).unwrap();
    let real = Real::new(&repo, Some(&repo), true, false).unwrap();
    let invalid = Line::start("0.189.0", &"f".repeat(40), 1);
    let classified = real.verify_supersede_result(SupersedeResult::Refused {
        why: "a competing writer won".into(),
        retry: false,
        winner: Some(invalid),
    });
    assert!(matches!(classified, SupersedeResult::Unknown(_)));
}

fn supersede_staging_fixture(slug: &str, reference: &str, path: &str) -> Result<Vec<u8>, String> {
    assert_eq!(reference, "main");
    assert_eq!(path, "Cargo.toml");
    let version = match slug {
        "fixture/retired-staging" => "0.188.0",
        "fixture/retired-hop-staging" => "0.190.0",
        "fixture/replacement-staging" => "0.188.0",
        other => return Err(format!("unexpected staging repository {other}")),
    };
    Ok(format!("[workspace.package]\nversion = \"{version}\"\n").into_bytes())
}

/// Production preflight reads every retired-line staging declaration before any selection
/// write. NEGATIVE CONTROL: reading only either base, or `replacement.base`, sees only older
/// staged versions and cannot detect that staging advanced beyond the requested version.
#[test]
fn production_supersede_preflight_reads_the_retired_lines_staging_repo() {
    use super::*;

    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("publish")).unwrap();
    let git = GitCli::new(&repo);
    let run = |args: &[&str]| crate::ledger::git_ok(&git, args).unwrap();
    run(&["init", "-q"]);
    let commit = || {
        run(&["add", "."]);
        run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ]);
        run(&["rev-parse", "HEAD"]).stdout_utf8().trim().to_string()
    };
    std::fs::write(
        repo.join("publish/config.sh"),
        "STAGING_REMOTE_DEFAULT=\"https://github.com/fixture/retired-staging.git\"\n",
    )
    .unwrap();
    let retired = commit();
    std::fs::write(
        repo.join("publish/config.sh"),
        "STAGING_REMOTE_DEFAULT=\"https://github.com/fixture/retired-hop-staging.git\"\n",
    )
    .unwrap();
    let retired_hop = commit();
    std::fs::write(
        repo.join("publish/config.sh"),
        "STAGING_REMOTE_DEFAULT=\"https://github.com/fixture/replacement-staging.git\"\n",
    )
    .unwrap();
    let replacement = commit();
    let mut previous = Line::start("0.189.0", &retired, 1);
    previous.hops.push(Hop {
        commit: retired_hop,
        carries: "fixture-fix".into(),
        why: "changes staging policy".into(),
        paths: vec!["publish/config.sh".into()],
    });
    let proposed = Line::start("0.189.0", &replacement, 2);
    let head_before = run(&["rev-parse", "HEAD"]).stdout_utf8().trim().to_string();
    let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
    real.staging_file = |_, _, _| Ok(b"[workspace.package]\nversion = \"0.189.0\"\n".to_vec());
    real.supersede_staging_state(&previous, &proposed).unwrap();
    // NEGATIVE CONTROL: the former >= comparison refuses this same staged version,
    // leaving only the fix-hop route even when a later main series cannot be carried.
    let staged_version = [0_u64, 189, 0];
    let requested_version = proposed
        .version
        .split('.')
        .map(|n| n.parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    assert!(staged_version.as_slice() >= requested_version.as_slice());
    real.staging_file = supersede_staging_fixture;
    let refusal = Backend::preflight_supersede(&mut real, &proposed, Some(&previous))
        .expect_err("a retired hop has a newer staged version");
    assert!(refusal.why.contains("staging has advanced to v0.190.0"));
    assert_eq!(
        run(&["rev-parse", "HEAD"]).stdout_utf8().trim(),
        head_before
    );
    assert!(!real.release.join(crate::release::SELECTION_FILE).exists());
}

fn supersede_release_fixture(slug: &str, tag: &str) -> Result<bool, String> {
    assert_eq!(tag, "v0.189.0");
    match slug {
        "fixture/base-channel" | "fixture/replacement-channel" => Ok(false),
        "fixture/hop-channel" => Ok(true),
        other => Err(format!("unexpected release channel {other}")),
    }
}

/// Every historical channel is checked. NEGATIVE CONTROL: checking only the base or current
/// checkout misses the release object addressed by the retired hop.
#[test]
fn supersede_checks_each_release_channel_declared_by_the_retired_line() {
    use super::*;

    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = GitCli::new(&repo);
    let run = |args: &[&str]| crate::ledger::git_ok(&git, args).unwrap();
    run(&["init", "-q"]);
    let manifest = |channel: &str| {
        format!(
            "[workspace]\nmembers = []\n\
             [workspace.package]\nversion = \"0.189.0\"\n\
             repository = \"https://github.com/fixture/source\"\n\
             [workspace.metadata.aterm]\nupdate_channel = \"{channel}\"\n"
        )
    };
    let commit = || {
        run(&["add", "Cargo.toml"]);
        run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ]);
        run(&["rev-parse", "HEAD"]).stdout_utf8().trim().to_string()
    };
    std::fs::write(repo.join("Cargo.toml"), manifest("fixture/base-channel")).unwrap();
    let base = commit();
    std::fs::write(repo.join("Cargo.toml"), manifest("fixture/hop-channel")).unwrap();
    let hop = commit();
    std::fs::write(
        repo.join("Cargo.toml"),
        manifest("fixture/replacement-channel"),
    )
    .unwrap();
    let _replacement = commit();
    let mut previous = Line::start("0.189.0", &base, 1);
    previous.hops.push(Hop {
        commit: hop,
        carries: "fixture-fix".into(),
        why: "changes release channel".into(),
        paths: vec!["Cargo.toml".into()],
    });
    let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
    real.release_exists = supersede_release_fixture;
    assert!(matches!(
        real.supersede_release_state(&previous, "v0.189.0"),
        Err(SupersedeCheck::Present(_))
    ));
}

#[test]
fn diagnostic_fixture_layout_matches_the_inherited_verifier_layout() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let repo = scratch.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let env = aterm_verify::EnvSnapshot {
        home: scratch.path().to_owned(),
        cargo_target_dir: Some(scratch.path().join("cold.noindex")),
        ..Default::default()
    };
    let ctx = aterm_verify::Ctx::new(
        repo.clone(),
        aterm_verify::Scope::workspace(),
        env.clone(),
        scratch.path().join("logs"),
    );
    let layout = super::diagnostic_verify_layout_from_env(&repo, &env).unwrap();
    assert_eq!(layout.target(), ctx.target_dir);
    assert_eq!(layout.build_dir(), ctx.main_build_dir.as_deref());
    assert_ne!(layout.target(), super::verify_plan(&repo).unwrap().target());
}

fn witnessed_fixture_wait(
    real: &super::Real,
    _candidate: &super::Candidate,
    _prover: &super::Prover,
    handoff: &super::TreeIndexHandoff,
    _stop: &super::BranchStop,
) -> Result<(), super::Refusal> {
    assert!(
        handoff.expected(),
        "the production build path kept the tree promise"
    );
    if let Some(pool) = &real.pool {
        let tree = pool.jobs.pool(true);
        let mut held = Vec::new();
        while let Some(token) = tree.try_acquire().unwrap() {
            held.push(token);
        }
        assert_eq!(
            held.len(),
            4,
            "tree must be able to produce the fixture index before build ends"
        );
        assert!(pool.jobs.pool(false).try_acquire().unwrap().is_none());
    }
    std::fs::write(real.repo.join("fixture-waited"), b"waited").unwrap();
    Ok(())
}

fn witnessed_fixture_stage(
    real: &super::Real,
    _step: super::Step,
    _candidate: &super::Candidate,
    _reserved: &super::Reservation,
    _prover: &super::Prover,
    _log: &std::path::Path,
    stop: Option<&super::BranchStop>,
) -> Result<String, super::Refusal> {
    assert!(real.repo.join("fixture-waited").is_file());
    assert!(
        stop.is_some(),
        "the production build path attaches its stop"
    );
    if let Some(pool) = &real.pool {
        let _producer = pool
            .jobs
            .pool(false)
            .try_acquire()
            .unwrap()
            .expect("the fixture producer owns one separately accounted CPU token");
        assert!(pool.jobs.pool(false).try_acquire().unwrap().is_none());
        let mut tree = Vec::new();
        while let Some(token) = pool.jobs.pool(true).try_acquire().unwrap() {
            tree.push(token);
        }
        assert_eq!(
            tree.len(),
            3,
            "the live fixture loan stays out of tree's allocation"
        );
    }
    std::fs::write(real.repo.join("fixture-staged"), b"staged").unwrap();
    Ok("fixtures staged".into())
}

fn red_tree_verify(
    _real: &super::Real,
    _step: super::Step,
    _candidate: &super::Candidate,
    _prover: &super::Prover,
    _run: &crate::check_record::VerifyRun,
    _artifact: Option<&super::Artifact>,
    _stop: &super::BranchStop,
) -> Result<super::Ran, super::Refusal> {
    Err(super::Refusal::new("tree red", "fix it"))
}

/// The production orchestration, not only its helpers, waits before staging fixtures and
/// closes the tree handoff before returning a red. NEGATIVE CONTROLS: removing the wait call
/// makes the injected stage observe no marker; removing `with_tree_index_completion` leaves
/// the production handoff Pending.
#[test]
fn production_build_and_tree_wiring_owns_the_fixture_handoff() {
    use super::*;

    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let mut real = Real::new(&repo, Some(&repo.join("engine")), true, false).unwrap();
    real.await_fixture_index_op = witnessed_fixture_wait;
    real.stage_fixtures_run_op = witnessed_fixture_stage;
    let candidate = Candidate {
        commit: "11".repeat(20),
        version: "0.0.0".into(),
        why: "fixture".into(),
        bump_from: None,
        carry: Vec::new(),
    };
    let reservation = Reservation {
        build: 1,
        commit: "22".repeat(20),
        line: "1 0.0.0".into(),
    };
    let prover = Prover {
        bin: repo.join("prover/bin"),
        commit: "33".repeat(20),
        source: "fixture".into(),
    };
    let handoff = TreeIndexHandoff::default();
    handoff.expect();
    let built = real.build_branch(
        &candidate,
        &reservation,
        &prover,
        BuildWork {
            bundle: false,
            fixtures: true,
        },
        &handoff,
        &BranchStop::default(),
    );
    assert!(built.artifact.is_none());
    assert!(built.fixtures.is_some_and(|result| result.is_ok()));
    assert!(repo.join("fixture-staged").is_file());

    for serial in [false, true] {
        std::fs::remove_file(repo.join("fixture-waited")).unwrap();
        std::fs::remove_file(repo.join("fixture-staged")).unwrap();
        let ledger = aterm_capacity::Ledger::at(dir.path().join(if serial {
            "serial-ledger"
        } else {
            "parallel-ledger"
        }));
        let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 4));
        real.pool = Some(
            qualify_pool(
                &dir.path().join(if serial {
                    "serial.jobs"
                } else {
                    "parallel.jobs"
                }),
                &Branch::ALL,
                |need| {
                    aterm_capacity::admit_with(
                        &ledger,
                        &machine,
                        need,
                        aterm_capacity::Admit::Wait,
                        aterm_capacity::Pace::default(),
                        &mut |_| {},
                    )
                },
            )
            .unwrap(),
        );
        let pool = real.pool.as_ref().unwrap();
        if serial {
            pool.jobs
                .reserve_initial()
                .unwrap()
                .release_to_build()
                .unwrap();
        }
        pool.jobs.finish(2);
        real.build_run_op = |_, _, _, _, _, _| {
            Ok(Artifact {
                key: String::new(),
                app: PathBuf::new(),
                macho_sha256: String::new(),
                build: 1,
            })
        };
        let built = real.build_branch(
            &candidate,
            &reservation,
            &prover,
            BuildWork {
                bundle: true,
                fixtures: true,
            },
            &handoff,
            &BranchStop::default(),
        );
        assert!(built.artifact.is_some_and(|result| result.is_ok()));
        assert!(built.fixtures.is_some_and(|result| result.is_ok()));
        let pool = real.pool.as_ref().unwrap();
        assert!(pool.jobs.pool(false).try_acquire().unwrap().is_none());
        let mut tree = Vec::new();
        while let Some(token) = pool.jobs.pool(true).try_acquire().unwrap() {
            tree.push(token);
        }
        assert_eq!(
            tree.len(),
            4,
            "fixture completion returns its one-token loan"
        );
        drop(tree);
        real.pool = None;
    }

    real.verify_half_op = red_tree_verify;
    let branches = RealBranches {
        real: &real,
        admission_barrier: std::sync::Mutex::new(None),
        tree_index: TreeIndexHandoff::default(),
        shared_tree: std::sync::Mutex::new(None),
    };
    branches.tree_index.expect();
    let stop = BranchStop::default();
    let run = crate::check_record::VerifyRun {
        half: crate::check_record::Half::Tree,
        checks: vec!["test".into()],
        passed_binaries: Vec::new(),
        passed_lint_packages: Vec::new(),
        passed_appkit_groups: Vec::new(),
    };
    assert!(
        branches
            .verify(Step::Qualify, &candidate, &prover, &run, None, &stop)
            .is_err()
    );
    assert_eq!(
        branches.tree_index.completion(),
        TreeIndexCompletion::Stopped
    );
    assert!(!stop.fired(), "the graph has not classified the red yet");
}

/// Production obtains its artifact through the completion transition itself. NEGATIVE
/// CONTROLS: an absent build and a failed build do not release admission; deleting the
/// successful transition leaves `bundle_is_finished` false.
#[test]
fn a_successful_product_build_publishes_completion_before_returning_its_artifact() {
    use super::*;

    let completed = TreeIndexHandoff::default();
    let artifact = build_artifact_with_handoff(true, &completed, || Ok::<_, &str>(7));
    assert_eq!(artifact, Some(Ok(7)));
    assert!(completed.bundle_is_finished());

    let failed = TreeIndexHandoff::default();
    assert_eq!(
        build_artifact_with_handoff(true, &failed, || Err::<u8, _>("red")),
        Some(Err("red"))
    );
    assert!(!failed.bundle_is_finished());

    let absent = TreeIndexHandoff::default();
    assert_eq!(
        build_artifact_with_handoff(false, &absent, || Ok::<_, &str>(7)),
        None
    );
    assert!(!absent.bundle_is_finished());
}

/// A scheduled tree gets the opportunity to publish its exact executable index before the
/// fixture path may compile. NEGATIVE CONTROLS: the former one-shot miss is `Compile`, not the
/// `Ready` answer this handoff reaches; and a tree red published before the graph fires the
/// build stop is already `Stopped`, never a fallback compile. A non-red completed tree with no
/// index still falls back.
#[test]
fn fixture_index_waits_for_the_tree_compile_before_fallback() {
    use super::*;
    use std::cell::Cell;

    let handoff = TreeIndexHandoff::default();
    handoff.expect();
    let probes = Cell::new(0);
    let pauses = Cell::new(0);
    let answer = await_tree_index(
        &handoff,
        || {
            let probe = probes.get() + 1;
            probes.set(probe);
            probe == 2
        },
        || false,
        || pauses.set(pauses.get() + 1),
    );
    assert_eq!(answer, TreeIndexAnswer::Ready);
    assert_eq!((probes.get(), pauses.get()), (2, 1));
    let one_shot_miss = TreeIndexAnswer::Compile;
    assert_ne!(
        answer, one_shot_miss,
        "the former one-shot lookup compiles instead of observing the tree's handoff"
    );

    let missing = TreeIndexHandoff::default();
    missing.expect();
    let pauses = Cell::new(0);
    let answer = await_tree_index(
        &missing,
        || false,
        || false,
        || {
            pauses.set(pauses.get() + 1);
            missing.finish(TreeIndexCompletion::Compile);
        },
    );
    assert_eq!(answer, TreeIndexAnswer::Compile);
    assert_eq!(pauses.get(), 1);

    let red = TreeIndexHandoff::default();
    red.expect();
    let graph_stop = BranchStop::default();
    let refusal = with_tree_index_completion(&red, &graph_stop, || {
        Err(Refusal::new("tree tests failed", "fix the tests"))
    })
    .unwrap_err();
    assert_eq!(refusal.why, "tree tests failed");
    assert!(
        !graph_stop.fired(),
        "the graph has not classified the note yet"
    );
    assert_eq!(
        await_tree_index(&red, || false, || graph_stop.fired(), || unreachable!()),
        TreeIndexAnswer::Stopped,
        "the tree outcome closes the pre-stop race instead of authorizing fallback"
    );

    let stopped = TreeIndexHandoff::default();
    stopped.expect();
    assert_eq!(
        await_tree_index(&stopped, || true, || true, || unreachable!()),
        TreeIndexAnswer::Stopped,
        "a sibling red starts no new fixture work even when an index exists"
    );
}

/// NEGATIVE CONTROLS: ignoring the shape verdict starts a writer; returning
/// immediately on the site's refusal leaves the independent tap unrun.
#[test]
fn public_shape_precedes_writers_and_each_writer_joins_on_refusal() {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = dir.path().join("after-cut.log");
    let calls = AtomicUsize::new(0);
    let site = || {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(Refusal::new("site refused", "retry"))
    };
    let tap = || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok("tap aligned".into())
    };
    let refused = distribute_after_shape(
        &log,
        Err(Refusal::new("bad public shape", "repair")),
        site,
        tap,
    )
    .unwrap_err();
    assert_eq!(refused.why, "bad public shape");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let refused = distribute_after_shape(&log, Ok(()), site, tap).unwrap_err();
    assert_eq!(refused.why, "site refused");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// Credential-free scheduling measurement. The engine child is a native
/// no-op at the existing tap seam; reads use agh-fake's loopback server and
/// real curl with an injected 40 ms service cost. This is not a real cut.
#[cfg(unix)]
#[test]
#[ignore = "explicit loopback cut/after-cut gate scheduling measurement"]
fn independent_cut_gate_fixture_walls() {
    use super::*;
    use aterm_update_core::github_rest::{Direct, Origins, Spawn};
    use std::time::Instant;
    struct Service;
    impl Spawn for Service {
        fn run(
            &self,
            cmd: &mut Command,
            stdin: Option<&[u8]>,
        ) -> std::io::Result<std::process::Output> {
            std::thread::sleep(std::time::Duration::from_millis(40));
            Direct.run(cmd, stdin)
        }
    }
    let fake = agh_fake::FakeGithub::start();
    let token = agh_fake::token("cut-gate-fixture");
    fake.add_token(&token, "gate-fixture-reader");
    fake.add_repo("gate-fixture/aterm", agh_fake::RepoSpec::public());
    let ports = fake.ports();
    let _route = crate::github::Fixture {
        origins: Origins::loopback(ports.api, ports.uploads, ports.web, ports.cdn),
        token: Some(token),
        spawn: Some(std::sync::Arc::new(Service)),
    }
    .enter();
    let dir = aterm_tempfile::tempdir().unwrap();
    let engine = dir.path().join("engine");
    std::fs::create_dir_all(engine.join("bin")).unwrap();
    std::os::unix::fs::symlink("/usr/bin/true", engine.join("bin/pub")).unwrap();
    let mut real = Real::new(dir.path(), Some(&engine), true, false).unwrap();
    real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
    let read = || {
        crate::github::get("repos/gate-fixture/aterm/releases?per_page=30").unwrap();
    };
    let tap = || {
        real.align_tap("v0.184.0").unwrap();
    };
    read();
    tap();
    for (take, parallel) in [false, true, true, false].into_iter().enumerate() {
        let started = Instant::now();
        if parallel {
            let commit = || {
                crate::cut_timing::independent(
                    ("bundle", read),
                    ("handoff", || {
                        crate::cut_timing::independent(("fixtures", read), ("policy", read));
                    }),
                )
            };
            let authority = || {
                crate::cut_timing::independent(
                    ("availability", || {
                        crate::cut_timing::independent(("lock", read), ("target", read));
                    }),
                    ("floors", || {
                        crate::cut_timing::independent(
                            ("head and signature", || {
                                crate::cut_timing::independent(("head", read), ("signature", read));
                            }),
                            ("roster and ledger", || {
                                crate::cut_timing::independent(("roster", read), ("ledger", read));
                            }),
                        );
                    }),
                )
            };
            crate::cut_timing::independent(("commit", commit), ("authority", authority));
            crate::cut_timing::independent(("signed bytes", read), ("published floor", read));
        } else {
            for _ in 0..11 {
                read();
            }
        }
        println!(
            "gate-fixture step=cut take={take} parallel={parallel} wall_s={:.6}",
            started.elapsed().as_secs_f64()
        );
        let started = Instant::now();
        let distribution = || {
            // Public shape precedes both distribution writes.
            if parallel {
                crate::cut_timing::independent(
                    ("release JSON", read),
                    ("public readers", || {
                        crate::cut_timing::independent(("pointer", read), ("election", read));
                    }),
                );
            } else {
                for _ in 0..3 {
                    read();
                }
            }
            if parallel {
                crate::cut_timing::independent(("site", read), ("tap", tap));
            } else {
                read();
                tap();
            }
        };
        let reader = || {
            read();
        };
        if parallel {
            crate::cut_timing::independent(("distribution", distribution), ("reader", reader));
        } else {
            distribution();
            reader();
        }
        println!(
            "gate-fixture step=after-cut take={take} parallel={parallel} wall_s={:.6}",
            started.elapsed().as_secs_f64()
        );
    }
}

#[test]
fn an_interrupted_cutter_checkout_is_reconstructed_at_the_same_path() {
    use super::*;
    let temp = aterm_tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("input"), "committed input").unwrap();
    let git = GitCli::new(&repo);
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        crate::ledger::git_ok(&git, &args).unwrap();
    }
    let commit = crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned();
    let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
    real.state = temp.path().join("state");
    let cache = temp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let compiler = real.cutter_workspace_at(&cache, &commit).unwrap();
    let source = compiler.repo.clone();
    drop(compiler);
    std::fs::write(source.join(".git/index.lock"), "interrupted checkout").unwrap();
    std::fs::write(source.join("input"), "partial checkout").unwrap();
    std::fs::remove_file(cache.join("source-ready")).unwrap();
    let compiler = real.cutter_workspace_at(&cache, &commit).unwrap();
    assert_eq!(compiler.repo, source);
    assert!(!source.join(".git/index.lock").exists());
    assert_eq!(
        std::fs::read_to_string(source.join("input")).unwrap(),
        "committed input"
    );
    crate::release_tools::clean_at(&source, &commit).unwrap();
}

#[cfg(unix)]
#[test]
fn automatic_prefreeze_follows_between_cuts_and_yields_to_stages_and_windows() {
    use super::*;
    let row = |pid, need| aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::default(),
        pid,
        start_ms: 1,
        seq: 1,
        checkout: String::new(),
        need,
        timing: Default::default(),
        held: None,
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    };
    let line = row(
        1,
        aterm_capacity::Need::new("release line").rank(aterm_capacity::Rank::Release),
    );
    let marker = line.cover_marker();
    let worker = row(2, prefreeze_need().rank(aterm_capacity::Rank::Release));
    assert_eq!(
        prefreeze_traffic(&[line.clone(), worker.clone()], &marker, Some(2)),
        PrefreezeTraffic::Free
    );
    let mut stage = row(
        1,
        aterm_capacity::Need::new("stage")
            .rank(aterm_capacity::Rank::Release)
            .mem_gib(1),
    );
    stage.seq = 2;
    assert_eq!(
        prefreeze_traffic(&[line.clone(), worker.clone(), stage], &marker, Some(2)),
        PrefreezeTraffic::Yield
    );
    let window = row(3, aterm_capacity::Need::new("window").quiet(f64::MAX));
    assert_eq!(
        prefreeze_traffic(&[line.clone(), worker.clone(), window], &marker, Some(2)),
        PrefreezeTraffic::Yield
    );
    let next = row(
        3,
        aterm_capacity::Need::new("release line").rank(aterm_capacity::Rank::Release),
    );
    assert_eq!(
        prefreeze_traffic(&[line, worker.clone(), next.clone()], &marker, Some(2)),
        PrefreezeTraffic::End
    );
    assert_eq!(
        prefreeze_traffic(&[next], &marker, None),
        PrefreezeTraffic::End,
        "a new cut must cancel preparation after the old cut exited"
    );
    assert_eq!(
        prefreeze_traffic(&[worker], &marker, Some(2)),
        PrefreezeTraffic::Free,
        "the old cut ending must not stop following main"
    );
    assert_eq!(
        prefreeze_traffic(&[], &marker, None),
        PrefreezeTraffic::Free
    );
    let now = std::time::Instant::now();
    let next = now + PREFREEZE_REFRESH;
    assert!(!prefreeze_due(now, next));
    assert!(prefreeze_due(next, next));
}

/// The fake clock advances one observation interval per wait, and `spawn` is the boundary
/// that creates a release run directory. NEGATIVE CONTROL: the former deadline-only loop
/// ignored both `prepared` and the observed target, so it calls `spawn` after the first
/// unchanged observation; this test's `start.tick >= QUIET_TICKS` assertion fails against
/// that policy.
#[cfg(unix)]
#[test]
fn prefreeze_supervisor_starts_once_per_main_and_states_a_yield() {
    use super::*;
    use std::time::Instant;

    const QUIET_TICKS: u32 = 4;
    const BASE: &str = "1111111111111111111111111111111111111111";
    const MOVED: &str = "2222222222222222222222222222222222222222";

    #[derive(Clone)]
    struct FakeWorker {
        pid: u32,
    }

    struct FakeObservation {
        target: String,
    }

    struct FakeSupervisor {
        now: Instant,
        waits: u32,
        stop_at: u32,
        line: aterm_capacity::Record,
        window: aterm_capacity::Record,
        yield_at: Option<u32>,
        exit: Option<PrefreezeExit>,
        observation_ready: bool,
        observation_settles: u32,
        starts: Vec<(u32, String)>,
        run_dirs: u32,
        settles: u32,
        events: Vec<PrefreezeEvent>,
    }

    impl PrefreezeSupervisorRuntime for FakeSupervisor {
        type Worker = FakeWorker;
        type Observation = FakeObservation;

        fn now(&mut self) -> Instant {
            self.now
        }

        fn interrupted(&mut self) -> bool {
            self.waits >= self.stop_at
        }

        fn rows(&mut self) -> Result<Vec<aterm_capacity::Record>, String> {
            let mut rows = vec![self.line.clone()];
            if self.yield_at == Some(self.waits) {
                rows.push(self.window.clone());
            }
            Ok(rows)
        }

        fn start_observation(&mut self) -> Result<Self::Observation, String> {
            Ok(FakeObservation {
                target: if self.waits < QUIET_TICKS {
                    BASE
                } else {
                    MOVED
                }
                .to_owned(),
            })
        }

        fn poll_observation(
            &mut self,
            observation: &mut Self::Observation,
        ) -> Result<Option<String>, String> {
            Ok(self.observation_ready.then(|| observation.target.clone()))
        }

        fn settle_observation(&mut self, _observation: &mut Self::Observation) {
            self.observation_settles += 1;
        }

        fn prepared(&mut self, target: &str) -> bool {
            target == BASE
        }

        fn spawn(&mut self, target: &str, _covered: bool) -> Result<Self::Worker, String> {
            self.run_dirs += 1;
            self.starts.push((self.waits, target.to_owned()));
            Ok(FakeWorker { pid: 40 })
        }

        fn worker_pid(&self, worker: &Self::Worker) -> u32 {
            worker.pid
        }

        fn poll(&mut self, _worker: &mut Self::Worker) -> Result<Option<PrefreezeExit>, String> {
            Ok(self.exit.clone())
        }

        fn settle(&mut self, _worker: &mut Self::Worker) {
            self.settles += 1;
        }

        fn event(&mut self, event: PrefreezeEvent) {
            self.events.push(event);
        }

        fn wait(&mut self) {
            self.waits += 1;
            self.now += PREFREEZE_REFRESH;
        }
    }

    let row = |pid, need| aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::default(),
        pid,
        start_ms: 1,
        seq: 1,
        checkout: String::new(),
        need,
        timing: Default::default(),
        held: None,
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    };
    let line = row(
        1,
        aterm_capacity::Need::new("release line").rank(aterm_capacity::Rank::Release),
    );
    let marker = line.cover_marker();
    let window = row(3, aterm_capacity::Need::new("window").quiet(f64::MAX));
    let fake = |yield_at, exit| FakeSupervisor {
        now: Instant::now(),
        waits: 0,
        stop_at: QUIET_TICKS + 5,
        line: line.clone(),
        window: window.clone(),
        yield_at,
        exit,
        observation_ready: true,
        observation_settles: 0,
        starts: Vec::new(),
        run_dirs: 0,
        settles: 0,
        events: Vec::new(),
    };

    let success = PrefreezeExit {
        success: true,
        exit_code: Some(0),
        signal: None,
        status: "exit status: 0".into(),
    };
    let mut completed = fake(None, Some(success));
    supervise_prefreeze_loop(&marker, &mut completed).unwrap();
    assert_eq!(
        completed.run_dirs, 1,
        "one main move makes one run directory"
    );
    assert_eq!(completed.starts, [(QUIET_TICKS + 1, MOVED.to_owned())]);
    assert!(
        completed
            .starts
            .iter()
            .all(|(tick, _)| *tick >= QUIET_TICKS),
        "N unchanged quiet ticks make no run directories"
    );
    assert!(completed.events.iter().any(|event| matches!(
        event,
        PrefreezeEvent::Finished {
            target,
            success: true,
            exit_code: Some(0),
            signal: None,
            status,
        } if target == MOVED && status == "exit status: 0"
    )));

    let mut yielded = fake(Some(QUIET_TICKS + 2), None);
    supervise_prefreeze_loop(&marker, &mut yielded).unwrap();
    assert_eq!(yielded.run_dirs, 1, "the yielded head is not restarted");
    assert_eq!(yielded.settles, 1);
    assert!(yielded.events.iter().any(|event| matches!(
        event,
        PrefreezeEvent::YieldRequested { target, reason, .. }
            if target == MOVED && reason == "Release stage or timing claim"
    )));
    assert!(yielded.events.iter().any(|event| matches!(
        event,
        PrefreezeEvent::Yielded { target, reason, .. }
            if target == MOVED && reason == "Release stage or timing claim"
    )));

    let killed = PrefreezeExit {
        success: false,
        exit_code: None,
        signal: Some(9),
        status: "signal: 9 (SIGKILL)".into(),
    };
    let mut simultaneous = fake(Some(QUIET_TICKS + 2), Some(killed));
    supervise_prefreeze_loop(&marker, &mut simultaneous).unwrap();
    assert_eq!(simultaneous.settles, 0);
    assert!(!simultaneous.events.iter().any(|event| matches!(
        event,
        PrefreezeEvent::YieldRequested { .. } | PrefreezeEvent::Yielded { .. }
    )));
    assert!(simultaneous.events.iter().any(|event| matches!(
        event,
        PrefreezeEvent::Finished {
            target,
            success: false,
            exit_code: None,
            signal: Some(9),
            ..
        } if target == MOVED
    )));

    let mut observing = fake(Some(1), None);
    observing.observation_ready = false;
    observing.stop_at = 2;
    supervise_prefreeze_loop(&marker, &mut observing).unwrap();
    assert_eq!(observing.run_dirs, 0);
    assert_eq!(observing.observation_settles, 1);
}

#[cfg(target_os = "macos")]
#[test]
fn the_next_cut_promotes_the_exact_prefrozen_cutter_and_says_so() {
    use super::*;

    const PREPARED_SEAL: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const CHANGED_SEAL: &str = "4444444444444444444444444444444444444444444444444444444444444444";
    static OFFERED: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
    fn arm_prefrozen_fixture(_: &Path, _: Option<&Path>, handed: Option<&Path>) {
        *OFFERED.lock().unwrap() = handed.map(Path::to_path_buf);
    }
    fn prepared_identity() -> Result<String, String> {
        if OFFERED.lock().unwrap().is_some() {
            Ok(PREPARED_SEAL.into())
        } else {
            Ok(CHANGED_SEAL.into())
        }
    }
    fn changed_identity() -> Result<String, String> {
        Ok(CHANGED_SEAL.into())
    }
    *OFFERED.lock().unwrap() = None;

    let temp = aterm_tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("input"), "fixed release input\n").unwrap();
    let git = GitCli::new(&repo);
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        crate::ledger::git_ok(&git, &args).unwrap();
    }
    let built_from = crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned();
    std::fs::write(repo.join("input"), "next fixed release input\n").unwrap();
    for args in [
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "target",
        ],
    ] {
        crate::ledger::git_ok(&git, &args).unwrap();
    }
    let commit = crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned();
    let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
    real.release = temp.path().join("release");
    real.state = temp.path().join("state");
    real.logs = temp.path().join("logs");
    real.build_inputs = prepared_identity;
    real.arm_prefrozen_inputs = arm_prefrozen_fixture;
    real.handed_inputs = || {
        Some(PathBuf::from(
            "/state/aterm-release/inputs-seal/current.toml",
        ))
    };
    real.prefrozen_inputs = |state, expected| {
        if expected != PREPARED_SEAL {
            return None;
        }
        let path = state.join("inputs-seal/prepared.toml");
        std::fs::create_dir_all(path.parent().unwrap()).ok()?;
        std::fs::write(&path, "fixture seal").ok()?;
        Some(path)
    };
    std::fs::create_dir_all(&real.logs).unwrap();

    let source = temp.path().join("prepared-cutter");
    aterm_tempfile::stand_in(
        &source,
        &format!(
            "#!/bin/sh\n[ \"$#\" -eq 1 ] && [ \"$1\" = \"--version\" ] || exit 64\nprintf '%s\\n' '{}'\n",
            crate::release_tools::CUTTER_VERSION
        ),
    )
    .unwrap();
    let source = std::fs::canonicalize(source).unwrap();
    let cache = cutter_cache_dir(&real.release);
    let inputs = crate::cutter_inputs::Inputs {
        digest: "1".repeat(64),
        sources: "2".repeat(64),
        seal: PREPARED_SEAL.into(),
    };
    let mut prepared = crate::release_tools::stage_cutter(
        &cache,
        &commit,
        &source,
        &real.step_log(real.step),
        Some(inputs.clone()),
    )
    .unwrap();
    prepared.built_from.clone_from(&built_from);
    crate::release_tools::publish_cutter(&cache, &prepared).unwrap();
    real.seal_inputs().unwrap();
    assert!(
        OFFERED
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|path| path.ends_with("cutter-cache/state/inputs-seal/prepared.toml"))
    );

    // This path cannot build anything. Success therefore proves that the real cut promoted
    // the exact cache record instead of entering cutter_workspace_at/frozen_cutter_here.
    let unusable = Prover {
        bin: temp.path().join("no-prover"),
        commit: "f".repeat(40),
        source: "negative-control prover".into(),
    };
    let (adopted, handed) = real.frozen_cutter_with_inputs(&commit, &unusable).unwrap();
    assert_eq!(adopted.commit, commit);
    assert_eq!(adopted.built_from, built_from);
    assert_eq!(adopted.inputs, Some(inputs));
    assert_ne!(adopted.tool.path, prepared.tool.path);
    let handed = handed.expect("the prepared step-1 seal is handed to the real cutter");
    assert_eq!(
        handed,
        PathBuf::from("/state/aterm-release/inputs-seal/current.toml")
    );
    let command = real.cutter_command_with_inputs(&adopted.tool.path, Some(handed.clone()));
    assert!(command.get_envs().any(|(name, value)| {
        name == std::ffi::OsStr::new(crate::build_inputs::SEAL_ENV)
            && value == Some(handed.as_os_str())
    }));
    assert_eq!(
        crate::release_tools::recorded_cutter(&real.release, &commit).unwrap(),
        adopted
    );
    let log = std::fs::read_to_string(real.step_log(real.step)).unwrap();
    assert!(
        log.contains("\"event\":\"cutter-prefreeze-adopted\"")
            && log.contains("\"step_1_inputs\":\"adopted-and-handed\"")
            && log.contains("\"event\":\"prefreeze-step-1-inputs-adopted\""),
        "the next cut must state exact preparation adoption: {log}"
    );

    std::fs::remove_file(crate::release_tools::cutter_record_path(&real.release)).unwrap();
    real.build_inputs = changed_identity;
    real.seal_inputs().unwrap();
    assert!(
        real.frozen_cutter_with_inputs(&commit, &unusable).is_err(),
        "changed step-1 inputs must reject the exact cached cutter and enter a real freeze"
    );
}

#[cfg(unix)]
#[test]
fn prefreeze_handoff_waits_ninety_seconds_with_progress_but_refuses_a_stall() {
    use super::*;
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    for progressing in [true, false] {
        let seconds = Cell::new(0_u64);
        let began = Instant::now();
        let mut logs = Vec::new();
        let result = wait_prefreeze(
            || {
                let elapsed = seconds.get();
                (!progressing || elapsed < 90).then(|| {
                    (
                        4321,
                        "same birth".into(),
                        Some(if progressing { elapsed / 30 } else { 0 }),
                    )
                })
            },
            || began + Duration::from_secs(seconds.get()),
            || seconds.set(seconds.get() + 1),
            || false,
            |message| logs.push(message.to_owned()),
        );
        if progressing {
            result.unwrap();
            assert_eq!(seconds.get(), 90);
            assert!(logs[0].contains("pid 4321"));
            assert!(logs[1].contains("yielded after 90.000 s"));
        } else {
            assert_eq!(seconds.get(), 60);
            assert!(
                result
                    .unwrap_err()
                    .contains("pid 4321 made no descendant CPU/I/O progress")
            );
            assert_eq!(logs.len(), 1);
        }
    }
}

#[cfg(unix)]
#[test]
fn prefreeze_compiler_stays_in_the_workers_cancellation_group() {
    use super::*;
    for prefreeze in [true, false] {
        let mut cmd = command("/bin/cat");
        cmd.stdin(Stdio::piped()).stdout(Stdio::null());
        step_process_group(&mut cmd, prefreeze);
        let mut child = cmd.spawn().unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        // SAFETY: these calls only read process-group identities of this process
        // and its owned, unreaped child.
        let (group, parent) = unsafe { (libc::getpgid(pid), libc::getpgrp()) };
        assert_eq!(group, if prefreeze { parent } else { pid });
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
    }
}

/// Real worker and supervisor, private traffic ledger: no cut or publication.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit real prefreeze cancellation measurement"]
fn measure_prefreeze_yield() {
    use super::*;
    use std::time::{Duration, Instant};
    let cwd = std::env::current_dir().unwrap();
    let source = PathBuf::from(
        crate::ledger::git_ok(&GitCli::new(&cwd), &["rev-parse", "--show-toplevel"])
            .unwrap()
            .stdout_utf8()
            .trim(),
    );
    let original = GitCli::new(&source);
    let temp = aterm_tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    crate::ledger::git_ok(
        &original,
        &[
            "clone",
            "--quiet",
            "--shared",
            source.to_str().unwrap(),
            repo.to_str().unwrap(),
        ],
    )
    .unwrap();
    let repo = repo.canonicalize().unwrap();
    let git = GitCli::new(&repo);
    let url = crate::ledger::git_ok(&original, &["remote", "get-url", "origin"])
        .unwrap()
        .stdout_utf8();
    crate::ledger::git_ok(&git, &["remote", "set-url", "origin", url.trim()]).unwrap();
    crate::ledger::git_ok(
        &git,
        &[
            "config",
            "submodule.vendor/astream.url",
            source.join("vendor/astream").to_str().unwrap(),
        ],
    )
    .unwrap();
    crate::ledger::git_ok(
        &git,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "vendor/astream",
        ],
    )
    .unwrap();
    let ledger = aterm_capacity::Ledger::at(temp.path().join("traffic"));
    let executable = aterm_tempfile::bin_exe("aterm-release");
    std::thread::scope(|scope| {
        let supervisor =
            scope.spawn(|| supervise_prefreeze_in(&repo, "old-line", &ledger, &executable));
        let mut last = Instant::now();
        let mut probe = aterm_uds::deadline::ProcessProgressProbe::default();
        let mut evidence = None;
        let compiler = loop {
            let sweep = aterm_sysprobe::processes().unwrap();
            let mut descendants = std::collections::BTreeSet::from([std::process::id()]);
            loop {
                let before = descendants.len();
                for process in &sweep.procs {
                    if descendants.contains(&process.ppid) {
                        descendants.insert(process.pid);
                    }
                }
                if descendants.len() == before {
                    break;
                }
            }
            if let Some(compiler) = sweep.procs.iter().find(|process| {
                descendants.contains(&process.pid)
                    && process.name == "trustc"
                    && aterm_sysprobe::kproc::argv(process.pid).is_some_and(|args| {
                        args.iter().any(|arg| arg.starts_with(b"--emit="))
                            && !args.iter().any(|arg| arg.starts_with(b"--print"))
                    })
            }) {
                let args = aterm_sysprobe::kproc::argv(compiler.pid).unwrap();
                let unit = args
                    .windows(2)
                    .find(|pair| pair[0] == b"--crate-name")
                    .unwrap();
                println!(
                    "prefreeze-active-compiler unit={}",
                    String::from_utf8_lossy(&unit[1])
                );
                break Some(compiler.pid);
            }
            let progress = probe.read_descendants(std::process::id());
            if progress.is_none() || progress != evidence {
                last = Instant::now();
            }
            evidence = progress;
            if supervisor.is_finished() || last.elapsed() >= Duration::from_secs(60) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let claimed = Instant::now();
        let _line = aterm_capacity::admit_with(
            &ledger,
            &aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 4)),
            aterm_capacity::Need::new("release line").rank(aterm_capacity::Rank::Release),
            aterm_capacity::Admit::Wait,
            aterm_capacity::Pace::default(),
            &mut |_| {},
        )
        .unwrap();
        supervisor.join().unwrap().unwrap();
        let compiler =
            compiler.expect("no compiler appeared before preparation stopped progressing");
        println!(
            "prefreeze-yield-measure compiler_pid={compiler} claim_to_holder_removed_s={:.6}",
            claimed.elapsed().as_secs_f64()
        );
        assert!(
            !repo
                .join(".git/aterm-release/.ubd/cutter-cache/prefreeze-running")
                .exists()
        );
        let remaining = aterm_sysprobe::kproc::record_read(compiler).unwrap();
        println!("prefreeze-yield-compiler-terminal {remaining:?}");
        assert!(
            remaining.is_none_or(|record| record.zombie),
            "a live compiler retained its build lock after yield"
        );
    });
}

/// Small real Cargo graph, no release, credential, service or product execution.
/// The instrument counts compiled units, rather than interpreting elapsed time as a gain.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit local pre-freeze timeline measurement"]
fn measure_automatic_prefreeze_timeline() {
    use super::*;

    const MEASURED_SEAL: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    let temp = aterm_tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(repo.join("crates/aterm-release/src")).unwrap();
    std::fs::create_dir_all(repo.join("dep/src")).unwrap();
    std::fs::create_dir_all(repo.join(".cargo")).unwrap();
    std::fs::write(repo.join(".gitignore"), "/target/\n").unwrap();
    std::fs::write(
        repo.join(".cargo/config.toml"),
        "[unstable]\nchecksum-freshness=true\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.join("app/src")).unwrap();
    std::fs::write(repo.join("app/src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        repo.join("app/Cargo.toml"),
        "[package]\nname='app'\nversion.workspace=true\nedition='2021'\n",
    )
    .unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers=['dep','app','crates/aterm-release']\nresolver='2'\n[workspace.package]\nversion='1.0.0'\n[profile.gate]\ninherits='dev'\n").unwrap();
    std::fs::write(repo.join("crates/aterm-release/Cargo.toml"), "[package]\nname='aterm-release'\nversion='0.0.0'\nedition='2021'\n[dependencies]\ncutter-dep={path='../../dep'}\n").unwrap();
    std::fs::write(
        repo.join("dep/Cargo.toml"),
        "[package]\nname='cutter-dep'\nversion='0.0.0'\nedition='2021'\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("dep/src/lib.rs"),
        "pub fn marker() -> u32 { 1 }\n",
    )
    .unwrap();
    let program =
        "fn main() { let _ = cutter_dep::marker(); println!(\"aterm-release tool 1\"); }\n";
    std::fs::write(repo.join("crates/aterm-release/src/main.rs"), program).unwrap();
    std::fs::write(repo.join("Cargo.lock"), "version=4\n[[package]]\nname='app'\nversion='1.0.0'\n[[package]]\nname='aterm-release'\nversion='0.0.0'\ndependencies=['cutter-dep']\n[[package]]\nname='cutter-dep'\nversion='0.0.0'\n").unwrap();
    let git = GitCli::new(&repo);
    let git_run = |args: &[&str]| crate::ledger::git_ok(&git, args).unwrap();
    git_run(&["init", "-q"]);
    let commit = || {
        git_run(&["add", "."]);
        git_run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ]);
        git_run(&["rev-parse", "HEAD"])
            .stdout_utf8()
            .trim()
            .to_owned()
    };
    let base = commit();
    std::fs::write(
        repo.join("crates/aterm-release/src/main.rs"),
        format!("{program}// main moves during cut N\n"),
    )
    .unwrap();
    std::fs::write(repo.join("notes"), "unrelated main movement\n").unwrap();
    let next = commit();
    let bin = crate::gates::trust_stage2_bin().unwrap();
    let prover = Prover {
        bin,
        commit: "0".repeat(40),
        source: "fixture pinned Trust".into(),
    };
    for (take, prepared) in [false, true, true, false].into_iter().enumerate() {
        git_run(&["checkout", "--quiet", "--detach", &base]);
        let state = temp.path().join(format!("take-{take}-{prepared}"));
        let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
        real.scratch_logs = false;
        real.release = state.join("release");
        real.state = state.join("state");
        real.logs = state.join("logs");
        std::fs::create_dir_all(&real.logs).unwrap();
        real.build_inputs = || Ok(MEASURED_SEAL.into());
        real.arm_inputs = |_, _| {};
        real.arm_prefrozen_inputs = |_, _, _| {};
        real.handed_inputs = || Some(PathBuf::from("/measurement/inputs-seal.toml"));
        real.prefrozen_inputs = |state, expected| {
            if expected != MEASURED_SEAL {
                return None;
            }
            let path = state.join("inputs-seal/measurement.toml");
            std::fs::create_dir_all(path.parent().unwrap()).ok()?;
            std::fs::write(&path, "measurement seal").ok()?;
            Some(path)
        };
        let n = real.frozen_cutter(&base, &prover).unwrap();
        git_run(&["checkout", "--quiet", "--detach", &next]);
        if prepared {
            let compiler = real
                .cutter_workspace_at(&cutter_cache_dir(&real.release), &next)
                .unwrap();
            compiler.frozen_cutter_here(&next, &prover).unwrap();
            assert_eq!(
                crate::release_tools::recorded_cutter(&real.release, &base).unwrap(),
                n,
                "preparation must not overwrite the current cut's proof"
            );
        }
        real.seal_inputs().unwrap();
        let log = real.step_log(real.step);
        std::fs::write(&log, "").unwrap();
        let selection = std::time::Instant::now();
        let next_cutter = real.frozen_cutter(&next, &prover).unwrap();
        let selection_s = selection.elapsed().as_secs_f64();
        let compiled = |log: &Path| -> u64 {
            std::fs::read_to_string(log)
                .unwrap()
                .lines()
                .filter_map(|line| line.strip_prefix("release-event: "))
                .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
                .filter(|event| event["event"].as_str() == Some("frozen-tool-compile"))
                .map(|event| event["compiled_units"].as_u64().unwrap())
                .sum()
        };
        let first = compiled(&log);
        assert_eq!(first, if prepared { 0 } else { 1 });
        let exact = std::fs::read_to_string(&log)
            .unwrap()
            .contains("\"event\":\"cutter-prefreeze-adopted\"");
        assert_eq!(
            exact, prepared,
            "only the prepared take adopts the exact record"
        );
        let hop = |label: &str, expected: u64| {
            let text = std::fs::read_to_string(&log).unwrap();
            assert_eq!(compiled(&log), expected, "{label}");
            println!(
                "prefreeze-timeline take={take} prepared={prepared} hop={label} compiled={expected}"
            );
            for line in text
                .lines()
                .filter(|line| line.contains("fingerprint") || line.starts_with("release-event: "))
            {
                println!("{line}");
            }
        };
        hop("unchanged", first);
        let manifest = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
        let lock = std::fs::read_to_string(repo.join("Cargo.lock")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), manifest.replace("1.0.0", "2.0.0")).unwrap();
        std::fs::write(repo.join("Cargo.lock"), lock.replace("1.0.0", "2.0.0")).unwrap();
        let bump = commit();
        std::fs::write(&log, "").unwrap();
        let bumped = real.frozen_cutter(&bump, &prover).unwrap();
        assert_eq!(bumped.built_from, next_cutter.built_from);
        assert_eq!(bumped.inputs, next_cutter.inputs);
        hop("bump-only", 0);
        if prepared {
            assert_eq!(next_cutter.built_from, next);
            std::fs::write(
                repo.join("crates/aterm-release/src/main.rs"),
                format!("{program}// another cutter delta\n"),
            )
            .unwrap();
            let delta = commit();
            std::fs::write(&log, "").unwrap();
            let delta_cutter = real.frozen_cutter(&delta, &prover).unwrap();
            assert_eq!(compiled(&log), 1, "unchanged dependency stays fresh");
            assert_eq!(delta_cutter.built_from, delta);
            assert_ne!(delta_cutter.inputs, next_cutter.inputs);
            hop("real-cutter-change", 1);
            std::fs::write(
                repo.join("dep/src/lib.rs"),
                "pub fn marker() -> u32 { 2 }\n",
            )
            .unwrap();
            let manifest = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
            let lock = std::fs::read_to_string(repo.join("Cargo.lock")).unwrap();
            std::fs::write(repo.join("Cargo.toml"), manifest.replace("2.0.0", "3.0.0")).unwrap();
            std::fs::write(repo.join("Cargo.lock"), lock.replace("2.0.0", "3.0.0")).unwrap();
            let delta = commit();
            std::fs::write(&log, "").unwrap();
            let cutter = real.frozen_cutter(&delta, &prover).unwrap();
            assert_eq!(
                cutter.built_from, delta,
                "a bump carrying a real input cannot adopt another input's pre-freeze"
            );
            assert_ne!(cutter.inputs, bumped.inputs);
            hop("real-dependency-change", 2);
        }
        println!(
            "prefreeze-timeline take={take} prepared={prepared} commit={next} \
             cutter_selection_wall_s={selection_s:.6} next_freeze_compiled_units={first}"
        );
    }
}

/// Read-only native origin observations and local bump writes in a disposable
/// clone. Deliberately stops before the native ref write or any publication.
#[test]
#[ignore = "explicit local bump attribution; reads configured source origin"]
fn measure_local_bump_parts() {
    let source = std::env::current_dir().unwrap();
    let original = GitCli::new(&source);
    let url = crate::ledger::git_ok(&original, &["remote", "get-url", "origin"])
        .unwrap()
        .stdout_utf8();
    let temp = aterm_tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    crate::ledger::git_ok(
        &original,
        &[
            "clone",
            "--quiet",
            "--shared",
            source.to_str().unwrap(),
            repo.to_str().unwrap(),
        ],
    )
    .unwrap();
    let repo = repo.canonicalize().unwrap();
    let git = GitCli::new(&repo);
    crate::ledger::git_ok(&git, &["remote", "set-url", "origin", url.trim()]).unwrap();
    crate::ledger::git_ok(
        &git,
        &[
            "config",
            "submodule.vendor/astream.url",
            source.join("vendor/astream").to_str().unwrap(),
        ],
    )
    .unwrap();
    crate::ledger::git_ok(
        &git,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "vendor/astream",
        ],
    )
    .unwrap();
    let mut real = Real::new(&repo, Some(&source), true, false).unwrap();
    let prover = real.prover(None).unwrap();
    let from = real.version_at("HEAD").unwrap();
    let to = aterm_types::release_version::ReleaseVersion::parse(&from)
        .unwrap()
        .successor()
        .unwrap()
        .to_string();
    crate::cut_timing::measure("fixture-bump-origin", || {
        crate::publish::native_origin(&repo).unwrap()
    });
    let editor = PathBuf::from(std::env::var_os("ATERM_RELEASE_BUMP_EDITOR").unwrap());
    crate::cut_timing::measure("fixture-bump-editor", || {
        let mut cmd = real.child(editor, None);
        cmd.arg(&repo).args([&to, &from]);
        assert_eq!(real.logged(Step::Commit, &mut cmd).code, 0);
    });
    crate::cut_timing::measure("fixture-bump-lockfiles", || {
        let changed = bump_nested_locks(
            &repo,
            real.git_paths(&["ls-files", "-z", "--", "Cargo.lock", "*/Cargo.lock"]),
            &from,
            &to,
        )
        .unwrap();
        real.validate_bumped_locks(&prover, &changed).unwrap();
    });
    crate::cut_timing::measure("fixture-bump-harness", || {
        // A libtest image has no credential-helper dispatch. Use the real
        // diagnostic CLI, so its native origin uses its own typed helper.
        let cli = PathBuf::from(std::env::var_os("ATERM_RELEASE_DIAGNOSTIC").unwrap());
        let mut cmd = real.child(cli, None);
        cmd.arg("harness-bump");
        assert_eq!(real.logged(Step::Commit, &mut cmd).code, 0);
    });
    crate::cut_timing::measure("fixture-bump-commit", || {
        crate::ledger::git_ok(
            &git,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qam",
                "Fixture bump (never pushed)",
            ],
        )
        .unwrap();
    });
    // Keep this mutable so Real's destructor owns all its ordinary cleanup.
    real.scratch_logs = true;
}

/// Replay exact historical compiler graphs in one persistent private checkout.
/// The seal is fixed fixture evidence; this measures compiler reuse, not an SDK walk.
#[test]
#[ignore = "explicit full cutter rebuild attribution"]
fn measure_real_cutter_rebuilds() {
    let repo = std::env::current_dir().unwrap().canonicalize().unwrap();
    let state = PathBuf::from(std::env::var_os("ATERM_RELEASE_REBUILD_STATE").unwrap());
    assert!(state.is_absolute());
    let commits = std::env::var("ATERM_RELEASE_REBUILD_COMMITS").unwrap();
    let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
    real.state = state.join("state");
    real.release = state.join("release");
    real.logs = state.join("logs");
    real.build_inputs = || Ok("f".repeat(64));
    real.arm_inputs = |_, _| {};
    std::fs::create_dir_all(&real.logs).unwrap();
    let prover = real.prover(None).unwrap();
    for rev in commits.split(',') {
        let commit = crate::ledger::git_ok(&real.git(), &["rev-parse", rev])
            .unwrap()
            .stdout_utf8();
        let compiler = real
            .cutter_workspace_at(&cutter_cache_dir(&real.release), commit.trim())
            .unwrap();
        let log = compiler.step_log(compiler.step);
        std::fs::write(&log, "").unwrap();
        let cutter = compiler.frozen_cutter_here(commit.trim(), &prover).unwrap();
        std::fs::copy(&log, state.join(format!("{rev}.log"))).unwrap();
        println!("cutter-rebuild rev={rev} built_from={}", cutter.built_from);
        for line in std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("fingerprint") || line.starts_with("release-event: "))
        {
            println!("{line}");
        }
    }
}

#[cfg(unix)]
#[test]
fn prefreeze_reserves_background_build_and_timing_exclusion() {
    use super::*;
    let need = prefreeze_need();
    assert_eq!(need.rank, aterm_capacity::Rank::Background);
    assert!(need.mem > 0 && need.cores > 0 && need.covers);
    assert_eq!(need.quiet, Some(f64::MAX));
    // The ordinary cut path carries Release rank; the preparation must remove it
    // from every child, retain the admitted build cover and bound compiler jobs.
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
    let mut cmd = real.child("targo", None);
    assert!(
        cmd.get_envs()
            .any(|(key, val)| key == aterm_capacity::RELEASE_RUN_ENV && val.is_some())
    );
    configure_prefreeze_child(&mut cmd, "fixture-cover", 3);
    let env: std::collections::BTreeMap<_, _> = cmd.get_envs().collect();
    assert_eq!(
        env[std::ffi::OsStr::new(aterm_capacity::RELEASE_RUN_ENV)],
        None
    );
    assert_eq!(
        env[std::ffi::OsStr::new(aterm_capacity::COVER_ENV)],
        Some(std::ffi::OsStr::new("fixture-cover"))
    );
    assert_eq!(
        env[std::ffi::OsStr::new("CARGO_BUILD_JOBS")],
        Some(std::ffi::OsStr::new("3"))
    );
}

/// Real offline resolution is the oracle. Omitting the rewrite must fail locked
/// metadata, and an incorrect rewritten version must fail it too.
#[cfg(unix)]
#[test]
fn version_only_lock_rewrite_matches_resolver_bytes() {
    use super::*;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let repo = scratch.path().canonicalize().unwrap();
    for dir in ["leaf/src", "nested/src"] {
        std::fs::create_dir_all(repo.join(dir)).unwrap();
        std::fs::write(repo.join(dir).join("lib.rs"), "").unwrap();
    }
    let root = "[workspace]\nmembers = [\"leaf\"]\nexclude = [\"nested\"]\nresolver = \"2\"\n[workspace.package]\nversion = \"0.147.0\"\n";
    std::fs::write(repo.join("Cargo.toml"), root).unwrap();
    std::fs::write(
        repo.join("leaf/Cargo.toml"),
        "[package]\nname = \"bump-leaf\"\nversion.workspace = true\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("nested/Cargo.toml"), "[workspace]\n[package]\nname = \"consumer\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nbump-leaf = { path = \"../leaf\" }\n").unwrap();
    let targo = crate::gates::trust_stage2_bin().unwrap().join("targo");
    let run = |manifest: &Path, args: &[&str]| {
        let mut cmd = command(&targo);
        cmd.current_dir(&repo)
            .args(args)
            .arg("--manifest-path")
            .arg(manifest);
        cmd.output().unwrap()
    };
    let locks = ["Cargo.lock", "nested/Cargo.lock"];
    let mut old = Vec::new();
    for lock in locks {
        let manifest = repo.join(lock).with_file_name("Cargo.toml");
        assert!(
            run(&manifest, &["update", "--workspace", "--offline"])
                .status
                .success()
        );
        old.push(std::fs::read_to_string(repo.join(lock)).unwrap());
    }
    std::fs::write(repo.join("Cargo.toml"), root.replace("0.147.0", "0.148.0")).unwrap();
    let metadata = ["metadata", "--locked", "--offline", "--format-version", "1"];
    for lock in locks {
        assert!(
            !run(&repo.join(lock).with_file_name("Cargo.toml"), &metadata)
                .status
                .success(),
            "unmodified {lock} must be stale"
        );
    }
    assert_eq!(
        bump_nested_locks(
            &repo,
            Ok(locks.map(str::to_owned).to_vec()),
            "0.147.0",
            "0.148.0"
        )
        .unwrap(),
        locks
    );
    for (lock, original) in locks.iter().zip(&old) {
        let manifest = repo.join(lock).with_file_name("Cargo.toml");
        let expected = lock_follows_bump(original, "0.147.0", "0.148.0").unwrap();
        assert_eq!(std::fs::read_to_string(repo.join(lock)).unwrap(), expected);
        assert!(run(&manifest, &metadata).status.success());
        assert!(
            run(&manifest, &["update", "--workspace", "--offline"])
                .status
                .success()
        );
        assert_eq!(std::fs::read_to_string(repo.join(lock)).unwrap(), expected);
        std::fs::write(repo.join(lock), expected.replace("0.148.0", "0.149.0")).unwrap();
        assert!(
            !run(&manifest, &metadata).status.success(),
            "wrong version must be refused"
        );
    }
}
/// NEGATIVE CONTROLS: waiting at start blocks on sleep; dropping without stopping and
/// reaping leaves the PID alive. No release executable, signer or credential is used.
#[cfg(unix)]
#[test]
fn early_packaging_child_is_owned_until_join_or_cancel() {
    use super::*;
    use crate::release::PendingSign as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
    for cancel in [false, true] {
        let mut cmd = command("/bin/sleep");
        cmd.arg("60");
        let mut pending = real.start_preparing_sign(cmd).unwrap();
        let pid = pending.child.as_ref().unwrap().id();
        assert!(
            pending
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none()
        );
        if cancel {
            drop(pending);
        } else {
            pending.stop().fire();
            let ran = Box::new(pending).finish();
            assert_ne!(ran.code, 0);
            assert!(
                ran.detail
                    .iter()
                    .any(|line| line.contains("step-child-finished"))
            );
        }
        // SAFETY: signal zero only queries this test child's PID after it was reaped.
        assert_ne!(
            unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), 0) },
            0
        );
    }
}

#[cfg(unix)]
#[test]
fn release_signal_disposition_probe() {
    let Ok(expected) = std::env::var("ATERM_RELEASE_SIGNAL_PROBE") else {
        return;
    };
    let expected = if expected == "ignored" {
        libc::SIG_IGN
    } else {
        libc::SIG_DFL
    };
    for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM] {
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        // SAFETY: a null new action reads into a live output slot.
        assert_eq!(
            unsafe { libc::sigaction(sig, std::ptr::null(), action.as_mut_ptr()) },
            0
        );
        // SAFETY: the successful query initialized the action.
        assert_eq!(
            unsafe { action.assume_init() }.sa_sigaction,
            expected,
            "signal {sig}"
        );
    }
    println!("\nRELEASE_SIGNAL_CHILD_CHECKED");
}

/// Detached release steps default the signals the cutter forwards even when its shell
/// ignored them. The ordinary child is the negative control: it observes inherited ignores.
#[cfg(unix)]
#[test]
fn detached_steps_default_the_background_shells_ignored_signals() {
    use super::*;
    use std::time::Duration;
    const PARENT: &str = "ATERM_RELEASE_SIGNAL_PARENT";
    const NAME: &str =
        "release_steps::tests::detached_steps_default_the_background_shells_ignored_signals";
    const PROBE: &str = "release_steps::tests::release_signal_disposition_probe";
    if std::env::var_os(PARENT).is_none() {
        let mut parent = command("/bin/sh");
        parent
            .args(["-c", "trap '' INT QUIT TERM; exec \"$0\" \"$@\""])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(PARENT, "1");
        let out = aterm_uds::deadline::ends_within(&mut parent, Duration::from_secs(60));
        assert!(out.status.success(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("\nDETACHED_SIGNALS_CHECKED\n"),
            "{out:?}"
        );
        return;
    }
    for reset in [false, true] {
        let mut child = command(std::env::current_exe().unwrap());
        child
            .args(["--exact", PROBE, "--nocapture", "--test-threads=1"])
            .env(
                "ATERM_RELEASE_SIGNAL_PROBE",
                if reset { "default" } else { "ignored" },
            );
        if reset {
            detach(&mut child);
        }
        let out = aterm_uds::deadline::ends_within(&mut child, Duration::from_secs(60));
        assert!(out.status.success(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("\nRELEASE_SIGNAL_CHILD_CHECKED\n"),
            "reset={reset}: {out:?}"
        );
    }
    println!("\nDETACHED_SIGNALS_CHECKED");
}

#[test]
fn reader_cargo_holds_its_implicit_job_through_success_and_failure() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = aterm_capacity::Jobserver::new(&dir.path().join("reader.jobs"), 1).unwrap();
    for fail in [false, true] {
        let stop = super::BranchStop::default();
        let mut cmd = super::command("targo");
        let result = super::with_reader_cargo(Some(&pool), &stop, &mut cmd, |cmd| {
            assert!(
                pool.try_acquire().unwrap().is_none(),
                "implicit job is charged"
            );
            for name in ["MAKEFLAGS", "CARGO_MAKEFLAGS"] {
                assert!(cmd.get_envs().any(|(key, value)| {
                    key == name && value == Some(std::ffi::OsStr::new(&pool.flags()))
                }));
            }
            if fail {
                Err(std::io::Error::other("compile failed"))
            } else {
                Ok(())
            }
        });
        assert_eq!(result.is_err(), fail);
        assert!(
            pool.try_acquire().unwrap().is_some(),
            "compile returned its job"
        );
    }
    let stop = super::BranchStop::default();
    stop.fire();
    let token = pool.try_acquire().unwrap().unwrap();
    let mut ran = false;
    let result = super::with_reader_cargo(Some(&pool), &stop, &mut super::command("targo"), |_| {
        ran = true;
        Ok(())
    });
    assert!(result.is_err());
    assert!(!ran, "cancelled warm-up never starts Cargo");
    drop(token);
}

#[test]
fn reader_prebuild_overlaps_cut_and_preserves_its_verdict() {
    let (started, began) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let stopped = std::sync::Arc::new(std::sync::Mutex::new(None));
    let warm_stop = std::sync::Arc::clone(&stopped);
    let timeout = std::time::Duration::from_secs(60);
    let verdict = super::with_reader_prebuild(
        None,
        move |stop| {
            *warm_stop.lock().unwrap() = Some(stop.clone());
            started.send(()).unwrap();
            released.recv_timeout(timeout).unwrap();
        },
        || {
            began.recv_timeout(timeout).unwrap();
            release.send(()).unwrap();
            super::Ran {
                code: 37,
                detail: Vec::new(),
            }
        },
    );
    assert_eq!(verdict.code, 37);
    assert!(stopped.lock().unwrap().as_ref().unwrap().fired());
}

#[test]
fn a_channel_transport_failure_never_blames_installed_prerequisites() {
    let transport = "curl: (56) HTTP 504";
    let error = format!("signed channel freshness transport: {transport}");
    let refusal = prerequisite_refusal(error);
    assert!(refusal.could_not_run);
    assert!(refusal.why.contains(transport), "{}", refusal.why);
    assert!(
        refusal.remedy.contains("transport answers"),
        "{}",
        refusal.remedy
    );
    assert!(
        !refusal.remedy.contains("pkg install"),
        "negative control: a freshness failure cannot be repaired by replacing local bytes: {}",
        refusal.remedy
    );

    let local = prerequisite_refusal("gitleaks is not installed".into());
    assert!(local.remedy.contains("pkg install"), "{}", local.remedy);
}

#[cfg(unix)]
#[test]
fn a_warming_verifier_is_bound_to_the_candidate_it_started_from() {
    use super::*;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(root.path(), Some(root.path()), true, false).unwrap();
    let mut command = command(aterm_uds::os_path!("true"));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let child = command.spawn().unwrap();
    let forwarded = Forwarded {
        _running: interrupt::Running::enter(child.id()),
        meter: None,
        note: None,
    };
    let expected = "a".repeat(40);
    real.warming = Some(WarmingVerifier {
        commit: expected.clone(),
        child,
        _forwarded: forwarded,
        stdout: root.path().join("no-build-output"),
        stderr: root.path().join("no-build-errors"),
    });
    // This checkout is not the recorded candidate. Without the binding, the code instead
    // consumes build output or resolves tools, neither of which is authorized here.
    let error = real.finish_tools().unwrap_err();
    assert!(error.contains(&format!("not {expected}")), "{error}");
    assert!(real.warm_verifier.is_none());
    assert!(real.warming.is_none());
}

#[test]
fn step_one_consumes_a_fleet_source_refresh_without_fetching_it_again() {
    use super::*;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(root.path(), Some(root.path()), true, false).unwrap();
    real.prefetched_origin = Some(crate::github_remote::FetchedOrigin {
        main: "a".repeat(40),
        tags: "fixture tags".into(),
    });
    // This directory has no source origin: trying a second refresh is a negative control.
    assert_eq!(real.fetch().unwrap(), "a".repeat(40));
    assert_eq!(real.origin_tags, "fixture tags");
    assert!(real.prefetched_origin.is_none());
}

/// NEGATIVE CONTROL: omitting settlement, or ending only the direct child, leaves the
/// output pipe open. EOF proves each owned build and its inherited-pipe descendant ended.
#[cfg(unix)]
#[test]
fn an_early_refusal_settles_owned_warmups_and_their_groups() {
    use super::*;
    use std::os::fd::AsFd as _;
    let temp = aterm_tempfile::tempdir().unwrap();
    let program = temp.path().join("warming-verifier");
    aterm_tempfile::stand_in(
        &program,
        "#!/bin/sh\n/bin/sh -c 'printf \"ready\\n\"; while :; do /bin/sleep 60; done' &\nwait\n",
    )
    .unwrap();
    for warming in [true, false] {
        let mut command = command(&program);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command.spawn().unwrap();
        let mut output = child.stdout.take().unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        assert!(
            !aterm_uds::ready::any_until(&[output.as_fd()], end)
                .unwrap()
                .is_empty()
        );
        let mut line = [0; 6];
        output.read_exact(&mut line).unwrap();
        assert_eq!(&line, b"ready\n");
        let forwarded = Forwarded {
            _running: interrupt::Running::enter(child.id()),
            meter: None,
            note: None,
        };
        if warming {
            drop(WarmingVerifier {
                commit: "a".repeat(40),
                child,
                _forwarded: forwarded,
                stdout: temp.path().join("out"),
                stderr: temp.path().join("err"),
            });
        } else {
            drop(forwarded);
            aterm_uds::deadline::settle_child(&mut child);
        }
        assert!(
            !aterm_uds::ready::any_until(&[output.as_fd()], end)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            output.read(&mut [0]).unwrap(),
            0,
            "no build descendant retains the pipe"
        );
    }
}

/// Local instrument for the launch boundary only. It never starts a release, reads a
/// roster, or publishes. Run A/B/B/A from a clean, fixed checkout with warm units.
#[test]
#[ignore = "explicit local freeze-time measurement"]
fn measure_cutter_freeze() {
    use super::*;
    let cwd = std::env::current_dir().unwrap();
    let root =
        crate::ledger::git_ok(&GitCli::new(&cwd), &["rev-parse", "--show-toplevel"]).unwrap();
    let repo = PathBuf::from(root.stdout_utf8().trim());
    let sample = std::env::var("ATERM_RELEASE_FREEZE_SAMPLE")
        .expect("A (old two-tool freeze) or B (cutter-only)");
    let temp = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(&repo, Some(&repo), false, false).unwrap();
    real.release = std::env::var_os("ATERM_RELEASE_FREEZE_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.path().join("release"));
    assert!(real.release.is_absolute(), "fixture state must be absolute");
    if std::env::var_os("ATERM_RELEASE_FREEZE_STATE").is_some() {
        real.state = real.release.join("fixture-state");
    }
    real.logs = std::env::var_os("ATERM_RELEASE_FREEZE_LOGS")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.path().join("logs"));
    std::fs::create_dir_all(&real.logs).unwrap();
    let commit = real.head();
    let prover = real.prover(None).unwrap();
    let start = std::time::Instant::now();
    if sample == "A" {
        crate::release_tools::clean_at(&repo, &commit).unwrap();
        let phase = std::time::Instant::now();
        let cutter = real
            .build_frozen_tool(&prover, &FROZEN_CUTTER, "gate")
            .unwrap()
            .0;
        println!(
            "freeze-phase cutter_build_ms={}",
            phase.elapsed().as_millis()
        );
        let phase = std::time::Instant::now();
        let verifier = real
            .build_frozen_tool(&prover, &FROZEN_VERIFY, "test")
            .unwrap()
            .0;
        println!(
            "freeze-phase verifier_build_ms={}",
            phase.elapsed().as_millis()
        );
        let phase = std::time::Instant::now();
        let tools =
            crate::release_tools::stage(&real.release, &commit, &cutter, &verifier).unwrap();
        crate::release_tools::clean_at(&repo, &commit).unwrap();
        crate::release_tools::publish(&real.release, &tools).unwrap();
        println!(
            "freeze-phase copy_prove_publish_ms={}",
            phase.elapsed().as_millis()
        );
    } else {
        assert_eq!(sample, "B");
        let cutter = real.frozen_cutter(&commit, &prover).unwrap();
        println!("freeze-proof {}", aterm_json::to_string(&cutter).unwrap());
    }
    println!(
        "freeze-sample mode={sample} elapsed_ms={}",
        start.elapsed().as_millis()
    );
    let log = std::fs::read_to_string(real.step_log(real.step)).unwrap_or_default();
    for line in log.lines().filter(|line| {
        line.starts_with("release-event: ") || line.starts_with("cutter adoption unavailable:")
    }) {
        println!("{line}");
    }
}

/// Full local launch instrument: real two-freeze/bump/seal work, with the existing
/// authority seam stopping at qualify. The caller owns a clean disposable branch;
/// restore its base between ABBA takes. No real release authority is contacted.
pub(super) fn measure_bump_launch_to_qualify(controller: fn() -> std::time::Duration) {
    use super::*;
    let repo = std::env::current_dir().unwrap().canonicalize().unwrap();
    let git = GitCli::new(&repo);
    let branch = crate::ledger::git_ok(&git, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
    assert!(branch.stdout_utf8().trim().starts_with("freeze-fixture-"));
    let base = crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned();
    crate::release_tools::clean_at(&repo, &base).unwrap();
    let mode = std::env::var("ATERM_RELEASE_LAUNCH_SAMPLE").expect("before or after");
    assert!(mode == "before" || mode == "after" || mode == "prepare");
    let state = PathBuf::from(
        std::env::var_os("ATERM_RELEASE_FREEZE_STATE").expect("private fixture state"),
    );
    assert!(state.is_absolute());
    let editor =
        PathBuf::from(std::env::var_os("ATERM_RELEASE_BUMP_EDITOR").expect("pub-bump-edit"));
    assert!(editor.is_absolute());
    let preparation_start = std::time::Instant::now();
    let preparation_timing = crate::cut_timing::Timer::new("launch-fixture-preparation");
    let mut real = Real::new(&repo, Some(&repo), false, false).unwrap();
    real.state = state.join("seal");
    real.logs = state.join(format!("logs-{mode}-{}", std::process::id()));
    real.release = state.join(if mode == "prepare" { "after" } else { &mode });
    std::fs::create_dir_all(&real.logs).unwrap();
    if mode == "prepare" {
        #[cfg(unix)]
        prepare_cached_cutter(&mut real, &base).unwrap();
        return;
    }
    let prover = real.prover(None).unwrap();
    let freeze = |real: &Real, commit: &str| real.frozen_cutter(commit, &prover).unwrap();
    if mode == "after" {
        // A separate process runs the actual Background preparation, then exits.
        // The measured cut retains its default QoS and inherits no resource cover.
        // Libtest has no tether entry; this opt-in instrument owns and waits for
        // its setsid child rather than detaching it.
        let mut cmd = command(std::env::current_exe().unwrap());
        cmd.args(std::env::args_os().skip(1))
            .env("ATERM_RELEASE_LAUNCH_SAMPLE", "prepare");
        #[cfg(unix)]
        cmd.setsid(true).default_signals();
        assert!(cmd.spawn().unwrap().wait().unwrap().success());
    }
    let preparation_s = preparation_start.elapsed().as_secs_f64();
    drop(preparation_timing);
    let start = std::time::Instant::now();
    let _launch_timing = crate::cut_timing::Timer::new("launch-fixture-cut");
    freeze(&real, &base);
    let version = real.version_at("HEAD").unwrap();
    let parts: Vec<u64> = version.split('.').map(|s| s.parse().unwrap()).collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[2], 0);
    let next = format!("{}.{}.0", parts[0], parts[1] + 1);
    let mut edit = real.child(&editor, None);
    edit.arg(&repo).args([&next, &version]);
    assert_eq!(real.logged(Step::Commit, &mut edit).code, 0);
    let bump_start = std::time::Instant::now();
    let lock_timing = crate::cut_timing::Timer::new("launch-fixture-lockfiles");
    if mode == "before" {
        let mut lock = real.child(prover.bin.join("targo"), Some(&prover));
        lock.args(["update", "--workspace", "--offline", "-q"]);
        assert_eq!(real.logged(Step::Commit, &mut lock).code, 0);
        bump_nested_locks(
            &repo,
            real.git_paths(&["ls-files", "-z", "--", "*/Cargo.lock"]),
            &version,
            &next,
        )
        .unwrap();
    } else {
        let changed = bump_nested_locks(
            &repo,
            real.git_paths(&["ls-files", "-z", "--", "Cargo.lock", "*/Cargo.lock"]),
            &version,
            &next,
        )
        .unwrap();
        real.validate_bumped_locks(&prover, &changed).unwrap();
    }
    let locks_s = bump_start.elapsed().as_secs_f64();
    drop(lock_timing);
    crate::ledger::git_ok(
        &git,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qam",
            "Fixture runtime identity bump",
        ],
    )
    .unwrap();
    let bumped = real.head();
    assert_ne!(bumped, base);
    let cutter = freeze(&real, &bumped);
    if mode == "after" {
        assert!(
            cutter.inputs.is_some(),
            "the fixture needs complete graph evidence"
        );
        assert_ne!(
            cutter.built_from, bumped,
            "the bump must adopt, not stage a new proof"
        );
    }
    // This is the real seal work formerly done by step 1. The authority seam below
    // models controller checks; its wall time is explicitly not service latency.
    real.seal_inputs().unwrap();
    let controller = controller();
    println!(
        "release-event: {}",
        aterm_json::json!({
            "schema":1,"event":"launch-fixture-to-qualify","sample":mode,
            "execution_s": start.elapsed().as_secs_f64(),
            "preparation_s": preparation_s,"locks_s":locks_s,
            "controller_s": controller.as_secs_f64(),"authority":"offline-test-seam",
            "base":base,"commit":bumped,"built_from":cutter.built_from,"inputs":cutter.inputs,
        })
    );
    let log = std::fs::read_to_string(real.step_log(real.step)).unwrap();
    for line in log.lines().filter(|line| {
        line.starts_with("release-event: ") || line.starts_with("cutter adoption unavailable:")
    }) {
        println!("{line}");
    }
}

pub(super) fn bind_native_engine_fixture(repo: &Path, bare: &Path) {
    let git = GitCli::new(repo);
    crate::ledger::git_ok(
        &git,
        &["config", "remote.fixture.url", bare.to_str().unwrap()],
    )
    .unwrap();
    crate::ledger::git_ok(
        &git,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    )
    .unwrap();
}

pub(super) fn native_engine_fixture(
    repo: &Path,
    authority: &Path,
) -> Result<crate::github_remote::Remote, String> {
    let bare = crate::ledger::git_ok(
        &GitCli::new(authority),
        &["config", "--get", "remote.fixture.url"],
    )
    .map_err(|why| why.to_string())?
    .stdout_utf8();
    Ok(crate::github_remote::claim_fixture(
        repo,
        Path::new(bare.trim()),
    ))
}

pub(super) fn fixture_engine_url(repo: &Path) -> Result<String, String> {
    let configured = engine_origin_url(repo)?;
    if !crate::publish::github_slug_from_remote_url(&configured)
        .is_ok_and(|slug| slug == "fixture/app")
    {
        return Ok(configured);
    }
    crate::ledger::git_ok(
        &GitCli::new(repo),
        &["config", "--get", "remote.fixture.url"],
    )
    .map(|out| out.stdout_utf8().trim().to_string())
    .or_else(|_| engine_origin_url(repo))
}

/// The native writer may canonicalize transport to SSH. Only the fixture repository
/// maps to the local bare; a different origin keeps its identity even with stale metadata.
#[test]
fn fixture_engine_origin_survives_transport_normalization_but_not_retargeting() {
    let root = aterm_tempfile::tempdir().unwrap();
    let repo = root.path().join("engine");
    let bare = root.path().join("engine.git");
    crate::ledger::testing::init(&repo, &[]);
    std::fs::create_dir(&bare).unwrap();
    bind_native_engine_fixture(&repo, &bare);
    let git = GitCli::new(&repo);
    crate::ledger::git_ok(
        &git,
        &[
            "config",
            "url.git@github.com:fixture/app.git.insteadOf",
            "https://github.com/fixture/app.git",
        ],
    )
    .unwrap();
    for url in [
        "https://github.com/fixture/app.git",
        "git@github.com:fixture/app.git",
        "ssh://git@github.com/fixture/app.git",
    ] {
        crate::ledger::git_ok(&git, &["remote", "set-url", "origin", url]).unwrap();
        if url == "https://github.com/fixture/app.git" {
            assert_eq!(
                engine_origin_url(&repo).unwrap(),
                "git@github.com:fixture/app.git"
            );
        }
        assert_eq!(
            fixture_engine_url(&repo).unwrap(),
            bare.display().to_string()
        );
    }
    let other = "git@github.com:another/app.git";
    crate::ledger::git_ok(&git, &["remote", "set-url", "origin", other]).unwrap();
    let resolved = fixture_engine_url(&repo).unwrap();
    assert_eq!(
        crate::publish::github_slug_from_remote_url(&resolved).unwrap(),
        "another/app"
    );
}

#[cfg(unix)]
#[test]
fn engine_audit_rows_use_the_native_expected_main_and_reject_retargeting() {
    let root = aterm_tempfile::tempdir().unwrap();
    let engine = root.path().join("engine");
    let bare = root.path().join("engine.git");
    crate::ledger::testing::init(
        &engine,
        &[
            ("user.name", "fixture"),
            ("user.email", "fixture@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    let git = GitCli::new(&engine);
    std::fs::write(engine.join("verification-log.jsonl"), "").unwrap();
    crate::ledger::git_ok(&git, &["add", "verification-log.jsonl"]).unwrap();
    crate::ledger::git_ok(&git, &["commit", "-qm", "initial audit"]).unwrap();
    crate::ledger::git_ok(
        &GitCli::new(root.path()),
        &[
            "clone",
            "-q",
            "--bare",
            engine.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    )
    .unwrap();
    bind_native_engine_fixture(&engine, &bare);
    let mut real = Real::new(&engine, Some(&engine), true, false).unwrap();
    real.engine_origin = native_engine_fixture;
    std::fs::write(engine.join("verification-log.jsonl"), "{\"row\":1}\n").unwrap();
    // Refuse only the audit commit, after its native origin admission and refresh.
    // A nonexistent signer is deterministic and needs no hook or executable fixture.
    let absent_signer = root.path().join("absent-audit-signer");
    crate::ledger::git_ok(
        &git,
        &["config", "gpg.program", absent_signer.to_str().unwrap()],
    )
    .unwrap();
    crate::ledger::git_ok(&git, &["config", "commit.gpgsign", "true"]).unwrap();
    let remote = GitCli::new(&bare);
    let unpublished = crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap();
    let refused = format!("{:?}", real.engine_fast_forward().unwrap_err());
    assert!(refused.contains("exited 128"), "{refused}");
    assert!(refused.contains("absent-audit-signer"), "{refused}");
    assert_eq!(
        crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap(),
        unpublished
    );
    assert_eq!(
        std::fs::read_to_string(engine.join("verification-log.jsonl")).unwrap(),
        "{\"row\":1}\n"
    );
    crate::ledger::git_ok(&git, &["config", "commit.gpgsign", "false"]).unwrap();
    real.engine_fast_forward().unwrap();
    assert_eq!(
        crate::ledger::git_ok(&remote, &["show", "refs/heads/main:verification-log.jsonl"])
            .unwrap()
            .stdout_utf8(),
        "{\"row\":1}\n"
    );
    assert_eq!(real.engine_git(&["status", "--porcelain"]).unwrap(), "");
    let before = crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap();
    crate::ledger::git_ok(
        &git,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/foreign/app.git",
        ],
    )
    .unwrap();
    std::fs::write(
        engine.join("verification-log.jsonl"),
        "{\"row\":1}\n{\"row\":2}\n",
    )
    .unwrap();
    assert!(real.engine_fast_forward().is_err());
    assert_eq!(
        crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_to_string(engine.join("verification-log.jsonl")).unwrap(),
        "{\"row\":1}\n{\"row\":2}\n"
    );
}
#[test]
fn a_moved_engine_main_costs_a_pass_and_any_other_refusal_ends_the_passes() {
    use crate::ledger::RefWrite;
    let refusal = || Refusal::new("the records did not land", "run it again");
    let mut made = 0;
    landed_within(5, || {
        made += 1;
        if made < 3 {
            Err(NotLanded::Moved(refusal()))
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(made, 3, "two lost writes, then the one that landed");

    let mut made = 0;
    let exhausted = landed_within(5, || {
        made += 1;
        Err(NotLanded::Moved(refusal()))
    })
    .unwrap_err();
    assert_eq!(made, 5);
    assert!(
        format!("{exhausted:?}").contains("moved before each of 5 writes"),
        "{exhausted:?}"
    );

    let mut made = 0;
    landed_within(5, || {
        made += 1;
        Err(NotLanded::Refused(refusal()))
    })
    .unwrap_err();
    assert_eq!(
        made, 1,
        "a refusal another pass would meet again is not repeated"
    );

    let outcome = |written| not_landed(written, "main moved", |why| Refusal::new(why, "again"));
    assert!(outcome(Ok(RefWrite::Written)).is_ok());
    assert!(matches!(
        outcome(Ok(RefWrite::Conflict)),
        Err(NotLanded::Moved(_))
    ));
    assert!(matches!(
        outcome(Ok(RefWrite::Transient("receive failed".into()))),
        Err(NotLanded::Refused(_))
    ));
    assert!(matches!(
        outcome(Err("no audit commit".into())),
        Err(NotLanded::Refused(_))
    ));
}

/// The state a lost expected-old write leaves: this release's rows are a commit of the engine,
/// and its origin's main holds another writer's rows. The pass after it merges that main (the
/// audit log merges by union, as the engine's own `.gitattributes` says) and lands both.
#[cfg(unix)]
#[test]
fn rows_that_lost_to_a_moved_engine_main_land_with_the_other_writers_rows() {
    let root = aterm_tempfile::tempdir().unwrap();
    let engine = root.path().join("engine");
    let bare = root.path().join("engine.git");
    let other = root.path().join("other");
    let identity = [
        ("user.name", "fixture"),
        ("user.email", "fixture@example.invalid"),
        ("commit.gpgsign", "false"),
    ];
    crate::ledger::testing::init(&engine, &identity);
    let git = GitCli::new(&engine);
    std::fs::write(
        engine.join(".gitattributes"),
        "verification-log.jsonl merge=union\n",
    )
    .unwrap();
    std::fs::write(engine.join("verification-log.jsonl"), "").unwrap();
    crate::ledger::git_ok(&git, &["add", ".gitattributes", "verification-log.jsonl"]).unwrap();
    crate::ledger::git_ok(&git, &["commit", "-qm", "initial audit"]).unwrap();
    let outside = GitCli::new(root.path());
    for (from, to, how) in [
        (&engine, &bare, "--bare"),
        (&bare, &other, "--no-hardlinks"),
    ] {
        crate::ledger::git_ok(
            &outside,
            &[
                "clone",
                "-q",
                how,
                from.to_str().unwrap(),
                to.to_str().unwrap(),
            ],
        )
        .unwrap();
    }
    bind_native_engine_fixture(&engine, &bare);
    let mut real = Real::new(&engine, Some(&engine), true, false).unwrap();
    real.engine_origin = native_engine_fixture;

    let theirs = GitCli::new(&other);
    for (key, value) in identity {
        crate::ledger::git_ok(&theirs, &["config", key, value]).unwrap();
    }
    std::fs::write(
        other.join("verification-log.jsonl"),
        "{\"row\":\"theirs\"}\n",
    )
    .unwrap();
    crate::ledger::git_ok(&theirs, &["commit", "-qam", "another writer's rows"]).unwrap();
    crate::ledger::git_ok(&theirs, &["push", "-q", "origin", "HEAD:refs/heads/main"]).unwrap();
    std::fs::write(
        engine.join("verification-log.jsonl"),
        "{\"row\":\"ours\"}\n",
    )
    .unwrap();
    crate::ledger::git_ok(
        &git,
        &[
            "commit",
            "-qam",
            "chore(audit): record stage verification rows",
        ],
    )
    .unwrap();

    real.engine_fast_forward().unwrap();
    let remote = GitCli::new(&bare);
    let landed =
        crate::ledger::git_ok(&remote, &["show", "refs/heads/main:verification-log.jsonl"])
            .unwrap()
            .stdout_utf8();
    let mut rows: Vec<&str> = landed.lines().collect();
    rows.sort_unstable();
    assert_eq!(
        rows,
        ["{\"row\":\"ours\"}", "{\"row\":\"theirs\"}"],
        "{landed}"
    );
    assert_eq!(
        crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap(),
        crate::ledger::rev_parse(&git, "HEAD").unwrap()
    );
    assert_eq!(real.engine_git(&["status", "--porcelain"]).unwrap(), "");
}

#[cfg(unix)]
#[test]
fn a_valid_retargeted_engine_adapter_cannot_replace_the_captured_authority() {
    let root = aterm_tempfile::tempdir().unwrap();
    let engine = root.path().join("engine");
    let bare = root.path().join("engine.git");
    crate::ledger::testing::init(
        &engine,
        &[
            ("user.name", "fixture"),
            ("user.email", "fixture@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    let git = GitCli::new(&engine);
    std::fs::write(engine.join("verification-log.jsonl"), "").unwrap();
    crate::ledger::git_ok(&git, &["add", "verification-log.jsonl"]).unwrap();
    crate::ledger::git_ok(&git, &["commit", "-qm", "initial audit"]).unwrap();
    crate::ledger::git_ok(
        &GitCli::new(root.path()),
        &[
            "clone",
            "-q",
            "--bare",
            engine.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    )
    .unwrap();
    bind_native_engine_fixture(&engine, &bare);
    crate::ledger::git_ok(
        &git,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/earlier/engine.git",
        ],
    )
    .unwrap();
    let mut real = Real::new(&engine, Some(&engine), true, false).unwrap();
    // This new native adapter is itself valid and permitted. The common
    // production binding must still reject its different captured slug.
    crate::ledger::git_ok(
        &git,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    )
    .unwrap();
    real.engine_origin = native_engine_fixture;
    assert!(
        native_engine_fixture(&engine, &engine)
            .unwrap()
            .repository()
            .eq_full_name("fixture/app")
    );
    let remote = GitCli::new(&bare);
    let before = crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap();
    std::fs::write(engine.join("verification-log.jsonl"), "{\"row\":1}\n").unwrap();
    let error = real.engine_fast_forward().unwrap_err();
    assert!(format!("{error:?}").contains("captured authority"));
    assert_eq!(
        crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap(),
        before
    );
    assert_eq!(
        git.git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/remotes/origin/main"
        ])
        .unwrap()
        .status,
        1,
        "refused before the native main refresh records its snapshot"
    );
    assert_eq!(
        std::fs::read_to_string(engine.join("verification-log.jsonl")).unwrap(),
        "{\"row\":1}\n"
    );
}

/// Only adopting branch children receive a pool. Build and tree have distinct FIFOs and
/// initial allocations; the command's finalized content context still ignores pool identity.
#[test]
fn the_qualify_pool_goes_only_to_the_branch_children_that_adopt_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = std::env::current_dir().unwrap();
    let mut real = Real::new(&repo, None, true, false).unwrap();
    let prover = Prover {
        bin: PathBuf::from("/fixture/trust/bin"),
        commit: "0".repeat(40),
        source: "fixture".into(),
    };
    let context_outside_pool = real
        .check_content_context(
            Path::new("/fixture/verify"),
            "frozen-tools",
            "seal",
            &prover,
            crate::check_record::Half::Tree,
            crate::check_content::Admission::WaitDefault,
        )
        .unwrap();
    let mut spawn_mirror = real.verifier_child(
        Path::new("/fixture/verify"),
        Some(&prover),
        crate::check_record::Half::Tree,
    );
    real.hand_pool(&mut spawn_mirror, true).unwrap();
    real.finalize_spawn_environment(&mut spawn_mirror).unwrap();
    assert_eq!(
        spawn_mirror
            .get_envs()
            .find(|(name, _)| *name == "GIT_TERMINAL_PROMPT")
            .and_then(|(_, value)| value),
        Some(std::ffi::OsStr::new("0"))
    );
    assert_eq!(
        spawn_mirror
            .get_envs()
            .find(|(name, _)| *name == "GIT_ASKPASS")
            .and_then(|(_, value)| value),
        Some(std::ffi::OsStr::new(""))
    );
    assert_eq!(
        context_outside_pool,
        crate::check_content::context(
            "frozen-tools",
            "seal",
            &spawn_mirror,
            crate::check_content::ReleaseHalf::Tree,
            crate::check_content::Admission::WaitDefault,
        ),
        "the context uses the same environment finalizer as the verifier spawn"
    );
    // `<slots>:<fifo>`, each exactly as handed.
    let pool_of = |cmd: &Command| {
        let value = cmd
            .get_envs()
            .find(|(k, _)| *k == aterm_capacity::POOL_ENV)
            .and_then(|(_, v)| v)?;
        let value = value.to_str().expect("this test's pool value is UTF-8");
        let (slots, fifo) = value.split_once(':').expect("<slots>:<fifo>");
        Some((slots.to_owned(), std::ffi::OsString::from(fifo)))
    };
    let mut none = command("verify");
    real.hand_pool(&mut none, true).unwrap();
    assert_eq!(pool_of(&none), None, "no pool, nothing handed");
    let verifier = Path::new("/frozen/verifier");

    // Asked for by a spelling no child is handed: a `.` component, and a doubled separator
    // too where `TMPDIR` holds one (as `$TMPDIR/.ubd/` does where `$TMPDIR` ends in a
    // separator).
    let path = dir.path().join(".").join("qualify.jobs");
    // This fixture must not publish a Release into the account ledger and cancel an
    // unrelated main-lane judgment. Its four-core machine and ledger are both private.
    let ledger_path = dir.path().join("pool-ledger");
    let ledger = aterm_capacity::Ledger::at(&ledger_path);
    let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 4));
    let pool = qualify_pool(&path, &Branch::ALL, |need| {
        assert_eq!(need.rank, aterm_capacity::Rank::Release);
        aterm_capacity::admit_with(
            &ledger,
            &machine,
            need.cores(4),
            aterm_capacity::Admit::Now,
            aterm_capacity::Pace::default(),
            &mut |_| {},
        )
    })
    .unwrap();
    assert_eq!(
        pool._claim.path().parent().unwrap().canonicalize().unwrap(),
        ledger_path.canonicalize().unwrap(),
        "the synthetic Release belongs only to the fixture's ledger"
    );
    assert_eq!(ledger.list().unwrap().len(), 1);
    assert_eq!(
        pool._claim.record().need.rank,
        aterm_capacity::Rank::Release
    );
    let (slots, export) = (pool.slots, pool.export);
    assert_eq!(slots, 4);
    assert_eq!(export, export_share(slots, true, true));
    let barrier = pool.jobs.reserve_initial().unwrap();
    assert_eq!(
        barrier.len(),
        usize::try_from(slots - export).unwrap(),
        "every compiler token is withheld until both memory reservations stand"
    );
    assert!(
        pool.jobs.pool(false).try_acquire().unwrap().is_none(),
        "no build Cargo driver can start through the admission snapshot"
    );
    assert!(
        pool.jobs.pool(true).try_acquire().unwrap().is_none(),
        "no tree Cargo driver can start through the admission snapshot"
    );
    drop(barrier);
    assert!(
        pool.jobs.pool(false).try_acquire().unwrap().is_some(),
        "releasing the barrier returns the build's compiler tokens"
    );
    assert!(
        pool.jobs.pool(true).try_acquire().unwrap().is_some(),
        "releasing the barrier returns the tree's compiler tokens"
    );
    let fifo = pool.jobs.pool(false).path().as_os_str().to_owned();
    assert_eq!(
        Path::new(&fifo),
        path,
        "the pool's FIFO is the one asked for"
    );
    real.pool = Some(pool);

    let tree = real.verifier_child(verifier, None, crate::check_record::Half::Tree);
    if cfg!(unix) {
        assert_eq!(tree.get_program(), "/usr/bin/nice");
        assert_eq!(
            tree.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new("-n"),
                std::ffi::OsStr::new("10"),
                verifier.as_os_str()
            ]
        );
    }
    // Negative control: bundle checks run after the pool's slices and keep foreground priority.
    let bundle = real.verifier_child(verifier, None, crate::check_record::Half::Bundle);
    assert_eq!(bundle.get_program(), verifier.as_os_str());
    assert_eq!(bundle.get_args().count(), 0);

    let mut actual_tree = real.verifier_child(
        Path::new("/fixture/verify"),
        Some(&prover),
        crate::check_record::Half::Tree,
    );
    real.hand_pool(&mut actual_tree, true).unwrap();
    real.finalize_spawn_environment(&mut actual_tree).unwrap();
    assert_eq!(
        context_outside_pool,
        crate::check_content::context(
            "frozen-tools",
            "seal",
            &actual_tree,
            crate::check_content::ReleaseHalf::Tree,
            crate::check_content::Admission::WaitDefault,
        ),
        "the pre-pool content plan predicts the finalized pooled tree child"
    );
    if cfg!(unix) {
        let mut omitted_wrapper = real.child("/fixture/verify", Some(&prover));
        real.hand_pool(&mut omitted_wrapper, true).unwrap();
        real.finalize_spawn_environment(&mut omitted_wrapper)
            .unwrap();
        assert_ne!(
            context_outside_pool,
            crate::check_content::context(
                "frozen-tools",
                "seal",
                &omitted_wrapper,
                crate::check_content::ReleaseHalf::Tree,
                crate::check_content::Admission::WaitDefault,
            ),
            "omitting nice's program and argv must invalidate tree content evidence"
        );
    }

    let mut build = command("ship");
    real.hand_pool(&mut build, false).unwrap();
    let non_export = slots - export;
    assert_eq!(
        pool_of(&build),
        Some(((non_export - 1).to_string(), fifo.clone())),
        "build receives every non-export token except tree's one"
    );
    let mut verify = command("targo");
    real.hand_pool(&mut verify, true).unwrap();
    assert_eq!(
        pool_of(&verify),
        Some((
            1_u32.to_string(),
            real.pool
                .as_ref()
                .unwrap()
                .jobs
                .pool(true)
                .path()
                .as_os_str()
                .to_owned()
        )),
        "verify has its own FIFO and allocation"
    );
    assert_eq!(
        context_outside_pool,
        real.check_content_context(
            Path::new("/fixture/verify"),
            "frozen-tools",
            "seal",
            &prover,
            crate::check_record::Half::Tree,
            crate::check_content::Admission::WaitDefault,
        )
        .unwrap(),
        "facts before qualify and the verifier inside its pool have one check context"
    );
    let plain = real.child("generator", None);
    assert_eq!(pool_of(&plain), None, "a plain child is handed no pool");

    // NEGATIVE CONTROL: recovering the build share before returning the barrier's original
    // token remints one byte too many. The finish guard must return all originals first.
    let pool = real.pool.as_ref().unwrap();
    let barrier = std::sync::Mutex::new(Some(pool.jobs.reserve_initial().unwrap()));
    drop(FinishShare {
        pool: Some(pool),
        branch: 0,
        admission_barrier: &barrier,
    });
    assert!(
        barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_none(),
        "branch completion returns the admission tokens before recovering its share"
    );
    let drain = |tree| {
        let branch = pool.jobs.pool(tree);
        let mut held = Vec::new();
        while let Some(token) = branch.try_acquire().unwrap() {
            held.push(token);
        }
        held
    };
    let build_tokens = drain(false);
    let tree_tokens = drain(true);
    assert_eq!(
        (build_tokens.len(), tree_tokens.len()),
        (0, usize::try_from(slots - export).unwrap()),
        "early build completion conserves the grant and moves its share to tree"
    );
    drop((build_tokens, tree_tokens));
    drop(barrier);

    // A real one-slot pool must admit tree before probing build's memory admission.
    // The ended predicate is an observation, so this control does not wait on a clock.
    real.pool = None;
    real.pool = Some(
        qualify_pool(&dir.path().join("single.jobs"), &Branch::ALL, |need| {
            aterm_capacity::admit_with(
                &ledger,
                &machine,
                need.cores(1),
                aterm_capacity::Admit::Now,
                aterm_capacity::Pace::default(),
                &mut |_| {},
            )
        })
        .unwrap(),
    );
    let asked = std::cell::Cell::new(0);
    real.branches().build_admitted(&|| {
        asked.set(asked.get() + 1);
        true
    });
    assert_eq!(
        asked.get(),
        0,
        "tree precedes the zero-token build's admission wait"
    );
}

/// The export's reservation stays bounded even on a one-slot host.
#[test]
fn the_export_reservation_is_bounded_by_the_grant() {
    assert_eq!(export_share(18, true, true), 4);
    assert_eq!(export_share(3, true, true), 1);
    assert_eq!(export_share(2, true, true), 1);
    assert_eq!(export_share(1, true, true), 1);
    assert_eq!(export_share(18, false, true), 0);
    assert_eq!(export_share(18, true, false), 18);
}

#[test]
fn a_zero_token_build_cannot_reserve_memory_before_tree_returns_its_share() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares =
        aterm_capacity::BranchShares::new(&dir.path().join("jobs"), 1, 0, [true, true]).unwrap();
    let entered = std::cell::Cell::new(false);
    let result = super::start_with_share(shares.pool(false), &|| true, || entered.set(true));
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
    assert!(
        !entered.get(),
        "no memory admission while the build has zero tokens"
    );
    shares.finish(1);
    super::start_with_share(shares.pool(false), &|| false, || {
        entered.set(true);
        assert!(
            shares.pool(false).try_acquire().unwrap().is_some(),
            "the child can charge its own implicit token"
        );
    })
    .unwrap();
    assert!(entered.get());
}

#[test]
fn a_direct_fixtures_runtime_manifest_survives_the_launcher_scrub() {
    let mut cmd = aterm_uds::spawnfd::command("generator");
    cmd.env("CARGO_MANIFEST_DIR", "/candidate/crates/aterm-gui");
    super::scrub_fixture_command(&mut cmd);
    assert_eq!(
        cmd.get_envs()
            .find(|(k, _)| *k == "CARGO_MANIFEST_DIR")
            .and_then(|(_, v)| v),
        Some(std::ffi::OsStr::new("/candidate/crates/aterm-gui"))
    );
    // The inherited launcher value is still stripped; only an explicit artifact
    // manifest can be restored by the fixture path.
    let mut inherited = aterm_uds::spawnfd::command("generator");
    super::scrub_names(
        &mut inherited,
        [std::ffi::OsString::from("CARGO_MANIFEST_DIR")],
    );
    assert_eq!(
        inherited
            .get_envs()
            .find(|(k, _)| *k == "CARGO_MANIFEST_DIR")
            .and_then(|(_, v)| v),
        None
    );
}
use super::*;

/// Every exec resource offer owns a distinct descriptor slot before any slot becomes an
/// `OwnedFd`. NEGATIVE CONTROL: repeat the toolchain, line, or cohort-writer number in either
/// other variable, and validation must refuse while the original descriptor remains owned by
/// this test.
#[cfg(unix)]
#[test]
fn duplicate_exec_resource_descriptor_offers_are_refused_before_ownership() {
    use std::os::fd::AsRawFd as _;

    let held = std::fs::File::open("/dev/null").unwrap();
    let repeated = std::ffi::OsString::from(held.as_raw_fd().to_string());
    for (lease, line, writer) in [
        (Some(repeated.as_os_str()), Some(repeated.as_os_str()), None),
        (Some(repeated.as_os_str()), None, Some(repeated.as_os_str())),
        (None, Some(repeated.as_os_str()), Some(repeated.as_os_str())),
    ] {
        let why = inherited_fd_offers(lease, line, writer).unwrap_err();
        assert!(why.contains("duplicates"), "{why}");
        assert!(
            held.metadata().is_ok(),
            "refusal took ownership of the offered descriptor"
        );
    }

    let lease = std::fs::File::open("/dev/null").unwrap();
    let line = std::fs::File::open("/dev/null").unwrap();
    let writer = std::fs::File::open("/dev/null").unwrap();
    let values = [
        std::ffi::OsString::from(lease.as_raw_fd().to_string()),
        std::ffi::OsString::from(line.as_raw_fd().to_string()),
        std::ffi::OsString::from(writer.as_raw_fd().to_string()),
    ];
    assert_eq!(
        inherited_fd_offers(
            Some(values[0].as_os_str()),
            Some(values[1].as_os_str()),
            Some(values[2].as_os_str()),
        )
        .unwrap(),
        [
            Some(lease.as_raw_fd()),
            Some(line.as_raw_fd()),
            Some(writer.as_raw_fd()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn publication_engine_bootstrap_requires_a_working_launcher_and_recovers() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("source");
    let engine = dir.path().join("engine");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(engine.join("bin")).unwrap();
    let launcher = engine.join("bin/pub");
    std::fs::write(&launcher, "#!/bin/sh\necho 'gone' >&2\nexit 127\n").unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    match engine_bootstrap_with(&repo, &engine, crate::managed_prerequisites::simulated) {
        Err(Bootstrap::Failed(why)) => {
            assert!(why.contains("(exit status: 127): gone;"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    std::fs::rename(&launcher, engine.join("bin/unavailable-pub")).unwrap();
    std::fs::write(
        &launcher,
        "#!/bin/sh\ncase \"$*\" in '--help') printf 'engine-help-ready\\n';; 'tap --help') printf '%s\\n' '--expect-version VERSION';; *) exit 3;; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(engine_bootstrap_with(&repo, &engine, crate::managed_prerequisites::simulated).is_ok());
    // An old tap may have successful help while lacking the cohort guard.
    std::fs::write(
        &launcher,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> probes\nprintf 'old-help\\n'\n",
    )
    .unwrap();
    let missing = engine_bootstrap_with(&repo, &engine, crate::managed_prerequisites::simulated);
    assert!(matches!(missing, Err(Bootstrap::Failed(why)) if why.contains("--expect-version")));
    assert_eq!(
        std::fs::read_to_string(repo.join("probes")).unwrap(),
        "--help\ntap --help\n"
    );
    // A healthy-looking engine cannot run when signed admission refuses.
    // No script or PATH interpreter is probed as a replacement.
    std::fs::write(&launcher, "#!/bin/sh\nprintf ran > child-ran\n").unwrap();
    let refused = engine_bootstrap_with(&repo, &engine, |wants, _| {
        assert_eq!(wants, crate::managed_prerequisites::PUBLICATION);
        Err("cpython is unavailable on this target".into())
    });
    assert!(matches!(refused, Err(Bootstrap::Prerequisites(why)) if why.contains("unavailable")));
    assert!(!repo.join("child-ran").exists());
}

#[test]
#[cfg(unix)]
fn an_old_tap_refuses_before_validation_tool_provisioning() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let engine = dir.path().join("engine");
    std::fs::create_dir_all(engine.join("bin")).unwrap();
    let script = engine.join("bin/pub");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > tap-probe\nprintf 'legacy tap help\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut real = Real::new(dir.path(), Some(&engine), true, false).unwrap();
    real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
    let refused = real.validation_tools().unwrap_err();
    assert!(refused.why.contains("--expect-version"), "{refused:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("tap-probe")).unwrap(),
        "tap --help\n"
    );
}

#[test]
#[cfg(unix)]
fn the_after_cut_tap_child_uses_the_validated_cohort_and_propagates_refusal() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let engine = dir.path().join("engine");
    std::fs::create_dir_all(engine.join("bin")).unwrap();
    let script = engine.join("bin/pub");
    let mut real = Real::new(dir.path(), Some(&engine), true, false).unwrap();
    assert!(
        real.tap_command("v0.159.0").is_err(),
        "unheld interpreter cannot run"
    );
    real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
    for bad in ["0.159.0", "v0.159.1", "v00.159.0", "v0.159.0 extra"] {
        assert!(real.tap_command(bad).is_err(), "{bad}");
    }
    let command = real.tap_command("v0.159.0").unwrap();
    assert_eq!(command.get_program(), script.as_os_str());
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["tap", "--bump", "--expect-version", "0.159.0"].map(std::ffi::OsStr::new)
    );
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > tap-invocation\nexit 17\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let refused = real.align_tap("v0.159.0").unwrap_err();
    assert!(refused.why.contains("exit 17"), "{refused:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("tap-invocation")).unwrap(),
        "tap --bump --expect-version 0.159.0\n"
    );
    std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$*\" > tap-invocation\nprintf 'canonical verified alignment\\n'\n").unwrap();
    assert!(real.align_tap("v0.159.0").unwrap().contains("v0.159.0"));
    let log = std::fs::read_to_string(real.side_log(Step::AfterCut, "tap")).unwrap();
    assert!(log.contains("canonical verified alignment"));
}

#[test]
fn publication_admission_is_retained_and_a_refusal_never_probes_scanners() {
    static ACQUIRED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
    real.prerequisite_acquire = |wants, _| {
        assert_eq!(wants, crate::managed_prerequisites::PUBLICATION);
        ACQUIRED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(crate::managed_prerequisites::Tools::Simulated)
    };
    real.prepare_prerequisites().unwrap();
    real.prepare_prerequisites().unwrap();
    assert_eq!(ACQUIRED.load(std::sync::atomic::Ordering::SeqCst), 1);
    real.prerequisites = None;
    real.prerequisite_acquire = |_, _| Err("selected package has a bad signature".into());
    real.scanner_probe = |_| panic!("admission must refuse before scanner execution");
    let prover = Prover {
        bin: dir.path().to_path_buf(),
        commit: "fixture".into(),
        source: "fixture".into(),
    };
    let why = real.prepare_scanner_tools(&prover).err().unwrap();
    assert!(why.why.contains("bad signature"));
    assert!(why.could_not_run);
    assert!(real.prerequisites.is_none());
    assert!(real.scanner_tools.is_none());
}

/// A descriptor without FD_CLOEXEC, held open as one is between a `pipe` and its
/// `fcntl`: neither a detached step nor the snapshot's git may inherit it.
#[cfg(unix)]
#[test]
fn forwarded_steps_and_object_git_exclude_an_unflagged_descriptor() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    unsafe extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }
    let file = std::fs::File::open("/dev/null").unwrap();
    // SAFETY: F_DUPFD creates a fresh descriptor without FD_CLOEXEC.
    let fd = unsafe { fcntl(file.as_raw_fd(), 0, 200) };
    assert!(fd >= 200);
    let _held = unsafe { OwnedFd::from_raw_fd(fd) };
    let probe = aterm_spec::host::held_probe(fd);
    let control = aterm_uds::spawnfd::transparent("/bin/sh")
        .args(["-c", &probe])
        .status()
        .unwrap();
    assert_eq!(control.code(), Some(3));

    let repo = std::env::current_dir().unwrap();
    let real = Real::new(&repo, None, true, false).unwrap();
    let out = real
        .output_forwarded(command("/bin/sh").args(["-c", &probe]))
        .unwrap();
    assert!(out.status.success(), "detached step inherited fd {fd}");

    let alias = format!("alias.fd-proof=!{probe}");
    let out = crate::ledger::object_git(&repo)
        .unwrap()
        .args(["-c", &alias, "fd-proof"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "object git inherited fd {fd}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The appcast adapter hands every transport attempt to `Launched<StepChildren>`'s launch
/// boundary. NEGATIVE CONTROL: replacing that adapter with the client's direct spawn leaves
/// `started` at zero; starting a later retry after the launch reports an interrupt makes the
/// second half fail.
#[test]
fn appcast_retries_are_release_children_and_an_interrupt_starts_no_later_attempt() {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    struct Launch {
        interrupt_after_first: bool,
        interrupted: AtomicBool,
        started: Arc<AtomicU32>,
    }

    impl crate::site::Launch for Launch {
        fn interrupted(&self) -> Option<i32> {
            self.interrupted.load(Ordering::SeqCst).then_some(15)
        }

        fn spawn(
            &self,
            command: &mut aterm_uds::spawnfd::Command,
        ) -> std::io::Result<(aterm_uds::spawnfd::Child, crate::site::Guard)> {
            if self.interrupted().is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "not started: the release was interrupted",
                ));
            }
            let child = command.spawn()?;
            self.started.fetch_add(1, Ordering::SeqCst);
            if self.interrupt_after_first {
                self.interrupted.store(true, Ordering::SeqCst);
            }
            Ok((child, Box::new(())))
        }
    }

    let fetch = |_: &str,
                 _: u64,
                 spawn: &dyn aterm_update_core::github_rest::Spawn|
     -> Result<Vec<u8>, String> {
        for _ in 0..3 {
            let mut command = aterm_uds::spawnfd::command("/bin/sh");
            command.args(["-c", "printf appcast"]);
            let output = spawn
                .run(&mut command, None)
                .map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(format!("fixture child exited {}", output.status));
            }
        }
        Ok(b"appcast".to_vec())
    };

    let started = Arc::new(AtomicU32::new(0));
    assert_eq!(
        appcast_bytes_with(
            "https://example.invalid/appcast",
            1024,
            Launch {
                interrupt_after_first: false,
                interrupted: AtomicBool::new(false),
                started: Arc::clone(&started),
            },
            fetch,
        ),
        Ok(b"appcast".to_vec())
    );
    assert_eq!(started.load(Ordering::SeqCst), 3);

    let started = Arc::new(AtomicU32::new(0));
    let refused = appcast_bytes_with(
        "https://example.invalid/appcast",
        1024,
        Launch {
            interrupt_after_first: true,
            interrupted: AtomicBool::new(false),
            started: Arc::clone(&started),
        },
        fetch,
    )
    .expect_err("the next retry is not started after the interrupt");
    assert!(refused.contains("not started"), "{refused}");
    assert_eq!(started.load(Ordering::SeqCst), 1);
}

/// The site sync's children keep the release's rules: each leads a session and group of
/// its own (where the interrupt is forwarded, with no terminal to stop on), git's prompt
/// is off, and a running-child note names it while it runs and is gone once it ends.
#[cfg(unix)]
#[test]
fn the_site_syncs_children_keep_the_releases_rules() {
    use crate::site::Launch as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-step-children-")
        .tempdir()
        .unwrap();
    let children = StepChildren {
        noted: Some((dir.path().to_path_buf(), Step::AfterCut)),
    };
    assert_eq!(children.interrupted(), None);
    let mut sh = aterm_uds::spawnfd::command("/bin/sh");
    sh.args([
        "-c",
        "echo \"$$ $(ps -o pgid= -p $$ | tr -d ' ') ${GIT_TERMINAL_PROMPT-unset} [${GIT_ASKPASS-unset}]\"",
    ]);
    let out = crate::site::output(&children, &mut sh).unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    let fields: Vec<&str> = said.split_whitespace().collect();
    assert_eq!(fields.len(), 4, "{said}");
    assert_eq!(
        fields[0], fields[1],
        "the child leads its own group: {said}"
    );
    assert_eq!(fields[2], "0", "git's prompt is off: {said}");
    assert_eq!(fields[3], "[]", "and so is its askpass: {said}");
    assert!(
        crate::release::child_notes(dir.path()).is_empty(),
        "the note goes with the child"
    );
    let mut sleeper = aterm_uds::spawnfd::command("/bin/sh");
    sleeper
        .args(["-c", "read line"])
        .stdin(aterm_uds::spawnfd::Stdio::piped());
    let (mut child, guard) = children.spawn(&mut sleeper).unwrap();
    let note = std::fs::read_to_string(crate::release::child_note(dir.path(), child.id())).unwrap();
    assert!(
        note.starts_with(&format!("pid {}\n", child.id())) && note.ends_with("step after-cut\n"),
        "{note}"
    );
    drop(child.stdin.take());
    child.wait().unwrap();
    drop(guard);
    assert!(crate::release::child_notes(dir.path()).is_empty());
}

#[test]
fn a_pointer_that_could_not_be_read_is_could_not_run() {
    let refused = pointer_verdict(Err("https://x/latest could not be read: timeout".into()))
        .expect_err("a failed read refuses");
    assert!(refused.could_not_run, "{refused:?}");
    assert_eq!(
        pointer_verdict(Ok(vec!["404".into()])).expect("shape problems pass through"),
        ["404"]
    );
}

fn pointer_release_list() -> aterm_json::Value {
    aterm_json::json!([
        {"tag_name": "v0.5.0", "draft": false, "prerelease": false,
         "published_at": "2026-11-08T00:00:00Z"},
        {"tag_name": "v0.6.0", "draft": false, "prerelease": false,
         "published_at": "2026-11-09T00:00:00Z"},
        {"tag_name": "v0.7.0", "draft": true, "prerelease": false,
         "published_at": "2026-11-09T00:00:00Z"},
        {"tag_name": "v0.8.0", "draft": false, "prerelease": true,
         "published_at": "2026-11-09T00:00:00Z"},
        {"tag_name": "v0.9.0", "draft": false, "prerelease": false,
         "published_at": null}
    ])
}

/// Negative controls: making any latest surface a one-shot read fails the two-stale-then-
/// current arm; returning the old shape refusal at the bound fails the retryable arm;
/// trusting the redirect instead of the list fails the successor arm (including when the
/// redirect still names this cut). Dropping the page or any asset fails the URL inventory.
#[test]
fn after_cut_latest_surfaces_wait_for_propagation_and_only_a_proven_successor_refuses() {
    let assets = lean();
    let pointers = latest_pointer_urls("owner/repo", "v0.6.0", &release(&assets, "lean"));
    assert_eq!(pointers.len(), assets.len() + 1);
    assert!(pointers.contains(&(
        "https://github.com/owner/repo/releases/latest".into(),
        "https://github.com/owner/repo/releases/tag/v0.6.0".into(),
    )));
    for (asset, _) in &assets {
        assert!(pointers.contains(&(
            format!("https://github.com/owner/repo/releases/latest/download/{asset}"),
            format!("https://github.com/owner/repo/releases/download/v0.6.0/{asset}"),
        )));
    }
    let mut counts = std::collections::BTreeMap::new();
    let (mut slept, mut lists) = (0, 0);
    let problems = settled_pointer_problems(
        &pointers,
        "v0.6.0",
        &mut |ptr| {
            let reads = counts.entry(ptr.to_string()).or_insert(0);
            *reads += 1;
            let want = &pointers.iter().find(|(url, _)| url == ptr).unwrap().1;
            Ok((
                302,
                Some(if *reads <= 2 {
                    want.replace("v0.6.0", "v0.5.0")
                } else {
                    want.clone()
                }),
            ))
        },
        &mut || {
            lists += 1;
            Ok(pointer_release_list())
        },
        &mut |pause| {
            assert_eq!(pause, crate::publish::POINTER_PROBE_DELAY);
            slept += 1;
        },
    )
    .unwrap();
    assert!(problems.is_empty());
    assert_eq!((slept, lists), (2, 3));
    assert_eq!(counts.len(), pointers.len());
    assert!(counts.values().all(|count| *count == 3));
    eprintln!(
        "after-cut injected propagation: {} surfaces, 3 reads each, 2 waits (20 s scheduled), PASS",
        pointers.len()
    );

    for stale in [(302, Some("https://x/v0.5.0".into())), (404, None)] {
        let (mut reads, mut slept) = (0, 0);
        let why = settled_pointer_problems(
            &pointers[..1],
            "v0.6.0",
            &mut |_| {
                reads += 1;
                Ok(stale.clone())
            },
            &mut || Ok(pointer_release_list()),
            &mut |_| slept += 1,
        )
        .unwrap_err();
        assert!(why.contains("propagation did not finish"), "{why}");
        assert!(!why.contains("another release holds"), "{why}");
        assert!(pointer_verdict(Err(why)).unwrap_err().could_not_run);
        assert_eq!(reads, crate::publish::POINTER_PROBE_ATTEMPTS);
        assert_eq!(slept, crate::publish::POINTER_PROBE_ATTEMPTS - 1);
    }

    let mut lists = 0;
    let problems = settled_pointer_problems(
        &pointers[..1],
        "v0.6.0",
        &mut |_| Ok((302, Some("https://x/v0.5.0".into()))),
        &mut || {
            lists += 1;
            let mut list = pointer_release_list();
            list.as_array_mut().unwrap()[2]
                .as_object_mut()
                .unwrap()
                .insert("draft".into(), aterm_json::Value::Bool(lists == 1));
            Ok(list)
        },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(lists, 2, "a successor published during the wait ends it");
    assert!(problems[0].contains("another release holds GitHub's latest"));
    assert!(problems[0].contains("v0.7.0"));
    let problems = settled_pointer_problems(
        &pointers[..1],
        "v0.6.0",
        &mut |_| panic!("the list proves a successor even if the redirect is cached or unreadable"),
        &mut || {
            let mut list = pointer_release_list();
            list.as_array_mut().unwrap()[2]
                .as_object_mut()
                .unwrap()
                .insert("draft".into(), aterm_json::Value::Bool(false));
            Ok(list)
        },
        &mut |_| panic!("a proven successor refuses immediately"),
    )
    .unwrap();
    assert!(problems[0].contains("another release holds GitHub's latest"));
}

/// Negative control: retrying a failed read instead of a stale pointer trips the sleeper;
/// re-reading a settled surface trips its exact read count.
#[test]
fn after_cut_pointer_reads_fail_unknown_and_settled_surfaces_are_not_polled() {
    let pointers = vec![
        ("page".into(), "current-page".into()),
        ("asset".into(), "current-asset".into()),
    ];
    for once in [Ok((503, None)), Err("timeout".into())] {
        let mut reads = 0;
        assert!(
            settled_pointer_problems(
                &pointers,
                "v0.6.0",
                &mut |_| {
                    reads += 1;
                    once.clone()
                },
                &mut || Ok(pointer_release_list()),
                &mut |_| panic!("a failed read is final")
            )
            .is_err()
        );
        assert_eq!(reads, 1);
    }
    let (mut pages, mut assets) = (0, 0);
    assert!(
        settled_pointer_problems(
            &pointers,
            "v0.6.0",
            &mut |ptr| {
                if ptr == "page" {
                    pages += 1;
                    Ok((302, Some("current-page".into())))
                } else {
                    assets += 1;
                    Ok((
                        302,
                        Some(
                            if assets == 1 {
                                "old-asset"
                            } else {
                                "current-asset"
                            }
                            .into(),
                        ),
                    ))
                }
            },
            &mut || Ok(pointer_release_list()),
            &mut |_| {}
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!((pages, assets), (1, 2));
    assert!(
        settled_pointer_problems(
            &pointers,
            "v0.6.0",
            &mut |_| panic!("list read failed"),
            &mut || Err("API unavailable".into()),
            &mut |_| panic!("failed list is final")
        )
        .is_err()
    );
}

fn release(assets: &[(&str, u64)], body: &str) -> aterm_json::Value {
    let uploaded: Vec<(&str, u64, &str)> =
        assets.iter().map(|&(n, s)| (n, s, "uploaded")).collect();
    release_in_states(&uploaded, body)
}

/// A release whose assets each carry the `state` GitHub lists them in.
fn release_in_states(assets: &[(&str, u64, &str)], body: &str) -> aterm_json::Value {
    let assets: Vec<String> = assets
        .iter()
        .map(|(n, s, state)| format!("{{\"name\":\"{n}\",\"size\":{s},\"state\":\"{state}\"}}"))
        .collect();
    aterm_json::from_str(&format!(
        "{{\"tag_name\":\"v0.6.0\",\"body\":{body:?},\"assets\":[{}]}}",
        assets.join(",")
    ))
    .unwrap()
}

fn lean() -> Vec<(&'static str, u64)> {
    vec![
        ("aterm-0.6.0.dmg", 40_600_000),
        ("aterm-0.6.0-mac.zip", 38_000_000),
        ("aterm.dmg", 40_600_000),
        ("aterm-mac.zip", 38_000_000),
        ("aterm-0.6.0.dmg.sha256", 90),
        ("aterm-0.6.0-mac.zip.sha256", 90),
        ("aterm.dmg.sha256", 90),
        ("aterm-mac.zip.sha256", 90),
        ("aterm-appcast.toml", 900),
        ("aterm-appcast.toml.sig", 100),
        ("aterm-machines.toml", 900),
        ("aterm-machines.toml.sig", 100),
        ("SHA256SUMS", 680_000),
        ("SHA256SUMS.sig", 64),
    ]
}

#[test]
fn source_publication_requires_one_manifest_and_one_signature_before_binary_assets() {
    let pair = [("SHA256SUMS", 100), ("SHA256SUMS.sig", 64)];
    assert!(source_attestation_findings(&release(&pair, "")).is_empty());
    for assets in [
        vec![],
        vec![pair[0]],
        vec![pair[1]],
        vec![pair[0], pair[0], pair[1]],
    ] {
        assert!(!source_attestation_findings(&release(&assets, "")).is_empty());
    }
    // NEGATIVE CONTROL: a names-only count passed an upload GitHub still lists as unfinished
    // (or one that finished empty), at step 7 and in after-cut's shape alike.
    for unfinished in [
        [
            ("SHA256SUMS", 100, "uploaded"),
            ("SHA256SUMS.sig", 0, "starter"),
        ],
        [
            ("SHA256SUMS", 100, "uploaded"),
            ("SHA256SUMS.sig", 64, "open"),
        ],
        [
            ("SHA256SUMS", 0, "starter"),
            ("SHA256SUMS.sig", 64, "uploaded"),
        ],
        [
            ("SHA256SUMS", 100, "uploaded"),
            ("SHA256SUMS.sig", 0, "uploaded"),
        ],
    ] {
        let found = source_attestation_findings(&release_in_states(&unfinished, ""));
        assert_eq!(found.len(), 1, "{unfinished:?}: {found:?}");
        let mut whole = lean();
        whole.retain(|(n, _)| !n.starts_with("SHA256SUMS"));
        let mut whole: Vec<(&str, u64, &str)> =
            whole.into_iter().map(|(n, s)| (n, s, "uploaded")).collect();
        whole.extend(unfinished);
        let bad = release_shape(&release_in_states(&whole, "notes"), "o/aterm", "0.6.0");
        assert_eq!(bad.len(), 1, "{unfinished:?}: {bad:?}");
    }
}

fn candidate_of(version: &str) -> Candidate {
    Candidate {
        commit: "d".repeat(40),
        version: version.into(),
        why: String::new(),
        bump_from: None,
        carry: Vec::new(),
    }
}

/// NEGATIVE CONTROL: step 7's refusal said only "finish pub sign-release aterm": the engine
/// reaches the journaled private draft only with `--keep-draft --draft-state` (without them
/// it takes a published release of the tag, publishing a draft it finds), and a published
/// source release's form named no tag.
#[test]
fn step_seven_names_the_sign_release_form_that_reaches_the_release_it_read() {
    let mut lab = crate::publish::draft::tests::Lab::for_cut("0.191.0", 191, &"a".repeat(40));
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    lab.fake.apply(agh_fake::Change::AddAsset {
        release_id: id,
        spec: agh_fake::AssetSpec::uploaded("SHA256SUMS", b"source evidence"),
    });
    lab.fake.apply(agh_fake::Change::AddAsset {
        release_id: id,
        spec: agh_fake::AssetSpec {
            state: "starter".into(),
            ..agh_fake::AssetSpec::uploaded("SHA256SUMS.sig", b"")
        },
    });
    let mut real = Real::new(&lab.ctx.repo, Some(&lab.ctx.repo), true, false).unwrap();
    let c = candidate_of("0.191.0");
    let refusal = real.source_publication(&c, 191).unwrap_err();
    assert!(!refusal.could_not_run, "{refusal:?}");
    assert!(
        refusal.why.contains("SHA256SUMS.sig") && refusal.why.contains("starter"),
        "{refusal:?}"
    );
    assert!(
        refusal.remedy.contains(&lab.keep_draft_remedy()),
        "{}",
        refusal.remedy
    );

    // No draft journaled, and the tag's published release lacks its signature.
    std::fs::remove_file(&lab.ctx.journal_path).unwrap();
    let published = lab.fake.add_release(
        &lab.ctx.slug,
        agh_fake::ReleaseSpec::published("v0.191.0", "2026-11-09T00:00:00Z"),
    );
    lab.fake.apply(agh_fake::Change::AddAsset {
        release_id: published,
        spec: agh_fake::AssetSpec::uploaded("SHA256SUMS", b"source evidence"),
    });
    let refusal = real.source_publication(&c, 191).unwrap_err();
    assert!(
        refusal.why.contains("SHA256SUMS.sig is missing"),
        "{refusal:?}"
    );
    assert!(
        refusal
            .remedy
            .contains("pub sign-release aterm --tag v0.191.0")
            && !refusal.remedy.contains("--keep-draft"),
        "{}",
        refusal.remedy
    );
}

/// STEP 7 JUDGES ONLY THE DRAFT JOURNALED FOR ITS OWN BUILD: dist/draft-state.toml names
/// build 192's draft, whose signature is still uploading, and R's reservation is build 191.
/// That draft is not R's, so step 7 reads R's release by tag (pending: the sign then meets
/// the foreign journal and names how to withdraw it), and the source signing onto R's draft
/// refuses (exit 1) with nothing run.
/// NEGATIVE CONTROL: the journal was filtered by version only, so step 7 judged build 192's
/// draft and named the `--keep-draft` sign-release, which would attach R's pair to build
/// 192's draft: the engine's cutter_draft checks the journal's version, create intent and the
/// commit's parent (R for every build of a version), never its build.
#[test]
fn step_seven_judges_only_the_draft_journaled_for_its_build() {
    let mut lab = crate::publish::draft::tests::Lab::for_cut("0.191.0", 192, &"a".repeat(40));
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    lab.fake.apply(agh_fake::Change::AddAsset {
        release_id: id,
        spec: agh_fake::AssetSpec {
            state: "starter".into(),
            ..agh_fake::AssetSpec::uploaded("SHA256SUMS.sig", b"")
        },
    });
    let mut real = Real::new(&lab.ctx.repo, Some(&lab.ctx.repo), true, false).unwrap();
    let c = candidate_of("0.191.0");
    assert_eq!(
        crate::publish::journaled_draft(&lab.ctx.dist, "0.191.0", 191),
        Ok(None)
    );
    let state = real.source_publication(&c, 191);
    assert!(
        matches!(&state, Ok(SourceState::Pending(why)) if why.contains("no private draft")),
        "{state:?}"
    );
    // The source signing has no draft of build 191 to attach to: refused, and nothing ran.
    let prover = Prover {
        bin: lab.ctx.repo.join("prover/bin"),
        commit: "33".repeat(20),
        source: "fixture".into(),
    };
    let refusal = real.attest_draft(&c, 191, &prover).unwrap_err();
    assert!(!refusal.could_not_run, "{refusal:?}");
    assert!(
        refusal.why.contains("names no private draft of v0.191.0")
            && refusal.why.contains("nothing ran"),
        "{refusal:?}"
    );
    // Nothing ran: the publish log holds no command line (`$ …`, as attest_draft and
    // Real::logged write one), only source_publication's own timer events.
    let log = std::fs::read_to_string(real.step_log(Step::Publish)).unwrap_or_default();
    assert!(!log.lines().any(|line| line.starts_with("$ ")), "{log}");
    // Build 192's own step 7 judges its draft.
    let refusal = real.source_publication(&c, 192).unwrap_err();
    assert!(refusal.remedy.contains("--keep-draft"), "{refusal:?}");
}

/// A JOURNALED DRAFT GITHUB NO LONGER ANSWERS IS A PERSON'S, NOT A RETRY'S: someone deleted
/// the private draft (as the withdrawal remedy's `agh api … -X DELETE` does) and left its
/// journal. Step 7 refuses (exit 1), naming the journal and its receipt to remove once the
/// releases page shows the draft gone; the next sign then drafts afresh. NEGATIVE CONTROL:
/// the by-id 404 was COULD NOT RUN "once the channel answers", every resume stopped there,
/// and the next `cut --sign` would only await a draft that never comes back.
#[test]
fn step_seven_refuses_a_journaled_draft_github_no_longer_answers() {
    let mut lab = crate::publish::draft::tests::Lab::for_cut("0.191.0", 191, &"a".repeat(40));
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    lab.fake
        .apply(agh_fake::Change::DeleteRelease { release_id: id });
    let mut real = Real::new(&lab.ctx.repo, Some(&lab.ctx.repo), true, false).unwrap();
    let refusal = real
        .source_publication(&candidate_of("0.191.0"), 191)
        .unwrap_err();
    assert!(!refusal.could_not_run, "{refusal:?}");
    assert!(
        refusal.why.contains(&format!("no release {id}")),
        "{refusal:?}"
    );
    assert!(
        refusal
            .remedy
            .contains("remove dist/draft-state.toml and dist/draft-bytes.toml")
            && refusal.remedy.contains("releases page"),
        "{}",
        refusal.remedy
    );

    // Done as it says, the sign has no draft to adopt: pending, as before any sign.
    std::fs::remove_file(&lab.ctx.journal_path).unwrap();
    std::fs::remove_file(lab.ctx.dist.join("draft-bytes.toml")).unwrap();
    let state = real.source_publication(&candidate_of("0.191.0"), 191);
    assert!(matches!(&state, Ok(SourceState::Pending(_))), "{state:?}");
}

/// NEGATIVE CONTROL: with no draft id journaled (the sign failed before it bound one, or an
/// ambiguous create POST left only its intent) step 7 read R's release by tag, GitHub's 404
/// (no release, or a draft the lookup by tag never serves) was COULD NOT RUN, and every
/// resume stopped there before the sign that makes and journals the draft.
#[test]
fn step_seven_reads_an_unjournaled_draft_as_pending_not_unreadable() {
    let mut lab = crate::publish::draft::tests::Lab::for_cut("0.191.0", 191, &"a".repeat(40));
    let mut real = Real::new(&lab.ctx.repo, Some(&lab.ctx.repo), true, false).unwrap();
    let c = candidate_of("0.191.0");
    assert_eq!(
        crate::publish::journaled_draft(&lab.ctx.dist, "0.191.0", 191),
        Ok(None)
    );
    let state = real.source_publication(&c, 191);
    assert!(
        matches!(&state, Ok(SourceState::Pending(why)) if why.contains("no private draft")),
        "{state:?}"
    );

    // The draft exists, but the journal kept only the create intent.
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    let bound = std::fs::read(&lab.ctx.journal_path).unwrap();
    let mut journal = crate::publish::Journal::load(&lab.ctx.journal_path)
        .unwrap()
        .unwrap();
    journal.release_id = None;
    journal.upload_intents.clear();
    journal.save(&lab.ctx.journal_path).unwrap();
    let state = real.source_publication(&c, 191);
    assert!(matches!(&state, Ok(SourceState::Pending(_))), "{state:?}");

    // The journaled draft, its pair missing: pending; then attested.
    std::fs::write(&lab.ctx.journal_path, bound).unwrap();
    let state = real.source_publication(&c, 191);
    assert!(
        matches!(&state, Ok(SourceState::Pending(why)) if why.contains(&id.to_string())),
        "{state:?}"
    );
    lab.attest();
    assert_eq!(real.source_publication(&c, 191), Ok(SourceState::Attested));

    // Any other unreadable answer stays COULD NOT RUN.
    lab.fake.fault_always(
        agh_fake::Match::api(&format!("/repos/{}/releases/{id}", lab.ctx.slug)).method("GET"),
        agh_fake::Fault::Status {
            status: 502,
            body: "bad gateway".into(),
            headers: Vec::new(),
        },
    );
    let state = real.source_publication(&c, 191);
    assert!(matches!(&state, Err(r) if r.could_not_run), "{state:?}");
}

/// 0.193.0's step 7 read its private draft by tag and GitHub answered 404, as it does for
/// every draft: a journaled draft is read by its id, a cut with none by its tag.
#[test]
fn step_seven_reads_the_journaled_private_draft_by_id() {
    assert_eq!(
        source_release_path("o/aterm", "v0.6.0", Some(440_210_489)),
        "repos/o/aterm/releases/440210489"
    );
    assert_eq!(
        source_release_path("o/aterm", "v0.6.0", None),
        "repos/o/aterm/releases/tags/v0.6.0"
    );
}

#[test]
fn a_draft_github_unbound_or_another_release_is_refused_before_the_claim() {
    let with = |id: u64, tag: &str| -> aterm_json::Value {
        aterm_json::from_str(&format!(
            "{{\"id\":{id},\"tag_name\":\"{tag}\",\"assets\":[]}}"
        ))
        .unwrap()
    };
    assert_eq!(
        source_binding_problem(&with(7, "v0.6.0"), "o/a", "v0.6.0", Some(7)),
        None
    );
    assert_eq!(
        source_binding_problem(&with(7, "v0.6.0"), "o/a", "v0.6.0", None),
        None
    );
    let (why, fix) = source_binding_problem(
        &with(7, "untagged-88239b0cc2be8b1abf48"),
        "o/a",
        "v0.6.0",
        Some(7),
    )
    .unwrap();
    assert!(why.contains("untagged-88239b0cc2be8b1abf48"), "{why}");
    assert!(
        fix.contains("releases/7 -X PATCH -f tag_name=v0.6.0"),
        "{fix}"
    );
    let (why, fix) = source_binding_problem(&with(8, "v0.6.0"), "o/a", "v0.6.0", Some(7)).unwrap();
    assert!(why.contains("not the private draft 7"), "{why}");
    // Before the claim no cut journal exists for `ship cut --abandon` to read.
    assert!(fix.contains("ship release --forget"), "{fix}");
}

#[test]
fn a_release_is_judged_by_the_lean_shape_github_serves() {
    assert_eq!(
        release_shape(&release(&lean(), "notes"), "o/aterm", "0.6.0"),
        Vec::<String>::new()
    );
    let mut seeded = lean();
    seeded[0].1 = 1_070_000_000;
    seeded.push(("aterm-0.6.0-x86_64.dmg", 9));
    seeded.retain(|(n, _)| *n != "aterm-mac.zip.sha256" && *n != "SHA256SUMS.sig");
    seeded[2].1 = 1;
    let bad = release_shape(
        &release(&seeded, "the batteries-included image"),
        "o/aterm",
        "0.6.0",
    );
    assert_eq!(bad.len(), 6, "{bad:#?}");
    for asset in [
        "aterm-mac.zip.sha256",
        "aterm-0.6.0-x86_64.dmg",
        "SHA256SUMS.sig",
    ] {
        assert!(bad.iter().any(|p| p.contains(asset)), "{asset}: {bad:#?}");
    }
    let mut linux = lean();
    linux.push(("aterm-0.6.0-linux-x86_64", 40_000_000));
    linux.push(("aterm-0.6.0-linux-riscv64", 40_000_000));
    linux.push(("aterm-0.5.0-linux-aarch64", 40_000_000));
    linux.push(("aterm-0.6.0-linux-aarch64.tar.gz", 1_000));
    linux.retain(|(n, _)| *n != "aterm-machines.toml.sig");
    let bad = release_shape(&release(&linux, ""), "o/aterm", "0.6.0");
    for asset in [
        "aterm-0.6.0-linux-riscv64",
        "aterm-0.5.0-linux-aarch64",
        "aterm-0.6.0-linux-aarch64.tar.gz.sha256",
        "aterm-machines.toml.sig",
    ] {
        assert!(bad.iter().any(|p| p.contains(asset)), "{asset}: {bad:#?}");
    }
}

#[test]
fn the_appcast_the_pointer_serves_names_this_version_and_binds_its_url() {
    let slug = "alabsystems/aterm";
    let good = "version = \"0.6.0\"\ndmg = \"aterm-0.6.0.dmg\"\n\
                url = \"https://github.com/alabsystems/aterm/releases/download/v0.6.0/aterm-0.6.0.dmg\"\n";
    assert!(appcast_problems(good, slug, "v0.6.0", "0.6.0").is_empty());
    assert!(!appcast_problems(good, slug, "v0.7.0", "0.7.0").is_empty());
    let unbound = good.replace("download/v0.6.0", "download/v0.5.0");
    assert!(!appcast_problems(&unbound, slug, "v0.6.0", "0.6.0").is_empty());
}

#[test]
fn the_engines_dry_run_is_read_in_its_own_words() {
    let text = "== exported 5807 of 7058 tracked files from abc\n\
                == guard ok: no forbidden content\n\
                == GUARD FAIL: private references (2)\n  a.rs:3\n  b.md:9\n\
                FAIL: guards failed — nothing was pushed\n";
    let run = EngineRun::read(1, text);
    assert_eq!(run.passed, ["no forbidden content"]);
    assert_eq!(
        run.failed,
        [(
            "private references (2)".to_string(),
            vec!["a.rs:3".to_string(), "b.md:9".to_string()]
        )]
    );
    assert_eq!(run.refusals, ["guards failed — nothing was pushed"]);
    assert_eq!(EngineRun::read(0, "").tail, Vec::<String>::new());
}

#[test]
fn an_engine_refusal_is_sorted_by_its_cause() {
    for line in [
        "malformed manifest line (bare '!'): '!'",
        "empty manifest",
        "captured aterm commit has no publish/manifest.txt",
        "publication policy contains a symlink, which could escape immutable inputs: x",
        "CHECK_TIMEOUT_SECONDS_DEFAULT must be a positive integer",
        "vendor/astream is not a gitlink in abc",
        "transforms failed",
        "the export does not pin the Trust toolchain the dev commit pins",
        "detected version '1.2' is not a semver string",
        "no staging remote configured",
        "staging remote must identify alabsystems/aterm-staging, found 'x'",
    ] {
        assert_eq!(EngineCause::of(line), EngineCause::Tree, "{line}");
    }
    for (line, cause) in [
        (
            "gitleaks not installed — a fail-closed guard cannot be skipped",
            EngineCause::NoGitleaks,
        ),
        (
            "gitleaks crashed/timed out — aborting rather than fail open",
            EngineCause::GitleaksBroke,
        ),
        (
            "release notes: cannot read the public tags of alabsystems/aterm: git \
             ls-remote failed",
            EngineCause::Network,
        ),
        (
            "release notes: cannot fetch the previous public release v0.1.0",
            EngineCause::Network,
        ),
        (
            "cannot query aterm origin/main: fatal: unable to access",
            EngineCause::Network,
        ),
        (
            "astream public release main fetch failed: Host key verification failed.",
            EngineCause::Network,
        ),
        // The engine's agh transport (2026-11-05), measured on this Mac on 2026-11-06 with
        // the installed agh: a read through it is agh's to route, not the network's.
        (
            "cannot query publication engine origin/main: pub GitHub transport: Agh exited 2: \
             aterm help: unknown topic 'repo'",
            EngineCause::Transport,
        ),
        (
            "astream public release main fetch failed: pub GitHub transport: Agh exited 4: \
             agh: not_found exit=4",
            EngineCause::Transport,
        ),
        (
            "Agh lacks the required exact anonymous fetch capability; select the qualified \
             client with PUBLISH_AGH",
            EngineCause::Transport,
        ),
        (
            "publication engine HEAD a does not exactly match queried origin/main b",
            EngineCause::Engine,
        ),
        (
            "another publication operation for aterm is running; cannot stage",
            EngineCause::Engine,
        ),
        (
            "--commit abc is not an ancestor of origin/main def",
            EngineCause::Source,
        ),
    ] {
        assert_eq!(EngineCause::of(line), cause, "{line}");
    }
}

#[test]
fn the_export_guards_pass_refuse_or_could_not_run() {
    let (log, reports) = (Path::new("/c/dry-run.log"), Path::new("/c/reports"));
    let outcome = |code: i32, text: &str, toolchain: Option<&Refusal>| {
        export_outcome(&EngineRun::read(code, text), "r", toolchain, log, reports)
    };
    let refused =
        |code: i32, text: &str, toolchain: Option<&Refusal>| match outcome(code, text, toolchain) {
            Outcome::Refuse(r) => r,
            other => panic!("{text:?}: {other:?}"),
        };
    let not_run =
        |code: i32, text: &str, toolchain: Option<&Refusal>| match outcome(code, text, toolchain) {
            Outcome::CouldNotRun(r) => r,
            other => panic!("{text:?}: {other:?}"),
        };
    let no_toolchain = Refusal::new("no rustup", "install rustup");
    let exported = "== exported 1 of 1 tracked files from r\n";
    let ok = format!("{exported}== guard ok: no forbidden content\n== guard ok: gitleaks clean\n");
    assert!(matches!(outcome(0, &ok, None), Outcome::Pass(_)));
    assert!(matches!(
        outcome(0, &ok, Some(&no_toolchain)),
        Outcome::Pass(_)
    ));
    let hit = format!("{exported}== GUARD FAIL: forbidden content (1 hits; first 5):\n  x.rs:1\n");
    for after in [
        "FAIL: guards failed — nothing was pushed\n",
        "FAIL: gitleaks not installed — a fail-closed guard cannot be skipped\n",
        "FAIL: gitleaks crashed/timed out — aborting rather than fail open\n",
    ] {
        let r = refused(1, &format!("{hit}{after}"), None);
        assert!(r.detail.iter().any(|l| l == "  x.rs:1"), "{r:?}");
    }
    for (text, line) in [
        (
            "FAIL: malformed manifest line (bare '!'): '!'\n".to_string(),
            "malformed manifest line (bare '!'): '!'",
        ),
        (
            format!(
                "{exported}FAIL: vendor/astream is not a gitlink in abc\nFAIL: transforms \
                 failed\n"
            ),
            "vendor/astream is not a gitlink in abc",
        ),
    ] {
        assert!(refused(1, &text, None).why.contains(line), "{text}");
    }
    for (text, line) in [
        (
            "FAIL: cannot query aterm origin/main: fatal: unable to access\n".to_string(),
            "cannot query aterm origin/main",
        ),
        (
            format!(
                "{exported}== guard ok: no forbidden content\nFAIL: gitleaks not installed \
                 — a fail-closed guard cannot be skipped\n"
            ),
            "gitleaks not installed",
        ),
        (
            format!(
                "{exported}== guard ok: no forbidden content\nFAIL: gitleaks crashed/timed \
                 out — aborting rather than fail open\n"
            ),
            "gitleaks crashed",
        ),
        (
            format!(
                "{exported}FAIL: release notes: cannot read the public tags of \
                 alabsystems/aterm: git ls-remote failed\n"
            ),
            "release notes: cannot read the public tags",
        ),
    ] {
        assert!(not_run(1, &text, None).why.contains(line), "{text}");
    }
    let metadata = format!(
        "{exported}== guard ok: gitleaks clean\n== GUARD FAIL: targo metadata failed (see \
         cargo-metadata.err)\nFAIL: guards failed — nothing was pushed\n"
    );
    refused(1, &metadata, None);
    assert_eq!(
        not_run(1, &metadata, Some(&no_toolchain)).why,
        no_toolchain.why
    );
    for (code, text) in [
        (0, exported),
        (1, exported),
        (-1, "cannot run /e/bin/pub: No such file or directory"),
    ] {
        not_run(code, text, None);
    }
}

#[test]
fn a_gitleaks_finding_is_named_by_place_and_rule_never_by_its_secret() {
    let report = r#"[{"File":"/x/export/a/b.rs","StartLine":7,"RuleID":"generic-api-key","Match":"key=hunter2","Secret":"hunter2"}]"#;
    assert_eq!(
        gitleaks_locations(report, Path::new("/x/export")),
        ["a/b.rs:7 generic-api-key"]
    );
    assert!(gitleaks_locations("not json", Path::new("/x")).is_empty());
    assert_eq!(
        gitleaks_report("gitleaks findings (see /o/gitleaks-adjudicated.json)"),
        Some("/o/gitleaks-adjudicated.json")
    );
    assert_eq!(gitleaks_report("private references (2)"), None);
}

#[test]
fn ship_check_export_drives_a_scratch_copy_of_the_engine_at_its_origins_main() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let git = |cwd: &Path, args: &[&str]| -> String {
        let out = crate::ledger::testing::git()
            .current_dir(cwd)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let (engine, engine_origin) = (root.join("publication"), root.join("publication.git"));
    let (repo, origin) = (root.join("aterm"), root.join("aterm.git"));
    for bare in [&engine_origin, &origin] {
        git(
            &root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                &bare.display().to_string(),
            ],
        );
    }
    for (dir, remote) in [(&engine, &engine_origin), (&repo, &origin)] {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        git(
            dir,
            &["remote", "add", "origin", &remote.display().to_string()],
        );
    }
    // The engine's GIT_* environment, as the gate hands it on from this process's own.
    let reads = public_reads(
        std::env::var_os("GIT_CONFIG_COUNT").as_deref(),
        &[],
        std::env::var_os("GIT_CONFIG_PARAMETERS").as_deref(),
    )
    .unwrap();
    let seen = root.join("seen-env");
    let scanner_report = root.join("scanner-report.json");
    std::fs::write(&scanner_report, scanner_tools::test_output().stdout).unwrap();
    let dump: String = reads
        .iter()
        .map(|(key, _)| {
            format!(
                "printf '%s=%s\\n' {key} \"${key}\" >> '{}'\n",
                seen.display()
            )
        })
        .collect();
    let agh = agh_standin(&repo);
    aterm_verify::standin(&agh, "exit 0");
    let stub = |say: &str, code: i32| {
        std::fs::create_dir_all(engine.join("bin")).unwrap();
        let pubbin = engine.join("bin/pub");
        std::fs::write(
            &pubbin,
            format!(
                "#!/bin/sh\nhere=$(cd \"$(dirname \"$0\")/..\" && pwd)\n\
                 if [ \"$1\" = scanner-preflight ]; then cat '{scanner}'; exit 0; fi\n\
                 [ -n \"$PUBLISH_SCANNER_TOOLS\" ] && cmp -s \"$PUBLISH_SCANNER_TOOLS\" '{scanner}' || exit 10\n\
                 [ \"$PUBLISH_AGH\" = '{agh}' ] || exit 11\n\
                 head=$(git -C \"$here\" rev-parse HEAD) || exit 12\n\
                 remote=$(git --git-dir='{engine_origin}' rev-parse refs/heads/main) || exit 12\n\
                 [ \"$head\" = \"$remote\" ] || {{ printf 'FAIL: publication engine HEAD %s does not exactly match queried origin/main %s\\n' \"$head\" \"$remote\"; exit 1; }}\n\
                 echo '{{\"row\":1}}' \
                 >> \"$here/verification-log.jsonl\"\n[ \"$1 $2 $3 $4\" = 'stage aterm \
                 --dry-run --no-check' ] || exit 9\n{dump}printf '%s' '{say}'\nexit {code}\n",
                scanner = scanner_report.display(),
                agh = agh.display(),
                engine_origin = engine_origin.display(),
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&pubbin).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&pubbin, perms).unwrap();
        std::fs::write(engine.join("verification-log.jsonl"), "").unwrap();
        git(&engine, &["add", "-A"]);
        git(&engine, &["commit", "-q", "-m", "stub"]);
        git(
            &engine,
            &[
                "push",
                "-q",
                "--force",
                engine_origin.to_str().unwrap(),
                "main",
            ],
        );
    };
    std::fs::write(
        repo.join("Cargo.toml"),
        "[workspace.package]\nversion = \"0.2.0\"\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    let opts = |commit: &str, engine: &Path| CheckOptions {
        commit: Some(commit.to_string()),
        engine: Some(engine.to_path_buf()),
        export: true,
        ..CheckOptions::default()
    };
    let export_built = |commit: &str, build: CheckoutBuild| {
        bind_native_engine_fixture(&engine, &engine_origin);
        let found = checked_with(&repo, &opts(commit, &engine), |real| {
            real.engine_origin = native_engine_fixture;
            real.prerequisite_acquire = crate::managed_prerequisites::simulated;
            real.checkout_build = build;
            // This test builds the checkout's agh, whatever PUBLISH_AGH its caller set.
            real.engine_agh_explicit = None;
        })
        .expect("a release");
        let gate = found.gates.last().expect("the export gate");
        assert_eq!(gate.name, EXPORT_GATE);
        (gate.outcome.clone(), found.exit_code())
    };
    let export = |commit: &str| export_built(commit, built_agh);
    let exported = "== exported 1 of 1 tracked files from r\n";
    stub(&format!("{exported}== guard ok: no forbidden content\n"), 0);
    let (outcome, code) = export("HEAD");
    assert!(matches!(outcome, Outcome::Pass(_)), "{outcome:?}");
    assert_eq!(code, 0);
    let want: String = reads
        .iter()
        .map(|(k, v)| format!("{k}={}\n", v.to_string_lossy()))
        .collect();
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), want);
    assert_eq!(
        std::fs::read_to_string(engine.join("verification-log.jsonl")).unwrap(),
        ""
    );
    assert_eq!(git(&engine, &["status", "--porcelain"]), "");
    // The fixture's engine advances while this checkout prepares its GitHub client.
    // The actual stage must still receive that origin's current main, not the copy
    // fetched before preparation. No elapsed-time threshold or remote network is involved.
    let (outcome, code) = export_built("HEAD", |real, what, args, plan| {
        let built = built_agh(real, what, args, plan)?;
        for args in [
            vec![
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "advance during agh build",
            ],
            vec!["push", "-q", "fixture", "main"],
        ] {
            let out = crate::ledger::testing::git()
                .current_dir(&real.engine)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(&args)
                .output()
                .map_err(|why| why.to_string())?;
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(built)
    });
    assert!(matches!(outcome, Outcome::Pass(_)), "{outcome:?}");
    assert_eq!(code, 0);
    stub("FAIL: malformed manifest line (bare !): !\n", 1);
    git(&engine, &["reset", "-q", "--hard", "HEAD~1"]);
    let (outcome, code) = export("HEAD");
    assert!(matches!(outcome, Outcome::Refuse(_)), "{outcome:?}");
    assert_eq!(code, 1);
    stub(
        "FAIL: cannot query aterm origin/main: fatal: unable to access\n",
        1,
    );
    let (outcome, code) = export("HEAD");
    assert!(matches!(outcome, Outcome::CouldNotRun(_)), "{outcome:?}");
    assert_eq!(code, 3);
    stub(&format!("{exported}== guard ok: no forbidden content\n"), 0);
    // No agh the engine can read GitHub through: the gate stops before the dry run, and its
    // refusal names what would.
    let (outcome, code) = export_built("HEAD", broken_agh);
    match &outcome {
        Outcome::CouldNotRun(why) => assert!(
            why.why.contains("only through agh")
                && why.why.contains("error[E0425]")
                && why.remedy.contains("PUBLISH_AGH"),
            "{why:?}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(code, 3);
    std::fs::write(repo.join("new.rs"), "\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "unpushed"]);
    let (outcome, code) = export("HEAD");
    assert!(matches!(outcome, Outcome::CouldNotRun(_)), "{outcome:?}");
    assert_eq!(code, 3);
    git(
        &repo,
        &["push", "-q", "origin", "HEAD:refs/heads/export-feature"],
    );
    git(&repo, &["fetch", "-q", "origin"]);
    let branch = checked_with(&repo, &opts("HEAD", &engine), |real| {
        real.prerequisite_acquire = |_, _| panic!("a feature commit acquires no export tools");
        real.checkout_build = |_, _, _, _| panic!("a feature commit builds no export tools");
    })
    .unwrap();
    assert!(matches!(
        branch.gates.last().map(|gate| &gate.outcome),
        Some(Outcome::CouldNotRun(_))
    ));
    assert_eq!(
        branch.exit_code(),
        3,
        "a feature branch push is not mainline"
    );
    let mut unread = Real::new(&repo, Some(&engine), true, false).unwrap();
    unread.prerequisite_acquire = |_, _| panic!("unread ancestry acquires no export tools");
    let (outcome, _) = unread.export_gate(&Scratch::read_only(&repo).unwrap(), "no-such-commit");
    assert!(matches!(outcome, Outcome::CouldNotRun(_)), "{outcome:?}");
    let found = checked_with(&repo, &opts("HEAD~1", &root.join("none")), |real| {
        real.prerequisite_acquire = crate::managed_prerequisites::simulated;
        real.checkout_build = |_, _, _, _| panic!("a missing engine builds no export tools");
    })
    .unwrap();
    assert!(matches!(
        found.gates.last().map(|g| &g.outcome),
        Some(Outcome::CouldNotRun(_))
    ));
    assert!(checked(&repo, &opts("no-such-commit", &engine)).is_err());
}

/// Where the stand-in agh of the checkout at `repo` is "built", outside the checkout.
fn agh_standin(repo: &Path) -> PathBuf {
    repo.with_file_name("agh-build").join("agh")
}

#[cfg(unix)]
#[test]
fn local_export_preparation_bounds_the_real_checkout_build_and_reuses_its_client() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    crate::ledger::git_ok(&GitCli::new(&repo), &["init", "-q"]).unwrap();
    let image = agh_standin(&repo);
    aterm_verify::standin(&image, "exit 0");
    let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
    real.engine_agh_explicit = None;
    real.checkout_build = |real, what, args, plan| {
        assert!(args.windows(2).any(|pair| pair == ["--jobs", "3"]));
        built_agh(real, what, args, plan)
    };
    assert!(real.engine_agh_with_jobs(Some(0)).is_err());
    assert_eq!(real.engine_agh_with_jobs(Some(3)).unwrap(), image);
    real.engine_agh_explicit = Some(image.as_os_str().to_owned());
    real.checkout_build = broken_agh;
    assert_eq!(real.engine_agh().unwrap(), image);
}

#[test]
fn a_prepared_export_client_rejects_a_changed_image_or_candidate() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("client");
    std::fs::write(&path, "first image").unwrap();
    let (sha256, _) = crate::dmg::sha256_and_size(&path).unwrap();
    let client = LocalExportClient {
        path: path.clone(),
        source: "candidate".into(),
        sha256,
        jobs: 3,
        wall_s: 0.0,
    };
    assert!(client.validate("candidate").is_ok());
    assert!(client.validate("another candidate").is_err());
    std::fs::write(&path, "changed image").unwrap();
    assert!(client.validate("candidate").is_err());
}

/// A build of the checkout's agh that cargo answers with the stand-in's path.
fn built_agh(
    real: &Real,
    what: &str,
    args: &[std::ffi::OsString],
    _: &aterm_verify::NativeBuildPlan,
) -> Result<Vec<u8>, String> {
    let pair = |flag: &str, value: &str| args.windows(2).any(|w| w[0] == flag && w[1] == value);
    assert!(
        what == "agh" && pair("-p", "agh") && pair("--bin", "agh"),
        "{what}: {args:?}"
    );
    let manifest = real.repo.join("crates/agh/Cargo.toml");
    Ok(aterm_json::json!({
        "reason": "compiler-artifact",
        "manifest_path": manifest.to_str().unwrap(),
        "target": {"name": "agh", "kind": ["bin"]},
        "executable": agh_standin(&real.repo).to_str().unwrap(),
    })
    .to_string()
    .into_bytes())
}

/// A build of the checkout's agh that does not compile.
fn broken_agh(
    _: &Real,
    what: &str,
    _: &[std::ffi::OsString],
    _: &aterm_verify::NativeBuildPlan,
) -> Result<Vec<u8>, String> {
    Err(format!(
        "{what} build exited exit status: 101: error[E0425]"
    ))
}

/// [`PUBLIC_READS`] numbered from `base`, the way [`public_reads`] hands them to the engine.
fn public_reads_at(base: usize) -> Vec<(String, std::ffi::OsString)> {
    let pair = |key: String, value: &str| (key, value.into());
    vec![
        pair(
            format!("GIT_CONFIG_KEY_{base}"),
            "url.https://github.com/alabsystems/.insteadOf",
        ),
        pair(
            format!("GIT_CONFIG_VALUE_{base}"),
            "git@github.com:alabsystems/",
        ),
        pair(
            format!("GIT_CONFIG_KEY_{}", base + 1),
            "credential.https://github.com/alabsystems.helper",
        ),
        pair(format!("GIT_CONFIG_VALUE_{}", base + 1), ""),
        pair(
            format!("GIT_CONFIG_KEY_{}", base + 2),
            "credential.https://github.com/alabsystems.helper",
        ),
        pair(
            format!("GIT_CONFIG_VALUE_{}", base + 2),
            "!f() { echo quit=1; }; f",
        ),
        pair(
            format!("GIT_CONFIG_KEY_{}", base + 3),
            "http.https://github.com/alabsystems/.extraHeader",
        ),
        pair(format!("GIT_CONFIG_VALUE_{}", base + 3), ""),
        pair("GIT_CONFIG_COUNT".to_string(), &(base + 4).to_string()),
        pair(
            "GIT_CONFIG_PARAMETERS".to_string(),
            concat!(
                "'url.https://github.com/alabsystems/.insteadOf=git@github.com:alabsystems/' ",
                "'credential.https://github.com/alabsystems.helper=' ",
                "'credential.https://github.com/alabsystems.helper=!f() { echo quit=1; }; f' ",
                "'http.https://github.com/alabsystems/.extraHeader='",
            ),
        ),
        pair("GIT_TERMINAL_PROMPT".to_string(), "0"),
    ]
}

/// The engine's dry run reads alabsystems as the public does, whatever this host's SSH keys: each `git@github.com:alabsystems/` URL over HTTPS, with no credential for `https://github.com/alabsystems` from a helper or an askpass program, numbered after the caller's own `GIT_CONFIG_*` entries, which still apply, while every other GitHub URL keeps its transport and its helper.
#[test]
fn the_engines_dry_run_reads_the_public_tier_anonymously_after_the_callers_git_config() {
    let reads = |count: Option<&str>| public_reads(count.map(std::ffi::OsStr::new), &[], None);
    assert_eq!(reads(None).unwrap(), public_reads_at(0));
    assert_eq!(reads(Some("")).unwrap(), public_reads_at(0));
    assert_eq!(reads(Some("3")).unwrap(), public_reads_at(3));
    for bogus in ["two", "-1", " 1", "1 ", &usize::MAX.to_string()] {
        let why = reads(Some(bogus)).unwrap_err();
        assert!(
            why.why.contains("GIT_CONFIG_COUNT") && why.why.contains(bogus),
            "{why:?}"
        );
    }

    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut real = Real::new(&root, Some(&root), true, false).unwrap();
    real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
    let (stage, handed) = real
        .dry_run_command(
            &root.join("engine"),
            "c0ffee",
            &root.join("clone"),
            &reads(Some("1")).unwrap(),
            &root.join("agh"),
        )
        .unwrap();
    let (full, _) = real
        .local_stage_command(
            &root.join("engine"),
            "c0ffee",
            &root.join("clone"),
            &reads(Some("1")).unwrap(),
            &root.join("agh"),
            Some(3),
        )
        .unwrap();
    assert!(stage.get_args().any(|arg| arg == "--no-check"));
    assert!(!full.get_args().any(|arg| arg == "--no-check"));
    assert!(full.get_args().any(|arg| arg == "--dry-run"));
    assert!(
        full.get_envs()
            .any(|(key, value)| key == "CARGO_BUILD_JOBS"
                && value == Some(std::ffi::OsStr::new("3")))
    );
    // This test process was never judged at its start, so it is no commit's build: the dry
    // run it builds is handed no cutter, not even one this process inherited.
    for name in [RELEASE_CUTTER_ENV, RELEASE_CUTTER_SINCE_ENV] {
        assert!(
            stage
                .get_envs()
                .any(|(k, v)| k == std::ffi::OsStr::new(name) && v.is_none()),
            "{name}: {handed}"
        );
    }
    assert!(
        handed.ends_with("c0ffee's publish/transforms.sh builds its own"),
        "{handed}"
    );
    let mut sent: Vec<(String, std::ffi::OsString)> = stage
        .get_envs()
        .filter_map(|(key, value)| {
            let key = key.to_str().filter(|k| k.starts_with("GIT_"))?;
            let value = value.map_or_else(|| "<removed>".into(), |v| v.to_os_string());
            Some((key.to_string(), value))
        })
        .collect();
    sent.sort();
    let mut want = public_reads_at(1);
    want.sort();
    assert_eq!(sent, want, "the engine's GIT_* environment");

    // What git makes of it, under a caller's own entry 0, a helper for every GitHub URL and
    // an askpass program that answers any prompt.
    let config = root.join("gitconfig");
    std::fs::write(
        &config,
        "[credential \"https://github.com\"]\n\thelper = \"!f() { echo username=fixture; \
         echo password=fixture-pass; }; f\"\n",
    )
    .unwrap();
    let askpass = root.join("askpass");
    std::fs::write(&askpass, "#!/bin/sh\necho askpass-pass\n").unwrap();
    let mut perms = std::fs::metadata(&askpass).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&askpass, perms).unwrap();
    let git = |args: &[&str], input: Option<&str>| {
        let mut git = crate::ledger::testing::git();
        git.current_dir(&root)
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("GIT_ASKPASS", &askpass)
            .env_remove("SSH_ASKPASS")
            .env(
                "GIT_CONFIG_KEY_0",
                "url.https://example.invalid/mirror/.insteadOf",
            )
            .env("GIT_CONFIG_VALUE_0", "fixture:")
            .envs(sent.iter().map(|(key, value)| (key, value)))
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = git.spawn().unwrap();
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin.write_all(input.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        (out.status.success(), text(&out.stdout), text(&out.stderr))
    };
    let url = |from: &str| git(&["ls-remote", "--get-url", from], None).1;
    assert_eq!(
        url("git@github.com:alabsystems/a3d.git").trim(),
        "https://github.com/alabsystems/a3d.git"
    );
    assert_eq!(
        url("git@github.com:alabsystems/publication.git").trim(),
        "git@github.com:alabsystems/publication.git",
        "a private address keeps this host's transport"
    );
    assert_eq!(
        url("fixture:x").trim(),
        "https://example.invalid/mirror/x",
        "the caller's own entry still applies"
    );
    let fill = |url: &str| git(&["credential", "fill"], Some(&format!("url={url}\n\n")));
    let (ok, out, err) = fill("https://github.com/alabsystems/publication.git");
    assert!(ok && out.contains("password=fixture-pass"), "{out}{err}");
    for public in [
        "https://github.com/alabsystems/a3d.git",
        "https://alabsystems@github.com/alabsystems/aterm.git",
    ] {
        let (ok, out, err) = fill(public);
        assert!(
            !ok && !out.contains("password=") && err.contains("told us to quit"),
            "{public}: {out}{err}"
        );
    }
}

#[test]
fn the_wasm32_gate_runs_rs_own_clause_under_a_stub_rustup() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let git = |cwd: &Path, args: &[&str]| -> String {
        let out = crate::ledger::testing::git()
            .current_dir(cwd)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let repo = root.join("aterm");
    std::fs::create_dir_all(repo.join("publish")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join("publish/config.sh"),
        "CHECK_CMD_DEFAULT=\"rustup target add --toolchain 9.9.9 wasm32-unknown-unknown && \
         RUSTC=$(rustup which --toolchain 9.9.9 rustc) rustup run 9.9.9 cargo check \
         --locked --target wasm32-unknown-unknown -p aterm-wasm\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "r"]);
    let r = git(&repo, &["rev-parse", "HEAD"]);
    let stub = root.join("rustup-bin");
    std::fs::create_dir_all(&stub).unwrap();
    std::fs::write(
        stub.join("rustup"),
        "#!/bin/sh\nhere=$(cd \"$(dirname \"$0\")\" && pwd)\ncase \"$1\" in\n\
         target) exit 0 ;;\n\
         which) echo \"$here/rustc\"; exit 0 ;;\n\
         run) [ -f Cargo.toml ] || { echo \"error: no tree laid out in $(pwd)\" >&2; exit 101; }\n\
         \x20 [ -f \"$here/broken\" ] && { echo 'error[E0425]: cannot find value `x`' >&2; exit 101; }\n\
         \x20 echo 'Finished'; exit 0 ;;\nesac\n\
         echo \"stub rustup: unexpected $*\" >&2; exit 2\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(stub.join("rustup"))
        .unwrap()
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(stub.join("rustup"), perms).unwrap();
    let tools = Wasm32Tools {
        rustup: stub.clone(),
        path: std::env::join_paths([stub.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
            .unwrap(),
    };
    let scratch = Scratch {
        dir: root.join("scratch"),
        cache: root.join("cache"),
    };
    std::fs::create_dir_all(&scratch.dir).unwrap();
    let export = root.join("export");
    std::fs::create_dir_all(&export).unwrap();
    std::fs::write(export.join("Cargo.toml"), "[workspace]\n").unwrap();
    let mut real = Real::new(&repo, None, true, false).expect("a backend");

    let absent = aterm_uds::host::tool("rustup")
        .locate_in(&aterm_uds::host::Lookup {
            named: None,
            tooling: Some(root.join("tooling.noindex")),
            path: None,
        })
        .unwrap_err();
    let none = real.wasm32_gate(&scratch, &r, Some(&export), Err(&absent));
    assert!(
        matches!(&none, Outcome::CouldNotRun(refused)
            if refused.why.starts_with("no rustup (looked in")
                && refused.remedy.contains("targo --unverified verify provision")),
        "no rustup: {none:?}"
    );
    let passed = real.wasm32_gate(&scratch, &r, Some(&export), Ok(&tools));
    assert!(matches!(passed, Outcome::Pass(_)), "{passed:?}");
    std::fs::write(stub.join("broken"), "").unwrap();
    let broken = real.wasm32_gate(&scratch, &r, Some(&export), Ok(&tools));
    match &broken {
        Outcome::Refuse(why) => assert!(
            why.detail.iter().any(|l| l.starts_with("error[E0425]")),
            "the compiler's errors ride along: {why:?}"
        ),
        other => panic!("a compile that fails refuses: {other:?}"),
    }
    std::fs::remove_file(stub.join("broken")).unwrap();
    let archived = real.wasm32_gate(&scratch, &r, None, Ok(&tools));
    assert!(matches!(archived, Outcome::Pass(_)), "{archived:?}");
    assert!(scratch.dir.join("wasm-src/publish/config.sh").is_file());
}

const CRLF_PROBE_ENV: &str = "ATERM_RELEASE_CRLF_PROBE_DIR";
const CRLF_PROBE: &str = "release_steps::tests::every_tree_the_release_writes_is_the_commits_bytes";

fn run_crlf_git(at: &Path, args: &[&str]) -> String {
    let out = crate::ledger::testing::git()
        .arg("-C")
        .arg(at)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write_crlf_fixture_file(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn setup_crlf_fixture_repos(root: &Path) -> (PathBuf, String) {
    use crate::ledger::testing;
    let ident = [
        ("user.name", "t"),
        ("user.email", "t@example.invalid"),
        ("commit.gpgsign", "false"),
    ];
    let sub = root.join("sub");
    testing::init(&sub, &ident);
    write_crlf_fixture_file(&sub.join("s.sh"), "#!/bin/sh\n");
    run_crlf_git(&sub, &["add", "-A"]);
    run_crlf_git(&sub, &["commit", "-q", "-m", "s"]);
    let (repo, origin) = (root.join("aterm"), root.join("aterm.git"));
    testing::init(&repo, &ident);
    write_crlf_fixture_file(&repo.join(".gitattributes"), "*.txt text\n");
    write_crlf_fixture_file(&repo.join("bin/pub"), "#!/bin/sh\n");
    write_crlf_fixture_file(&repo.join("notes.txt"), "one\n");
    write_crlf_fixture_file(&repo.join("RELEASES.ledger"), "1783354739 0.25.0\n");
    write_crlf_fixture_file(
        &repo.join("CHANGELOG.md"),
        "## [Unreleased]\n\n### Added\n- An entry.\n",
    );
    let sub_s = sub.display().to_string();
    let add_sub = ["submodule", "add", "-q", &sub_s, "vendor/sub"];
    run_crlf_git(
        &repo,
        &[&["-c", "protocol.file.allow=always"][..], &add_sub].concat(),
    );
    run_crlf_git(&repo, &["add", "-A"]);
    run_crlf_git(&repo, &["commit", "-q", "-m", "r"]);
    run_crlf_git(root, &["init", "-q", "--bare", "-b", "main", "aterm.git"]);
    run_crlf_git(
        &repo,
        &["remote", "add", "origin", &origin.display().to_string()],
    );
    run_crlf_git(&repo, &["push", "-q", "origin", "main"]);
    let head = run_crlf_git(&repo, &["rev-parse", "HEAD"]);
    (repo, head)
}

fn probe_every_tree_the_release_writes_under_a_crlf_config_if_asked() -> bool {
    use std::fs;
    let Some(root) = std::env::var_os(CRLF_PROBE_ENV).map(PathBuf::from) else {
        return false;
    };
    let (repo, r) = setup_crlf_fixture_repos(&root);
    // The configuration the parent chose is what this probe's own git reads (it was started
    // with `crate::ledger::testing::CHOSEN_GIT_ENV`), not the test binary's hermetic one.
    let host = GitCli::new(&repo);
    let read = |args: &[&str]| {
        crate::ledger::GitRunner::git(&host, args)
            .unwrap()
            .stdout_utf8()
            .trim()
            .to_owned()
    };
    assert_eq!(read(&["config", "core.autocrlf"]), "true");
    assert_eq!(
        read(&["check-attr", "eol", "--", "notes.txt"]),
        "notes.txt: eol: crlf"
    );
    let lf = |tree: &Path, notes: &str, with_sub: bool| {
        let read = |rel: &str| fs::read_to_string(tree.join(rel)).unwrap();
        assert_eq!(read("bin/pub"), "#!/bin/sh\n", "{}", tree.display());
        assert_eq!(read("notes.txt"), notes, "{}", tree.display());
        if with_sub {
            assert_eq!(read("vendor/sub/s.sh"), "#!/bin/sh\n", "{}", tree.display());
        }
    };
    bind_native_engine_fixture(&repo, &root.join("aterm.git"));
    let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
    real.engine_origin = native_engine_fixture;
    let scratch = Scratch {
        dir: root.join("scratch"),
        cache: root.join("cache"),
    };
    fs::create_dir_all(&scratch.dir).unwrap();
    lf(&real.scratch_engine(&scratch).unwrap(), "one\n", false);
    let clone = scratch.dir.join("dry-run/aterm");
    real.dry_clone(&scratch.dir, &clone, &r).unwrap();
    lf(&clone, "one\n", true);
    let archive = root.join("archive");
    fs::create_dir_all(&archive).unwrap();
    assert!(real.archive_into(&r, &archive));
    lf(&archive, "one\n", true);
    // The engine segment above uses its explicit native fixture. The
    // placement segment exercises the existing local-file route; keep its
    // authority local rather than accidentally selecting network Git.
    run_crlf_git(
        &repo,
        &[
            "remote",
            "set-url",
            "origin",
            root.join("aterm.git").to_str().unwrap(),
        ],
    );
    let place = |commit: &str| {
        let source = crate::gates::PublishedSource {
            commit: commit.to_string(),
            verified_at: "2026-10-21T00:00:00+00:00".to_string(),
        };
        let git = GitCli::new(&repo);
        let origin = crate::gates::fixture_origin::FixtureOrigin(&git);
        crate::gates::place_published(&git, &origin, &repo, &source).unwrap()
    };
    lf(&place(&r).tree, "one\n", true);
    write_crlf_fixture_file(&repo.join("notes.txt"), "one\ntwo\n");
    run_crlf_git(&repo, &["commit", "-q", "-am", "two"]);
    run_crlf_git(
        &repo,
        &[
            "push",
            "-q",
            root.join("aterm.git").to_str().unwrap(),
            "main",
        ],
    );
    let source = run_crlf_git(&repo, &["rev-parse", "HEAD"]);
    let moved = place(&source);
    assert!(moved.moved);
    lf(&moved.tree, "one\ntwo\n", true);
    let plan = crate::ledger::ClaimPlan {
        version: "0.26.0",
        now: 0,
        allow_existing_section: false,
        max_attempts: 1,
        source: &source,
        harness_base: None,
    };
    let cut = GitCli::new(&moved.tree);
    bind_native_engine_fixture(&moved.tree, &root.join("aterm.git"));
    let native = native_engine_fixture(&moved.tree, &moved.tree).unwrap();
    let reserved = crate::ledger::reserve(&cut, &native, &plan, "2026-10-21").unwrap();
    crate::ledger::claim(&cut, &native, &moved.tree, &plan, &reserved).unwrap();
    let rolled = fs::read_to_string(moved.tree.join("CHANGELOG.md")).unwrap();
    assert!(
        rolled.contains("## [0.26.0]") && !rolled.contains('\r'),
        "{rolled:?}"
    );
    true
}

/// Every entry under `root` and its bytes, symlinks not followed: what a probe writes grows it.
fn tree_bytes(root: &Path) -> u64 {
    let mut total = 0;
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            total += 1 + meta.len();
            if meta.is_dir() {
                dirs.push(entry.path());
            }
        }
    }
    total
}

#[test]
fn every_tree_the_release_writes_is_the_commits_bytes() {
    if probe_every_tree_the_release_writes_under_a_crlf_config_if_asked() {
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let config = root.join("gitconfig");
    std::fs::write(
        &config,
        "[core]\n\tautocrlf = true\n\tsafecrlf = true\n\teol = crlf\n[protocol \"file\"]\n\tallow = always\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("xdg/git")).unwrap();
    std::fs::write(root.join("xdg/git/attributes"), "* text eol=crlf\n").unwrap();
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    for leaf in ["home", "tmp", "state"] {
        std::fs::create_dir(root.join(leaf)).unwrap();
    }
    let mut child = command(std::env::current_exe().unwrap());
    aterm_uds::fixture_git::hermetic(&mut child)
        .args(["--exact", "--nocapture", "--test-threads=1", CRLF_PROBE])
        .env(CRLF_PROBE_ENV, &work)
        .env("HOME", root.join("home"))
        .env("TMPDIR", root.join("tmp"))
        .env("TMP", root.join("tmp"))
        .env("TEMP", root.join("tmp"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env(crate::ledger::testing::CHOSEN_GIT_ENV, "1")
        .stdin(Stdio::null());
    // The probe is a whole release's git work: the hang detector is a minute in which its
    // fixture tree stops growing, not a minute for all of it.
    let mut grown = || tree_bytes(&work);
    let out = aterm_uds::deadline::output_while_progressing(
        &mut child,
        aterm_spec::wait::MUST_HAPPEN,
        &mut grown,
    );
    let out = match out {
        Ok(out) => out,
        Err(error) => {
            let kept = dir.keep();
            panic!(
                "CRLF probe: {error} (its fixture tree stopped growing); retained fixture {}",
                kept.display()
            );
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the probe under a CRLF config: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_refusal_outranks_a_gate_that_could_not_run() {
    let gate = |outcome: Outcome| PrecheckGate { name: "g", outcome };
    let pass = || gate(Outcome::Pass("ok".into()));
    let refuse = || gate(Outcome::Refuse(Refusal::new("no", "fix")));
    let not_run = || gate(Outcome::CouldNotRun(Refusal::new("no tool", "install")));
    for (gates, code) in [
        (vec![pass()], 0),
        (vec![pass(), not_run()], 3),
        (vec![not_run(), refuse(), pass()], 1),
        (vec![refuse(), not_run()], 1),
    ] {
        let found = Precheck {
            commit: "c".repeat(40),
            version: "0.2.0".into(),
            gates,
        };
        assert_eq!(found.exit_code(), code);
    }
}

#[test]
fn the_engines_ledger_and_attestation_are_read_as_the_release_reads_them() {
    let a = "a".repeat(40);
    let b = "b".repeat(40);
    let json = format!(
        "{{\"mappings\":{{\"aterm\":{{\"{a}\":{{\"verified_at\":\"2026-10-01T00:00:00Z\",\
         \"release_rev\":\"e1\"}},\"{b}\":{{\"verified_at\":\"2026-10-02T00:00:00Z\"}}}}}}}}"
    );
    let rows = engine_rows_in(&json, Some(&a));
    assert_eq!(
        rows,
        EngineRows {
            newest: Some(b.clone()),
            has: true,
            rev: Some("e1".into())
        }
    );
    assert!(!engine_rows_in(&json, Some("c")).has);
    assert_eq!(engine_rows_in("not json", None), EngineRows::default());
    // A revoked row is no publication of its commit, nor the newest. NEGATIVE CONTROL: the
    // same rows unrevoked (above) have `a` published and `b` newest.
    let revoked = format!(
        "{}, \"revoked\":{{\"aterm\":{{\"{b}\":{{}},\"{a}\":{{}}}}}}}}",
        json.strip_suffix('}').unwrap()
    );
    assert_eq!(engine_rows_in(&revoked, Some(&a)), EngineRows::default());
    let attestation = |gate: &str, mode: &str| {
        format!(
            "{{\"source\":{{\"commit\":\"{a}\"}},\"guards\":{{\"public_clone\":\"{gate}\"}},\
             \"release_mode\":\"{mode}\",\"engine\":{{\"commit\":\"{b}\"}},\
             \"staging\":{{\"candidate_rev\":\"{b}\",\"attested_rev\":\"{b}\"}},\
             \"export\":{{\"tree\":\"{a}\"}}}}"
        )
    };
    assert!(attestation_promotes(&attestation("pass", "full"), &a));
    assert!(attestation_promotes(
        &attestation("skipped", "source-only"),
        &a
    ));
    assert!(!attestation_promotes(&attestation("skipped", "full"), &a));
    assert!(!attestation_promotes(&attestation("pass", "full"), &b));
    assert_eq!(
        attested_engine(&attestation("pass", "full")).as_deref(),
        Some(b.as_str())
    );
    let engineless =
        attestation("pass", "full").replace(&format!(",\"engine\":{{\"commit\":\"{b}\"}}"), "");
    assert_eq!(attested_engine(&engineless), None);
    assert!(!attestation_promotes(&engineless, &a));
    let private = attestation("pass", "full").replace(
        &format!("\"attested_rev\":\"{b}\""),
        "\"attested_rev\":null",
    );
    assert!(attestation_qualifies(&private, &a));
    assert!(!attestation_promotes(&private, &a));
    for invalid in [
        private.replace(
            &format!("\"candidate_rev\":\"{b}\""),
            "\"candidate_rev\":null",
        ),
        private.replace(&format!("\"tree\":\"{a}\""), "\"tree\":null"),
        private.replace("\"public_clone\":\"pass\"", "\"public_clone\":\"skipped\""),
    ] {
        assert!(!attestation_qualifies(&invalid, &a), "{invalid}");
        assert!(!attestation_promotes(&invalid, &a), "{invalid}");
    }
    let mismatch = attestation("pass", "full").replace(
        &format!("\"attested_rev\":\"{b}\""),
        &format!("\"attested_rev\":\"{a}\""),
    );
    assert!(!attestation_promotes(&mismatch, &a));
}

#[test]
fn the_qualified_candidate_requires_both_commit_and_tree() {
    let (s, t) = ("5".repeat(40), "6".repeat(40));
    let json =
        format!("{{\"staging\":{{\"attested_rev\":\"{s}\"}},\"export\":{{\"tree\":\"{t}\"}}}}");
    assert_eq!(attested_candidate(&json), Some((s.clone(), t.clone())));
    assert_eq!(
        attested_candidate("{\"staging\":{\"attested_rev\":\"r\"}}"),
        None
    );
    assert_eq!(attested_candidate("not json"), None);
}

#[test]
fn retained_candidate_is_checked_locally_and_missing_evidence_is_not_a_pass() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let git = GitCli::new(dir.path());
    let run = |args: &[&str]| {
        let out = git.git(args).unwrap();
        assert!(out.success(), "{}", String::from_utf8_lossy(&out.stderr));
        out.stdout_utf8().trim().to_string()
    };
    run(&["init", "-q", "-b", "main"]);
    run(&[
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--allow-empty",
        "-qm",
        "qualified",
    ]);
    let revision = run(&["rev-parse", "HEAD"]);
    let tree = run(&["rev-parse", "HEAD^{tree}"]);
    let reference = format!("refs/publication/candidates/{revision}");
    run(&["update-ref", &reference, &revision]);
    let out = dir.path().join("publish/.out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(
        out.join("attestation.json"),
        format!(
            "{{\"staging\":{{\"attested_rev\":\"{revision}\"}},\"export\":{{\"tree\":\"{tree}\"}}}}"
        ),
    )
    .unwrap();
    let mut real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
    assert_eq!(real.candidate_missing().unwrap(), None);
    run(&["update-ref", "-d", "refs/heads/main"]);
    assert_eq!(real.candidate_missing().unwrap(), None);
    run(&["update-ref", "-d", &reference]);
    assert!(real.candidate_missing().unwrap().is_some());
    std::fs::rename(dir.path().join(".git"), dir.path().join("missing-git")).unwrap();
    assert!(real.candidate_missing().is_err());
}

#[test]
fn dry_run_machine_checks_refuse_an_incomplete_source_signing_ring() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
    real.home = dir.path().join("empty-home");
    let (_, refusals) = real.machine(&real.home.join("profile.toml"), false);
    assert!(
        refusals
            .iter()
            .any(|r| r.why.starts_with("source signing ring:"))
    );
}

/// The build and cut steps run this very executable or nothing: a cutter that cannot name
/// its own executable builds no command for an `aterm-release` found on `PATH`, which would
/// be handed nothing of this one; one that can runs that path, handed itself. NEGATIVE
/// CONTROL: fall back to `aterm-release` on `PATH` (as before), and a command comes back.
#[test]
fn a_cutter_that_cannot_name_itself_starts_no_step() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = Real::new(
        dir.path(),
        Some(Path::new("/other/publication")),
        true,
        false,
    )
    .expect("a backend");
    let prover = Prover {
        bin: PathBuf::from("/store/trust/9192/bin"),
        commit: "321aaeda7".into(),
        source: "the store".into(),
    };
    let args = ["cut".to_string()];
    let gone = real.cut_command_of(Err(std::io::Error::other("gone")), &args, &prover);
    assert_eq!(
        gone.err().as_deref(),
        Some("this cutter cannot name its own executable (gone)")
    );
    let exe = PathBuf::from("/t/gate/aterm-release");
    let cmd = real
        .cut_command_of(Ok(exe.clone()), &args, &prover)
        .unwrap();
    assert_eq!(cmd.get_program(), exe.as_os_str());
    assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["cut"]);
    assert!(cmd.get_envs().any(|kv| kv
        == (
            std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
            Some(exe.as_os_str())
        )));
}

#[test]
fn the_engine_the_release_judges_is_the_one_every_child_reads() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(
        dir.path(),
        Some(Path::new("/other/publication")),
        true,
        false,
    )
    .expect("a backend");
    let prover = Prover {
        bin: PathBuf::from("/store/trust/9192/bin"),
        commit: "321aaeda7".into(),
        source: "the store".into(),
    };
    let engine = (
        std::ffi::OsStr::new("PUBLICATION_ENGINE"),
        Some(std::ffi::OsStr::new("/other/publication")),
    );
    let cut = real
        .cut_command(&["cut".into(), "--mac-only".into()], &prover)
        .unwrap();
    let envs: Vec<_> = cut.get_envs().collect();
    assert!(envs.contains(&engine), "{envs:?}");
    let exe = std::env::current_exe().unwrap();
    // The cut step's cutter is this executable, handed itself, so its cut gate judges it
    // from this process's moment; never judged at its start, this test process has none.
    for handed in [
        (RELEASE_CUTTER_ENV, Some(exe.as_os_str())),
        (RELEASE_CUTTER_SINCE_ENV, None),
    ] {
        assert!(
            envs.contains(&(std::ffi::OsStr::new(handed.0), handed.1)),
            "{handed:?}: {envs:?}"
        );
    }
    assert!(
        envs.contains(&(
            std::ffi::OsStr::new("TRUST_STAGE2_BIN"),
            Some(std::ffi::OsStr::new("/store/trust/9192/bin"))
        )),
        "{envs:?}"
    );
    let stage = real.child("/other/publication/bin/pub", Some(&prover));
    assert!(stage.get_envs().any(|kv| kv == engine));
    real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
    let publish = real.publish_command(&prover).unwrap();
    assert_eq!(
        publish.get_program(),
        std::ffi::OsStr::new("/other/publication/bin/pub")
    );
    assert!(
        publish.get_args().any(|a| a == "--skip-site"),
        "the site follows the cut, not the promote"
    );
    let args: Vec<_> = publish.get_args().collect();
    assert!(args.windows(2).any(|pair| {
        pair == [
            "--release-remote",
            "https://github.com/alabsystems/aterm.git",
        ]
    }));
    let envs: Vec<_> = publish.get_envs().collect();
    assert!(envs.contains(&engine), "{envs:?}");
    assert!(
        envs.contains(&(
            std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
            Some(exe.as_os_str())
        )),
        "{envs:?}"
    );
    // Never judged at its start, this test process has no moment to hand on with itself.
    assert!(
        envs.contains(&(std::ffi::OsStr::new(RELEASE_CUTTER_SINCE_ENV), None)),
        "{envs:?}"
    );
    let edit = real.scrubbed("/other/publication/bin/pub-bump-edit");
    assert!(edit.get_envs().any(|kv| kv == engine));
}

#[cfg(unix)]
/// EVERY CHILD THE RELEASE STARTS IS NAMED ITS DISK CLAIM WHILE IT HOLDS ONE: a `verify` it
/// runs is covered by that claim alone (`aterm_capacity::covering_disk`), walking nothing. One
/// this process inherited is never passed on, and once the claim is let go no child is named
/// it. NEGATIVE CONTROL: before the claim, the same child carries no marker.
#[test]
fn every_child_is_named_the_releases_disk_claim_while_it_holds_one() {
    let gib = 1u64 << 30;
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
    // `Real::new` keeps its record below this test binary's own root (`release_state`),
    // which every test in this process shares; a record of its own keeps "a run that did
    // not pass keeps nothing" below independent of them.
    real.state = dir.path().join("state");
    let marker = |cmd: &Command| {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(aterm_capacity::DISK_COVER_ENV))
            .map(|(_, v)| v.map(std::ffi::OsStr::to_owned))
    };
    let mut inherited = command("/bin/true");
    scrub_names(
        &mut inherited,
        [std::ffi::OsString::from(aterm_capacity::DISK_COVER_ENV)],
    );
    assert_eq!(
        marker(&inherited),
        Some(None),
        "an inherited claim is removed"
    );
    assert_eq!(
        marker(&real.child("/bin/true", None)),
        None,
        "no claim, no marker"
    );

    let mut reading = aterm_capacity::Reading::of_gib(48, 40, 14);
    reading.volume = Some(aterm_capacity::Volume {
        mount: dir.path().to_path_buf(),
        total: 400 * gib,
        free: 300 * gib,
        floor: 10 * gib,
        room: 2 * gib,
    });
    let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
    let whole = atpkg::admit_disk::admit_sized_on(
        crate::release_disk::sized(dir.path(), 39 * gib, "release v0.6.0", Vec::new()),
        &ledger,
        &aterm_capacity::Fixed::new(reading),
        atpkg::admit_disk::Admit::Wait,
        aterm_capacity::Pace::default(),
        &mut atpkg::admit_disk::Reclaim::of(
            Box::new(|_, _| (0, Vec::new(), Vec::new())),
            Box::new(|_, _, _| None),
            Box::new(|_, _| Vec::new()),
        ),
        &mut |_| {},
    )
    .expect("room for the claim");
    let name = std::ffi::OsString::from(whole.marker());
    real.disk_claim = Some(whole);
    assert_eq!(marker(&real.child("/bin/true", None)), Some(Some(name)));

    real.let_disk_go(false);
    assert!(real.disk_claim.is_none());
    assert_eq!(marker(&real.child("/bin/true", None)), None, "let go");
    assert!(
        !crate::release_disk::record_in(&real.state).exists(),
        "a run that did not pass keeps nothing"
    );
}

#[test]
fn scanner_export_preflight_uses_the_actual_dry_run_environment() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = root.join("candidate");
    let export = root.join("export-candidate");
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(&export).unwrap();
    let mut real = Real::new(&repo, Some(&root.join("engine")), true, false).unwrap();
    real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
    let (mut stage, _) = real
        .dry_run_command(
            &root.join("scratch-engine"),
            "candidate",
            &export,
            &public_reads_at(0),
            &root.join("agh"),
        )
        .unwrap();
    let report = scanner_tools::prepare(
        &stage,
        real.engine_prefix_len(),
        &real.logs.join("export-scanner-tools.json"),
        &[&repo, &export],
        |probe| {
            assert_eq!(probe.get_program(), stage.get_program());
            assert_eq!(
                probe.get_envs().collect::<Vec<_>>(),
                stage.get_envs().collect::<Vec<_>>()
            );
            assert_eq!(
                probe.get_args().collect::<Vec<_>>(),
                [std::ffi::OsStr::new("scanner-preflight")]
            );
            Ok(scanner_tools::test_output())
        },
    )
    .unwrap();
    report.apply(&mut stage);
    assert!(
        stage
            .get_envs()
            .any(|(name, value)| name == scanner_tools::ENV && value.is_some())
    );
    assert_eq!(std::fs::read_dir(repo).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(export).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn scanner_failures_stop_the_export_and_its_tools_reach_only_the_engine() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    crate::ledger::testing::init(
        &repo,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    let git = GitCli::new(&repo);
    git.git(&["commit", "--allow-empty", "-qm", "candidate"])
        .unwrap();
    let candidate = Candidate {
        commit: git
            .git(&["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim()
            .into(),
        version: "1.0.0".into(),
        why: "test".into(),
        bump_from: None,
        carry: Vec::new(),
    };
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink("/usr/bin/false", bin.join("targo")).unwrap();
    let prover = Prover {
        bin: bin.clone(),
        commit: "test".into(),
        source: "test".into(),
    };
    // An engine clone with no bin/pub, synced from its origin before the export runs.
    let engine = dir.path().join("engine");
    let origin = dir.path().join("engine-origin.git");
    crate::ledger::testing::init(
        &engine,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    let engine_git = GitCli::new(&engine);
    engine_git
        .git(&["commit", "--allow-empty", "-qm", "engine"])
        .unwrap();
    GitCli::new(dir.path())
        .git(&["clone", "-q", "--bare", "engine", "engine-origin.git"])
        .unwrap();
    engine_git
        .git(&["remote", "add", "origin", &origin.display().to_string()])
        .unwrap();
    bind_native_engine_fixture(&engine, &origin);
    // A real run's backend (a dry one makes no engine to stage from), its logs its own.
    let backend = |engine: &Path, logs: &str| {
        let mut real = Real::new(&repo, Some(engine), false, false).unwrap();
        real.engine_origin = native_engine_fixture;
        real.engine_url = fixture_engine_url;
        real.logs = dir.path().join(logs);
        std::fs::create_dir_all(&real.logs).unwrap();
        real
    };
    let mut real = backend(&engine, "logs");
    real.home = dir.path().join("empty-home");
    real.prerequisite_acquire = crate::managed_prerequisites::simulated;
    let mut missing = backend(&dir.path().join("missing-engine"), "logs-missing");
    let unsynced =
        Backend::stage(&mut missing, &candidate, &prover, StageTarget::Remote).unwrap_err();
    assert!(
        unsynced.why.starts_with("no publication engine at"),
        "the engine is synced before the export: {unsynced:?}"
    );
    let scanner_failure =
        Backend::stage(&mut real, &candidate, &prover, StageTarget::Remote).unwrap_err();
    assert!(
        scanner_failure
            .why
            .starts_with("publication scanner preflight:")
    );
    assert!(real.scanner_tools.is_none());
    real.scanner_probe = |_| Ok(scanner_tools::test_output());
    let ran = Backend::stage(&mut real, &candidate, &prover, StageTarget::Remote).unwrap();
    assert_ne!(ran.code, 0, "the engine has no bin/pub: {ran:?}");
    let report = real.logs.canonicalize().unwrap().join("scanner-tools.json");
    for cmd in [
        real.with_scanners(
            real.stage_command(&candidate.commit, &prover, StageTarget::Remote)
                .unwrap()
                .0,
        ),
        real.publish_command(&prover).unwrap(),
    ] {
        assert!(
            cmd.get_envs().any(
                |(name, value)| name == scanner_tools::ENV && value == Some(report.as_os_str())
            )
        );
    }
    // Negative control: no other child of the run is handed them (the build's, verify's);
    // each has the name removed, so none inherits one either.
    assert!(
        real.child(prover.bin.join("targo"), Some(&prover))
            .get_envs()
            .any(|(name, value)| name == scanner_tools::ENV && value.is_none())
    );
    // The stage outside the graph (step 7's, again) is that step's child, in its log; the
    // graph's export is the export branch, in its own.
    let logged = |path: PathBuf| std::fs::read_to_string(path).unwrap_or_default();
    let branch_log = real.side_log(Step::Qualify, Branch::Export.name());
    assert!(
        logged(real.step_log(Step::Publish)).contains("\"stage\""),
        "the stage again logs as step 7's child"
    );
    assert!(!branch_log.exists(), "and not as step 4's export");
    let ran = Backend::branches(&mut real)
        .export(
            &candidate,
            &prover,
            StageTarget::Remote,
            &BranchStop::default(),
        )
        .unwrap();
    assert_ne!(ran.code, 0, "{ran:?}");
    assert!(logged(branch_log).contains("\"stage\""));
    // An engine that cannot be synced after the graph's export says nothing of R: COULD NOT
    // RUN, so it stops no other branch. NEGATIVE CONTROL: the same refusal outside the graph
    // stays a refusal.
    std::fs::rename(&origin, dir.path().join("gone.git")).unwrap();
    let unsynced = Backend::branches(&mut real)
        .export(
            &candidate,
            &prover,
            StageTarget::Remote,
            &BranchStop::default(),
        )
        .unwrap_err();
    assert!(
        unsynced.why.starts_with("cannot fetch the engine's origin") && unsynced.could_not_run,
        "{unsynced:?}"
    );
    let refused = Backend::stage(&mut real, &candidate, &prover, StageTarget::Remote).unwrap_err();
    assert!(
        refused.why.starts_with("cannot fetch the engine's origin") && !refused.could_not_run,
        "{refused:?}"
    );
    std::fs::rename(dir.path().join("gone.git"), &origin).unwrap();
    let failures: [scanner_tools::Probe; 3] = [
        |_| Err("engine missing or probe timed out".into()),
        |_| {
            let mut out = scanner_tools::test_output();
            out.stdout = b"unsupported command help".to_vec();
            Ok(out)
        },
        |_| {
            use std::os::unix::process::ExitStatusExt as _;
            let mut out = scanner_tools::test_output();
            out.status = std::process::ExitStatus::from_raw(1 << 8);
            out.stderr = b"zstd unavailable: restore the supported managed zstd tool".to_vec();
            Ok(out)
        },
    ];
    for (index, probe) in failures.into_iter().enumerate() {
        real.scanner_probe = probe;
        let refusal =
            Backend::stage(&mut real, &candidate, &prover, StageTarget::Remote).unwrap_err();
        assert!(refusal.why.starts_with("publication scanner preflight:"));
        if index == 2 {
            assert!(
                refusal
                    .detail
                    .iter()
                    .any(|line| line.contains("zstd unavailable")
                        && line.contains("supported managed zstd"))
            );
        }
        assert!(real.scanner_tools.is_none());
        assert!(
            real.publish_command(&prover)
                .unwrap()
                .get_envs()
                .any(|(name, value)| name == scanner_tools::ENV && value.is_none())
        );
    }
}

/// The fixture generator compiles the clone's working tree, so a clone off the candidate's
/// tree is refused before it runs, in the build branch as in the after-cut step.
#[cfg(unix)]
#[test]
fn the_fixture_generator_refuses_a_clone_off_the_candidates_tree() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    crate::ledger::testing::init(
        &repo,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    let git = GitCli::new(&repo);
    git.git(&["commit", "--allow-empty", "-qm", "candidate"])
        .unwrap();
    let candidate = Candidate {
        commit: git
            .git(&["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim()
            .into(),
        version: "1.0.0".into(),
        why: "test".into(),
        bump_from: None,
        carry: Vec::new(),
    };
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink("/usr/bin/false", bin.join("targo")).unwrap();
    let prover = Prover {
        bin,
        commit: "test".into(),
        source: "test".into(),
    };
    let real = Real::new(&repo, Some(&dir.path().join("missing-engine")), true, false).unwrap();
    let log = dir.path().join("fixtures.log");
    let off_tree = |refusal: &Refusal| refusal.why.contains("tree and clean");
    let clean = real.stage_fixtures_run(
        Step::Qualify,
        &candidate,
        &Reservation::default(),
        &prover,
        &log,
    );
    assert!(!clean.as_ref().is_err_and(off_tree), "{clean:?}");
    std::fs::write(repo.join("untracked"), "changed").unwrap();
    let dirty = real
        .stage_fixtures_run(
            Step::Qualify,
            &candidate,
            &Reservation::default(),
            &prover,
            &log,
        )
        .unwrap_err();
    assert!(off_tree(&dirty), "{dirty:?}");
    assert!(
        dirty.detail.iter().any(|line| line.contains("untracked")),
        "{dirty:?}"
    );
}

/// The stage step's `pub stage` is a child with the prover's bin, handed this cutter only as
/// the commit's own build: this test process, never judged at its start, hands none, and
/// removes any it inherited. NEGATIVE CONTROL: build the command without
/// [`hand_transforms_cutter`], and neither variable is removed.
#[test]
fn the_stage_step_is_handed_this_cutter_only_as_the_commits_own_build() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(
        dir.path(),
        Some(Path::new("/other/publication")),
        true,
        false,
    )
    .expect("a backend");
    let prover = Prover {
        bin: PathBuf::from("/store/trust/9192/bin"),
        commit: "321aaeda7".into(),
        source: "the store".into(),
    };
    let commit = "c".repeat(40);
    real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
    let (stage, handed) = real
        .stage_command(&commit, &prover, StageTarget::Remote)
        .unwrap();
    assert_eq!(
        stage.get_program(),
        std::ffi::OsStr::new("/other/publication/bin/pub")
    );
    let args: Vec<_> = stage.get_args().collect();
    let repo = dir.path().as_os_str();
    assert_eq!(
        args,
        ["stage", "aterm", "--commit", commit.as_str(), "--repo-path"]
            .map(std::ffi::OsStr::new)
            .into_iter()
            .chain([repo])
            .collect::<Vec<_>>()
    );
    let local = real
        .stage_command(&commit, &prover, StageTarget::Local)
        .unwrap()
        .0;
    let local_args: Vec<_> = local.get_args().collect();
    let mut expected_local = args.clone();
    expected_local.push(std::ffi::OsStr::new("--dry-run"));
    assert_eq!(local_args, expected_local);
    assert!(!local_args.contains(&std::ffi::OsStr::new("--no-check")));
    let envs: Vec<_> = stage.get_envs().collect();
    for kv in [
        ("PUBLICATION_ENGINE", Some("/other/publication")),
        ("TRUST_STAGE2_BIN", Some("/store/trust/9192/bin")),
        (STAGE_APPEND_ONLY_ENV, Some("1")),
        (RELEASE_CUTTER_ENV, None),
        (RELEASE_CUTTER_SINCE_ENV, None),
    ] {
        let want = (std::ffi::OsStr::new(kv.0), kv.1.map(std::ffi::OsStr::new));
        assert!(envs.contains(&want), "{kv:?}: {envs:?}");
    }
    // Both stage commands stage append-only: the engine's GitHub transport has no forced
    // write, so the legacy orphan-snapshot replacement refuses at the staging push.
    let local_envs: Vec<_> = local.get_envs().collect();
    let append_only = (
        std::ffi::OsStr::new(STAGE_APPEND_ONLY_ENV),
        Some(std::ffi::OsStr::new("1")),
    );
    assert!(local_envs.contains(&append_only), "{local_envs:?}");
    assert!(
        handed.contains("no commit's build")
            && handed.ends_with("ccccccccc's publish/transforms.sh builds its own"),
        "{handed}"
    );
}

/// Frozen tools share verify's main build directory, and only the test-profile verifier
/// widens to the workspace test graph. NEGATIVE CONTROL: applying those words to the gate
/// cutter would change its compiled feature set.
#[test]
fn frozen_tools_use_the_main_layout_and_only_verify_uses_the_test_graph() {
    let repo = Path::new("/release");
    let layout = "build.build-dir='/cache/build.noindex'";
    let words = |args: Vec<std::ffi::OsString>| {
        args.into_iter()
            .map(|word| word.into_string().unwrap())
            .collect::<Vec<_>>()
    };
    let cutter = words(Real::frozen_tool_args(
        repo,
        &FROZEN_CUTTER,
        "gate",
        Some(layout),
    ));
    let verify = words(Real::frozen_tool_args(
        repo,
        &FROZEN_VERIFY,
        "test",
        Some(layout),
    ));
    assert_eq!(&cutter[..4], ["--config", layout, "--unverified", "build"]);
    assert!(
        !cutter
            .iter()
            .any(|word| word == aterm_toolchain::build_cache::TEST_GRAPH_FLAG)
    );
    let [config, workspace] = aterm_toolchain::build_cache::TEST_GRAPH_CONFIG;
    assert_eq!(
        &verify[..7],
        [
            "--config",
            layout,
            "--unverified",
            config,
            workspace,
            "build",
            aterm_toolchain::build_cache::TEST_GRAPH_FLAG
        ]
    );
    for (args, package, profile) in [
        (&cutter, FROZEN_CUTTER.package, "gate"),
        (&verify, FROZEN_VERIFY.package, "test"),
    ] {
        assert!(args.windows(2).any(|pair| pair == ["--profile", profile]));
        assert!(args.windows(2).any(|pair| pair == ["-p", package]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--manifest-path", "/release/Cargo.toml"])
        );
    }
}

/// A Cargo test binary's release keeps its state, its logs included, below that binary's own
/// root ([`release_state`]), never below the person's HOME, whose logs, step times, footprints
/// and kept seal the next real release reads (m3's logs held empty run folders this crate's
/// tests had made). NEGATIVE CONTROL: resolve the state from HOME alone, and this run's log
/// folder is made below the person's.
#[test]
fn a_test_binarys_release_keeps_its_state_below_its_own_root() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = Real::new(dir.path(), Some(dir.path()), false, false).expect("a backend");
    let root = aterm_uds::test_binary::scratch_root().expect("a test binary's root");
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME in tests"));
    assert_eq!(real.state, root.join(RELEASE_STATE));
    assert!(
        real.logs.starts_with(&real.state) && real.logs.is_dir(),
        "{}",
        real.logs.display()
    );
    assert!(
        real.logs
            .starts_with(real.state.join(aterm_tempfile::QUIET).join("logs.noindex"))
    );
    assert!(
        !real.logs.starts_with(home.join(RELEASE_STATE)),
        "{}",
        real.logs.display()
    );
}

/// On macOS a dry run's logs go in its temp root's quiet folder, which fseventsd drops, even
/// where that root is heard; elsewhere nothing drops a file event by its path, so they go in
/// the root itself ([`aterm_tempfile::quiet_in`]).
#[test]
fn dry_run_logs_go_quiet_on_macos_even_when_the_callers_temp_root_is_heard() {
    let root = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
    assert!(!aterm_tempfile::is_quiet(root.path()));
    let logs = dry_log_dir(root.path(), 42, 7);
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("operational-status.json"), b"{}").unwrap();
    let macos = cfg!(target_os = "macos");
    assert_eq!(aterm_tempfile::is_quiet(&logs), macos);
    assert!(logs.ends_with("aterm-release-dry-42-7.noindex"));
    assert_eq!(
        root.path().join("aterm-release-dry-42-7.noindex").exists(),
        !macos
    );
}

/// A handed cutter carries the moment it was judged from, with this process as the one that
/// handed it, as the cutter it hands reads it, or none: an inherited one is removed.
/// NEGATIVE CONTROL: leave the variable alone when there is no moment, and an inherited one
/// reaches the handed cutter.
#[test]
fn a_handed_cutter_carries_the_moment_it_was_judged_from() {
    let exe = Path::new("/t/gate/aterm-release");
    let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::new(1_793_520_000, 7);
    let since = |cmd: &Command| {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(RELEASE_CUTTER_SINCE_ENV))
            .map(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
    };
    let mut cmd = command("/other/publication/bin/pub");
    hand_this_cutter(&mut cmd, exe, Some(at));
    let value = crate::cutter::hand_value(at);
    #[cfg(target_os = "macos")]
    assert!(
        value.as_deref().is_some_and(
            |v| v.starts_with(&format!("1793520000000000007 {} ", std::process::id()))
        ),
        "the moment and this process's pid, in nanoseconds: {value:?}"
    );
    assert_eq!(
        since(&cmd),
        Some(value.map(Into::into)),
        "handed by this process"
    );
    assert!(cmd.get_envs().any(|kv| kv
        == (
            std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
            Some(exe.as_os_str())
        )));
    let mut cmd = command("/other/publication/bin/pub");
    hand_this_cutter(&mut cmd, exe, None);
    assert_eq!(since(&cmd), Some(None), "none, and none inherited");
}

/// A step runs without the cutter or the build-inputs seal this process may have been
/// handed: a child has [`RELEASE_CUTTER_ENV`] and [`RELEASE_CUTTER_SINCE_ENV`] only from
/// [`hand_this_cutter`] and [`crate::build_inputs::SEAL_ENV`] only from
/// [`hand_inputs_seal`] (this process's kept seal, else none), never inherited, and keeps
/// what the scrub does not name. NEGATIVE CONTROL: scrub only what [`clears`] and
/// [`launcher_env`] name, and all three reach the child.
#[test]
fn a_step_inherits_no_handed_cutter() {
    let seal = crate::build_inputs::SEAL_ENV;
    let mut cmd = command("/usr/bin/true");
    let inherited = [
        RELEASE_CUTTER_ENV,
        RELEASE_CUTTER_SINCE_ENV,
        seal,
        "HOME",
        "RUSTFLAGS",
    ];
    scrub_names(&mut cmd, inherited.map(std::ffi::OsString::from));
    let removed: Vec<_> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_os_string())
        .collect();
    assert_eq!(
        removed,
        [
            RELEASE_CUTTER_ENV,
            RELEASE_CUTTER_SINCE_ENV,
            seal,
            "RUSTFLAGS"
        ]
    );
    hand_this_cutter(&mut cmd, Path::new("/t/gate/aterm-release"), None);
    assert!(cmd.get_envs().any(|kv| kv
        == (
            std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
            Some(std::ffi::OsStr::new("/t/gate/aterm-release"))
        )));
    let handed = |cmd: &Command| {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(seal))
            .map(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
    };
    hand_inputs_seal(&mut cmd, None);
    assert_eq!(handed(&cmd), Some(None), "none kept, none handed");
    let kept = PathBuf::from("/state/aterm-release/inputs-seal/k.toml");
    hand_inputs_seal(&mut cmd, Some(kept.clone()));
    assert_eq!(handed(&cmd), Some(Some(kept.into_os_string())));
}

/// A RELEASE'S OWN FROZEN CUTTER TAKES THE ENGINE SOURCE AND REPOSITORY THE RELEASE CAPTURED:
/// the exec hands both ([`hand_engine_capture`]) beside the release's copy as
/// `PUBLICATION_ENGINE`, the engine a cutter older than the handover syncs, never the source;
/// no other child inherits them ([`scrub_names`]); and the new image (this test binary run with
/// the exec's engine handover, `Real::new` reading its environment) takes the release's source
/// and holds to its repository ([`engine_authority`]), and hands both to the next image it
/// would exec. A source that names another repository since the capture is refused, as is a
/// handover from a release that admitted none, with that release's own reason, and either
/// refusal is handed on unchanged ([`ENGINE_REPOSITORY_NONE`], [`ENGINE_REPOSITORY_WHY_ENV`]),
/// so no later hop adopts what the copy names. NEGATIVE CONTROL: the exec of a release older
/// than the handover, which names only the copy, has the image take the copy as its source and
/// hold to the other repository the copy names.
#[test]
fn a_releases_frozen_cutter_takes_the_engine_source_and_repository_it_captured() {
    const PROBE: &str = "ATERM_RELEASE_ENGINE_CAPTURE_PROBE";
    const HANDOVER: [&str; 4] = [
        "PUBLICATION_ENGINE",
        ENGINE_SOURCE_ENV,
        ENGINE_REPOSITORY_ENV,
        ENGINE_REPOSITORY_WHY_ENV,
    ];
    let named = |cmd: &Command, name: &str| {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(name))
            .map(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
    };
    if let Some(repo) = std::env::var_os(PROBE) {
        let real = Real::new(Path::new(&repo), None, true, false).unwrap();
        let next = real.cutter_command(Path::new("/usr/bin/true"));
        let next = |name: &str| {
            named(&next, name)
                .flatten()
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        println!(
            "PROBE source={} repository={} next_source={} next_repository={} next_why={}",
            real.engine_source.display(),
            real.engine_repository
                .as_ref()
                .map_or_else(|why| format!("refused: {why}"), ToString::to_string),
            next(ENGINE_SOURCE_ENV),
            next(ENGINE_REPOSITORY_ENV),
            next(ENGINE_REPOSITORY_WHY_ENV),
        );
        // A dry run removes its scratch logs when it drops, and `exit` runs no destructor.
        drop(real);
        std::process::exit(0);
    }
    let slug = |s: &str| agh::Slug::parse(s).unwrap();
    let handed = |s: &str| Some(std::ffi::OsString::from(s));
    assert_eq!(
        engine_authority(Ok(slug("foreign/app")), handed(""), handed("no origin")),
        Ok(slug("foreign/app"))
    );
    assert_eq!(
        engine_authority(
            Ok(slug("fixture/app")),
            handed(ENGINE_REPOSITORY_NONE),
            handed("no origin")
        ),
        Err("no origin".to_owned()),
        "a release that admitted none is refused for its own reason"
    );
    for refused in [
        engine_authority(Ok(slug("foreign/app")), handed("fixture/app"), None),
        engine_authority(Err("no origin".into()), handed("fixture/app"), None),
        engine_authority(Ok(slug("fixture/app")), handed("not one repository"), None),
        engine_authority(
            Ok(slug("fixture/app")),
            handed(ENGINE_REPOSITORY_NONE),
            None,
        ),
        engine_authority(
            Ok(slug("fixture/app")),
            handed(ENGINE_REPOSITORY_NONE),
            handed(""),
        ),
    ] {
        assert!(refused.is_err(), "{refused:?}");
    }
    let mut inherited = command("/usr/bin/true");
    let captured = &HANDOVER[1..];
    scrub_names(
        &mut inherited,
        captured.iter().map(std::ffi::OsString::from),
    );
    for name in captured {
        assert_eq!(
            named(&inherited, name),
            Some(None),
            "{name} is not inherited"
        );
    }

    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let checkout = |name: &str, url: &str| {
        let at = root.join(name);
        crate::ledger::testing::init(&at, &[]);
        crate::ledger::git_ok(&GitCli::new(&at), &["remote", "add", "origin", url]).unwrap();
        at
    };
    let source = checkout("publication", "https://github.com/fixture/app.git");
    let copy = checkout("copy", "https://github.com/foreign/app.git");
    let moved = checkout("moved", "https://github.com/other/app.git");
    let repo = root.join("aterm");
    std::fs::create_dir(&repo).unwrap();
    let probe = |exec: &Command| {
        let mut cmd = command(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "release_steps::tests::a_releases_frozen_cutter_takes_the_engine_source_and_repository_it_captured",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE, &repo)
        .stdin(Stdio::null());
        for name in HANDOVER {
            cmd.env_remove(name);
            if let Some(Some(value)) = named(exec, name) {
                cmd.env(name, value);
            }
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{out:?}");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        stdout
            .lines()
            .find_map(|line| line.split_once("PROBE ").map(|(_, probe)| probe.to_owned()))
            .unwrap_or_else(|| panic!("no probe line: {stdout}"))
    };
    let (s, c, m) = (source.display(), copy.display(), moved.display());

    let mut real = Real::new(&repo, Some(&source), true, false).unwrap();
    assert_eq!(real.engine_repository, Ok(slug("fixture/app")));
    // A real run's engine is its copy; this dry one has none made.
    real.engine.clone_from(&copy);
    let cutter = Path::new("/usr/bin/true");
    let exec = real.cutter_command(cutter);
    assert_eq!(
        named(&exec, "PUBLICATION_ENGINE"),
        Some(Some(copy.clone().into_os_string())),
        "a cutter older than the handover is named the release's copy, never its source"
    );
    assert_eq!(
        probe(&exec),
        format!(
            "source={s} repository=fixture/app next_source={s} next_repository=fixture/app \
             next_why="
        )
    );
    assert_eq!(
        probe(&real.scrubbed(cutter)),
        format!(
            "source={c} repository=foreign/app next_source={c} next_repository=foreign/app \
             next_why="
        ),
        "NEGATIVE CONTROL: the exec of a release older than the handover"
    );
    real.engine_source.clone_from(&moved);
    let why = engine_authority(Ok(slug("other/app")), handed("fixture/app"), None).unwrap_err();
    assert_eq!(
        probe(&real.cutter_command(cutter)),
        format!(
            "source={m} repository=refused: {why} next_source={m} next_repository=none \
             next_why={why}"
        ),
        "a source naming another repository since the capture"
    );
    real.engine_repository = Err("no origin".into());
    real.engine_source.clone_from(&copy);
    assert_eq!(
        probe(&real.cutter_command(cutter)),
        format!(
            "source={c} repository=refused: no origin next_source={c} next_repository=none \
             next_why=no origin"
        ),
        "a release that admitted none, refused for its own reason"
    );
}

/// NEGATIVE CONTROL: leaking a launch envelope lets a nested cutter mistake the
/// parent's proof or retry budget for its own.
#[test]
fn only_exec_explicitly_hands_the_freezer_envelope() {
    let names = [
        crate::release_tools::RECORD_ENV,
        crate::release_tools::FREEZER_ENV,
        crate::release_tools::CONTRACT_ENV,
        crate::release_tools::REFREEZE_ENV,
    ];
    let mut cmd = command("/usr/bin/true");
    scrub_names(&mut cmd, names.map(std::ffi::OsString::from));
    for name in names {
        assert!(
            cmd.get_envs()
                .any(|(key, value)| key == name && value.is_none())
        );
    }
}

/// NEGATIVE CONTROL for the scrub above: a child that runs this frozen image (step 7's
/// `cut --sign`, step 8's `cut`) gets back the record this process was proved by, and no
/// part of the freezer envelope. Without it the copy falls back to a dep-info judgment no
/// copy passes: 0.177.0 refused at step 7 on 2026-11-08 ("this aterm-release binary is no
/// commit's cutter: cannot read …/aterm-release.d") when only the scrub ran. A cutter that
/// was not proved hands no record.
#[test]
fn a_child_of_this_frozen_image_is_handed_its_proof_and_never_the_envelope() {
    use crate::release_tools::{CONTRACT_ENV, FREEZER_ENV, RECORD_ENV, REFREEZE_ENV};
    let names = [RECORD_ENV, FREEZER_ENV, CONTRACT_ENV, REFREEZE_ENV];
    let exe = Path::new("/release/tools/0a/aterm-release");
    let record = std::ffi::OsStr::new("/release/cutter.toml");

    let mut proved = command("/usr/bin/true");
    scrub_names(&mut proved, names.map(std::ffi::OsString::from));
    hand_cutter_with_proof(&mut proved, exe, None, Some(record.to_owned()));
    let envs: Vec<_> = proved.get_envs().collect();
    assert!(
        envs.contains(&(std::ffi::OsStr::new(RECORD_ENV), Some(record))),
        "{envs:?}"
    );
    assert!(
        envs.contains(&(
            std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
            Some(exe.as_os_str())
        )),
        "{envs:?}"
    );
    for name in [FREEZER_ENV, CONTRACT_ENV, REFREEZE_ENV] {
        assert!(
            envs.contains(&(std::ffi::OsStr::new(name), None)),
            "{name}: {envs:?}"
        );
    }

    let mut built = command("/usr/bin/true");
    scrub_names(&mut built, names.map(std::ffi::OsString::from));
    hand_cutter_with_proof(&mut built, exe, None, None);
    assert!(
        built
            .get_envs()
            .any(|(key, value)| key == RECORD_ENV && value.is_none())
    );
}

/// Step 1's seal arms keeping in the release state, then takes the seal, whose failure is
/// the step's; every child the backend starts (a step or the cut) is handed the kept seal,
/// and none while none is kept. CONTROL: off macOS step 1 arms and
/// takes nothing. The seal's reports go to the run's transcript, as the cutter's stderr
/// reaches no log. NEGATIVE CONTROL: drop the hand-off from [`Real::scrubbed`], the arm
/// from [`Real::seal_inputs`], or its transcript, and the children carry no seal, nothing
/// is armed, or the cutter's own seal calls go uncounted.
#[test]
fn step_one_keeps_the_inputs_seal_and_every_child_is_handed_it() {
    thread_local! {
        static ARMED: std::cell::RefCell<Vec<(PathBuf, Option<PathBuf>)>> =
            const { std::cell::RefCell::new(Vec::new()) };
        static TAKEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(dir.path(), Some(dir.path()), true, false).expect("a backend");
    real.arm_inputs = |state, log| {
        ARMED.with(|armed| {
            armed
                .borrow_mut()
                .push((state.into(), log.map(Path::to_path_buf)));
        });
    };
    real.build_inputs = || {
        TAKEN.with(|n| n.set(n.get() + 1));
        Ok("inputs".into())
    };
    let macos = cfg!(target_os = "macos");
    assert_eq!(real.seal_inputs(), Ok(()));
    assert_eq!(
        (
            ARMED.with(|armed| armed.borrow().clone()),
            TAKEN.with(std::cell::Cell::get)
        ),
        if macos {
            (
                vec![(real.state.clone(), Some(real.logs.join("transcript.log")))],
                1,
            )
        } else {
            (Vec::new(), 0)
        }
    );
    assert!(real.state.ends_with(RELEASE_STATE));
    real.build_inputs = || Err("build input changed while it was hashed: /sdk".into());
    assert_eq!(
        real.seal_inputs().is_err(),
        macos,
        "a seal that cannot be taken refuses step 1"
    );

    let seal = |cmd: &Command| {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(crate::build_inputs::SEAL_ENV))
            .and_then(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
    };
    let prover = Prover {
        bin: PathBuf::from("/store/trust/9192/bin"),
        commit: "321aaeda7".into(),
        source: "the store".into(),
    };
    let exe = || Ok(PathBuf::from("/t/gate/aterm-release"));
    let children = |real: &Real| {
        [
            real.child("/usr/bin/true", None),
            real.cut_command_of(exe(), &["cut".into()], &prover)
                .unwrap(),
        ]
        .map(|cmd| seal(&cmd))
    };
    real.handed_inputs = || None;
    assert_eq!(children(&real), [None, None], "none kept, none handed");
    real.handed_inputs = || Some(PathBuf::from("/state/aterm-release/inputs-seal/k.toml"));
    let kept = Some(std::ffi::OsString::from(
        "/state/aterm-release/inputs-seal/k.toml",
    ));
    assert_eq!(children(&real), [kept.clone(), kept]);
}

#[test]
fn a_stage_is_handed_this_cutter_for_its_transforms_only_as_the_commits_own_build() {
    let (commit, other) = ("c".repeat(40), "d".repeat(40));
    let exe = || Ok(PathBuf::from("/t/gate/aterm-release"));
    assert_eq!(
        transforms_cutter(Ok(commit.as_str()), exe(), &commit),
        Ok(PathBuf::from("/t/gate/aterm-release"))
    );
    let dirty = crate::cutter::NotOwn {
        why: "its checkout /w has uncommitted cutter sources (Cargo.lock)".into(),
        fix: String::new(),
    };
    for (built, words) in [
        (Ok(other.as_str()), "the build of ddddddddd, not ccccccccc"),
        (
            Err(&dirty),
            "no commit's build: its checkout /w has uncommitted",
        ),
    ] {
        let why = transforms_cutter(built, exe(), &commit).unwrap_err();
        assert!(why.contains(words), "{words}: {why}");
    }
    let unnamed = transforms_cutter(
        Ok(commit.as_str()),
        Err(std::io::Error::other("gone")),
        &commit,
    );
    assert!(
        unnamed
            .unwrap_err()
            .contains("cannot name its own executable (gone)")
    );
}

#[test]
fn a_verbose_step_logs_both_streams_in_the_order_written() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(dir.path(), None, true, true).expect("a backend");
    let mut cmd = real.child("/bin/sh", None);
    cmd.args(["-c", "echo one; echo two >&2; echo three; exit 7"]);
    let ran = real.logged(Step::Sync, &mut cmd);
    assert_eq!(ran.code, 7, "{ran:?}");
    let log = std::fs::read_to_string(real.log_path(Step::Sync)).unwrap();
    let at = |needle: &str| {
        log.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {log}"))
    };
    assert!(
        at("one\n") < at("two\n") && at("two\n") < at("three\n"),
        "{log}"
    );
}

/// The direct child exits only after its background descendant holds the shared output
/// writer. The zero drain grace makes the step cancel immediately, and its witness must say
/// the pump is gone before the return. NEGATIVE CONTROL: discarding the pump's join handle
/// after the timed receive leaves the witness set until the descendant is released.
#[cfg(unix)]
#[test]
fn a_logged_step_joins_its_pump_when_a_grandchild_holds_the_pipe() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let go = dir.path().join("finish-after-return");
    let finished = dir.path().join("grandchild-finished");
    let token = dir.path().join("pump-token");
    std::fs::write(&token, b"pump-read-before-direct-exit\n").unwrap();
    let pump_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let real = Real::new(dir.path(), None, true, false).expect("a backend");
    let log = real.step_log(Step::Sync);
    let mut cmd = real.child("/bin/sh", None);
    cmd.arg("-c")
        .arg(
            "(token=$(/bin/cat \"$2\"); printf '%s\\n' \"$token\"; \
             while [ ! -e \"$0\" ]; do /bin/sleep 0.01; done; : > \"$1\") & \
             token=$(/bin/cat \"$2\"); \
             while ! /usr/bin/grep -Fq \"$token\" \"$3\"; do /bin/sleep 0.01; done; exit 0",
        )
        .arg(&go)
        .arg(&finished)
        .arg(&token)
        .arg(&log);
    let ran = real.logged_in_with_drain(
        Logged {
            step: Step::Sync,
            log,
            stream: true,
            stop: None,
            push: None,
            arrivals: false,
            settles: false,
        },
        &mut cmd,
        std::time::Duration::ZERO,
        Some(pump_active.clone()),
    );

    let active_after_return = pump_active.load(std::sync::atomic::Ordering::SeqCst);
    std::fs::write(&go, b"go").unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the grandchild to finish after the pump assertion",
        || finished.exists().then_some(()),
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(!active_after_return, "the output pump survived the return");
}

/// NEGATIVE CONTROLS: treating build-script executions or arbitrary JSON as artifacts
/// overcounts compiled units; treating absent freshness as false claims an unobserved
/// compile. A fresh build-script executable is still a compiler unit.
#[test]
fn frozen_compile_counts_only_artifacts_with_observed_freshness() {
    let output = br#"{"reason":"compiler-artifact","fresh":true,"target":{"kind":["custom-build"]}}
{"reason":"compiler-artifact","fresh":false}
{"reason":"compiler-artifact"}
{"reason":"build-script-executed","fresh":false}
{"fresh":false}
not json
"#;
    let line = frozen_build_event(
        "aterm-release",
        "gate",
        Some("build.build-dir='fixture'"),
        output,
    );
    let event: aterm_json::Value =
        aterm_json::from_str(line.strip_prefix("release-event: ").unwrap()).unwrap();
    assert_eq!(event["fresh_units"].as_u64(), Some(1));
    assert_eq!(event["compiled_units"].as_u64(), Some(1));
    assert_eq!(event["unknown_units"].as_u64(), Some(1));
    assert_eq!(
        event["build_layout"].as_str(),
        Some("build.build-dir='fixture'")
    );
}

/// Each step child leaves one `step-child-finished` record after its output: its exit, when
/// it ran, and the CPU sampled in its descendants (here a
/// grandchild that did all the work), with no `outcome`, so a child's exit is never a red.
#[cfg(unix)]
#[test]
fn a_step_child_books_the_cpu_of_what_it_reaped_and_is_no_red() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = Real::new(dir.path(), None, true, false).expect("a backend");
    let mut cmd = real.child("/bin/sh", None);
    cmd.args([
        "-c",
        "/bin/sh -c 'i=0; while [ $i -lt 200000 ]; do i=$((i+1)); done' & wait; echo done; exit 3",
    ]);
    let ran = real.logged(Step::Qualify, &mut cmd);
    assert_eq!(ran.code, 3, "{ran:?}");
    let log = std::fs::read_to_string(real.step_log(Step::Qualify)).unwrap();
    let line = log
        .lines()
        .find(|l| l.contains("\"step-child-finished\""))
        .unwrap_or_else(|| panic!("no record: {log}"));
    assert!(
        log.find("done\n").unwrap() < log.find(line).unwrap(),
        "the record follows the output: {log}"
    );
    let event: aterm_json::Value =
        aterm_json::from_str(line.strip_prefix("release-event: ").unwrap()).unwrap();
    assert_eq!(event["step"].as_str(), Some("qualify"), "{event}");
    assert_eq!(event["exit_code"].as_u64(), Some(3), "{event}");
    assert!(event.get("outcome").is_none(), "{event}");
    assert!(
        event["started_at_unix_ms"].as_u64() <= event["ended_at_unix_ms"].as_u64(),
        "{event}"
    );
    if !event["child_cpu"].is_null() {
        assert_eq!(
            event["child_cpu"]["scope"].as_str(),
            Some("sampled-descendant-processes")
        );
        assert!(event["starved_s"].as_f64().is_some(), "{event}");
    } else {
        assert!(event["starved_s"].is_null(), "{event}");
    }
    assert!(
        event["program"].as_str().unwrap().starts_with("sh -c"),
        "{event}"
    );
}

/// A step child whose output the step reads (the fixture generator, the export dry run)
/// is booked as a logged one is, and its output still comes back whole and apart.
#[test]
fn a_captured_step_child_is_booked_and_keeps_its_output() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = Real::new(dir.path(), None, true, false).expect("a backend");
    let mut cmd = real.child("/bin/sh", None);
    cmd.args([
        "-c",
        "i=0; while [ $i -lt 200000 ]; do i=$((i+1)); done; echo out; echo err >&2; exit 4",
    ]);
    let (out, record) = real.output_accounted(Step::Qualify, &mut cmd).unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        (out.stdout.as_slice(), out.stderr.as_slice()),
        (&b"out\n"[..], &b"err\n"[..])
    );
    let event: aterm_json::Value =
        aterm_json::from_str(record.strip_prefix("release-event: ").unwrap()).unwrap();
    assert_eq!(event["step"].as_str(), Some("qualify"), "{event}");
    assert_eq!(event["exit_code"].as_u64(), Some(4), "{event}");
    assert!(event.get("outcome").is_none(), "{event}");
    if !event["child_cpu"].is_null() {
        assert_eq!(
            event["child_cpu"]["scope"].as_str(),
            Some("sampled-descendant-processes")
        );
        assert!(event["starved_s"].as_f64().is_some(), "{event}");
    } else {
        assert!(event["starved_s"].is_null(), "{event}");
    }
}

/// The publish step's lines are stamped as they arrive, in a side log, even when they are
/// not echoed: the split of the engine's time comes from nowhere else.
#[cfg(unix)]
#[test]
fn the_publish_steps_lines_are_stamped_as_they_arrive() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut real = Real::new(dir.path(), None, true, false).expect("a backend");
    real.scratch_logs = false;
    let mut cmd = real.child("/bin/sh", None);
    cmd.args([
        "-c",
        "echo '== first section'; echo body >&2; echo '== second'",
    ]);
    let ran = real.logged(Step::Publish, &mut cmd);
    real.scratch_logs = true;
    assert_eq!(ran.code, 0, "{ran:?}");
    let log = std::fs::read_to_string(real.step_log(Step::Publish)).unwrap();
    assert!(
        log.contains("== first section\n") && log.contains("body\n"),
        "{log}"
    );
    let arrivals = std::fs::read_to_string(real.side_log(Step::Publish, "arrivals")).unwrap();
    let lines: Vec<aterm_json::Value> = arrivals
        .lines()
        .map(|l| aterm_json::from_str(l.strip_prefix("release-event: ").unwrap()).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "{arrivals}");
    assert!(
        lines
            .iter()
            .all(|e| e["event"].as_str() == Some("publish-line")
                && e["at_unix_ms"].as_u64().is_some())
    );
    assert!(
        lines
            .iter()
            .any(|e| e["line"].as_str() == Some("== second")),
        "{arrivals}"
    );
}

/// A signal the release was started ignoring (`nohup`) stays ignored, and every other
/// one is caught.
#[cfg(unix)]
#[test]
fn an_inherited_sig_ign_is_kept_and_the_others_are_caught() {
    const PROBE: &str = "ATERM_RELEASE_SIGNAL_PROBE";
    const SIG_DFL: usize = 0;
    const SIG_IGN: usize = 1;
    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    if let Some(mode) = std::env::var_os(PROBE) {
        // SAFETY: SIG_DFL is a disposition, no handler of ours.
        unsafe {
            // SIGTERM, which the probe sends itself, at its default action whatever this
            // test binary was started with: an ignored one stays ignored.
            signal(interrupt::SIGTERM, SIG_DFL);
            if mode == "control" {
                signal(interrupt::SIGHUP, SIG_DFL);
            }
        }
        interrupt::install();
        // SAFETY: as above; the disposition read is put straight back.
        let hup = unsafe {
            let now = signal(interrupt::SIGHUP, SIG_IGN);
            signal(interrupt::SIGHUP, now);
            now
        };
        let hup = match hup {
            SIG_IGN => "ignored",
            SIG_DFL => "default",
            _ => "caught",
        };
        println!("PROBE hup={hup}");
        interrupt::send(std::process::id().cast_signed(), interrupt::SIGTERM);
        let term = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the probe's own SIGTERM caught",
            interrupt::caught,
        );
        println!("PROBE term={term}");
        std::process::exit(0);
    }
    let probe = |nohup: bool, mode: &str| {
        let exe = std::env::current_exe().unwrap();
        let mut cmd = if nohup {
            let mut cmd = command(aterm_uds::os_path!("nohup"));
            cmd.arg(exe);
            cmd
        } else {
            command(exe)
        };
        cmd.args([
            "--exact",
            "release_steps::tests::an_inherited_sig_ign_is_kept_and_the_others_are_caught",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE, mode);
        // Its stdin closed, within twice the hang detector: the probe's own detector says
        // first what never came.
        let out = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN * 2)
            .expect("hang detector: the probe never ended");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let kept = probe(true, "nohup");
    assert!(kept.contains("PROBE hup=ignored"), "{kept}");
    assert!(kept.contains("PROBE term=15"), "{kept}");
    let control = probe(false, "control");
    assert!(control.contains("PROBE hup=caught"), "{control}");
    assert!(control.contains("PROBE term=15"), "{control}");
}

/// How many looks a settling child's forwarder makes, every one with the release's own stop
/// caught, before the probe judges what it sent
/// ([`a_settling_child_is_never_sent_the_own_stop_and_is_sent_one_from_elsewhere`]).
#[cfg(unix)]
const SETTLING_LOOKS: u32 = 4;

/// A SETTLING CHILD IS NEVER SENT THE RELEASE'S OWN STOP, AND IS SENT ONE FROM ELSEWHERE. Once
/// this process stopped itself on its own account (`interrupt::stop_self`, a lost fleet
/// line's `SIGTERM`) and caught that stop, a child [`Real::logged_settling`] starts (the
/// rewind's `git reset --keep`; here a `sleep` that only a signal ends) runs on: its
/// forwarder looks [`SETTLING_LOOKS`] times and sends its group nothing, and the child is
/// still running. A person's `SIGINT` caught then is forwarded, and the child ends. The
/// probe waits for the forwarder's counted looks (`interrupt::counts_for_test`), never for
/// a time, so correct code passes however slowly the forwarder runs, and a forwarder that
/// sends the own stop at any of those looks has sent it by then. Signal state is
/// process-global, so the case runs in this test binary re-executed as a child.
/// NEGATIVE CONTROLS: `interrupt::Running::settling` forwarding every caught signal (as
/// `Running::enter` does) sent the group the own `SIGTERM` at its first look; one that
/// did so from its second look on, which the scratch reset of
/// `tests/it/release_line.rs`'s probe outran, sent it too. Each killed the child before
/// the person's signal.
#[cfg(unix)]
#[test]
fn a_settling_child_is_never_sent_the_own_stop_and_is_sent_one_from_elsewhere() {
    const PROBE: &str = "ATERM_RELEASE_SETTLING_FORWARD_PROBE";
    if std::env::var_os(PROBE).is_some() {
        settling_forward_probe();
        return;
    }
    let mut probe = command(std::env::current_exe().unwrap());
    probe
        .args([
            "--exact",
            "release_steps::tests::a_settling_child_is_never_sent_the_own_stop_and_is_sent_one_from_elsewhere",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE, "1");
    // Within twice the hang detector: the probe's own detector says first what never came.
    let out = aterm_uds::deadline::output_within(&mut probe, aterm_spec::wait::MUST_HAPPEN * 2)
        .unwrap_or_else(|e| panic!("hang detector: the probe never ended: {e}"));
    let said = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && said.contains("settling probe: judged"),
        "{:?}\n{said}\n{err}",
        out.status
    );
}

/// [`a_settling_child_is_never_sent_the_own_stop_and_is_sent_one_from_elsewhere`]'s child.
#[cfg(unix)]
fn settling_forward_probe() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    // SAFETY: SIG_DFL (0) for INT and TERM, which this probe sends itself, whatever this
    // test binary was started with: `install` leaves an ignored one ignored.
    unsafe {
        signal(interrupt::SIGINT, 0);
        signal(interrupt::SIGTERM, 0);
    }
    interrupt::install();
    interrupt::stop_self();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the release's own SIGTERM caught",
        || (interrupt::caught() == Some(interrupt::SIGTERM)).then_some(()),
    );
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = Real::new(dir.path(), None, true, false).expect("a backend");
    let mut cmd = real.child(aterm_uds::os_path!("sleep"), None);
    // Longer than every hang detector here: only a signal ends it.
    cmd.arg("300");
    let returned = Arc::new(AtomicBool::new(false));
    let pid = std::process::id().cast_signed();
    let watcher = std::thread::spawn({
        let returned = Arc::clone(&returned);
        move || {
            // Until the forwarder looked, or the child ended (only a signal its group was
            // sent ends it): a one-minute hang detector that does not panic, so the
            // person's signal below still ends the child when neither comes.
            let looked = aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                std::time::Duration::from_millis(10),
                || {
                    (returned.load(Ordering::SeqCst)
                        || interrupt::counts_for_test().looks >= SETTLING_LOOKS)
                        .then_some(())
                },
            );
            let seen = interrupt::counts_for_test();
            let running = !returned.load(Ordering::SeqCst);
            // A person's ^C, a signal from elsewhere, sent whatever was seen.
            interrupt::send(pid, interrupt::SIGINT);
            (looked.is_some(), seen, running)
        }
    });
    let ran = real.logged_settling(Step::Commit, &mut cmd);
    returned.store(true, Ordering::SeqCst);
    let (looked, seen, running) = watcher.join().expect("the watcher ends");
    let after = interrupt::counts_for_test();
    assert_eq!(
        seen.caught, 1,
        "the looks were judged with the release's own stop alone caught: {seen:?} {ran:?}"
    );
    assert_eq!(
        seen.sends, 0,
        "the settling child's group was sent the release's own stop: {seen:?} {ran:?}"
    );
    assert!(
        running,
        "the settling child ended before a signal from elsewhere: {seen:?} {ran:?}"
    );
    assert!(
        looked,
        "the settling child's forwarder never looked {SETTLING_LOOKS} times: {seen:?} {ran:?}"
    );
    assert!(
        after.sends > 0 && ran.code != 0,
        "a person's signal is forwarded and ends the settling child: {after:?} {ran:?}"
    );
    println!("settling probe: judged");
}

#[test]
fn kept_audit_rows_go_back_in_time_order() {
    let text = "{\"at\":\"1\"}\n{\"at\":\"3\"}\n";
    let kept = vec!["{\"at\":\"2\"}".to_string(), "{\"at\":\"3\"}".to_string()];
    assert_eq!(
        audit_rows_merged(text, &kept),
        "{\"at\":\"1\"}\n{\"at\":\"2\"}\n{\"at\":\"3\"}\n"
    );
    assert_eq!(local_stamp(0, 0), "19700101T000000");
    assert_eq!(local_stamp(1_792_627_200, 7200), "20261022T020000");
    assert_eq!(local_stamp(1_792_627_200, -27_000), "20261021T163000");
    let (pdt, pst, fall_back) = (-7 * 3600, -8 * 3600, 1_793_523_600);
    let zone = |t: i64| Some(if t < fall_back { pdt } else { pst });
    assert_eq!(hms_local(1_793_518_200, zone), "00:30:00");
    assert_eq!(hms_local(1_793_527_200, zone), "02:00:00");
    assert_eq!(
        hms_local(1_793_527_200, |_| Some(pdt)),
        "03:00:00",
        "NEGATIVE CONTROL: start-of-run offset shifts post-change step times by an hour"
    );
    assert!(clears("RUSTFLAGS") && clears("CARGO_TARGET_DIR") && !clears("PATH"));
    for name in [
        "CARGO",
        "CARGO_PKG_NAME",
        "TARGO_MANIFEST_DIR",
        "TRUST_TARGO_FRONTEND",
    ] {
        assert!(launcher_env(name), "{name}");
    }
    assert!(!launcher_env("CARGO_HOME") && !launcher_env("HOME"));
}

/// A commit-only gate that reads origin is R's red only when origin answered every read: with no
/// origin, or with a read of it that failed while the gate judged, it could not ask (COULD NOT
/// RUN) and names no gate, so a remote's failure opens no repair worker. NEGATIVE CONTROL: the
/// same red after reads that answered is the gate's red.
#[test]
fn an_origin_gate_that_could_not_ask_names_no_red_of_r() {
    /// origin answers `ls-remote` with no tags, or (`down`) not at all.
    struct Remote {
        down: bool,
    }
    impl GitRunner for Remote {
        fn git(&self, args: &[&str]) -> crate::ledger::Result<crate::ledger::RunOut> {
            if self.down {
                return Err(crate::ledger::Error::new(format!(
                    "git {}: Could not resolve host: github.com",
                    args.join(" ")
                )));
            }
            Ok(crate::ledger::RunOut {
                status: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }
    let reads_then_red = |origin: &dyn crate::ledger::Origin| -> crate::ledger::Result<()> {
        origin.tag_catalog().map_err(crate::ledger::Error::new)?;
        Err(crate::ledger::Error::new(
            "CHANGELOG.md has no entry for 0.6.0",
        ))
    };
    let down = crate::gates::fixture_origin::FixtureOrigin(&Remote { down: true });
    let refused = origin_gate("changelog", Ok(&down), &reads_then_red).unwrap_err();
    assert!(
        refused.could_not_run
            && refused.failing.is_none()
            && refused.why.starts_with("changelog — "),
        "{refused:?}"
    );
    let none = crate::ledger::Error::new("the clone has no GitHub origin");
    let refused = origin_gate("handoff policy", Err(&none), &reads_then_red).unwrap_err();
    assert!(
        refused.could_not_run && refused.failing.is_none(),
        "{refused:?}"
    );

    let up = crate::gates::fixture_origin::FixtureOrigin(&Remote { down: false });
    let refused = origin_gate("changelog", Ok(&up), &reads_then_red).unwrap_err();
    assert!(!refused.could_not_run, "{refused:?}");
    assert_eq!(refused.failing.as_deref(), Some("changelog"));
    assert!(
        origin_gate("changelog", Ok(&up), &|origin| {
            origin.tag_catalog().map_err(crate::ledger::Error::new)?;
            Ok(())
        })
        .is_ok()
    );
}

#[cfg(unix)]
mod nested_lock_files {
    use super::*;

    const NESTED_LOCK_BEFORE: &str = "version = 4\n\n[[package]]\nname = \"aterm-local\"\nversion = \"0.147.0\"\n\n[[package]]\nname = \"registry\"\nversion = \"0.147.0\"\nsource = \"registry+https://example.invalid/index\"\n";
    const NESTED_LOCK_AFTER: &str = "version = 4\n\n[[package]]\nname = \"aterm-local\"\nversion = \"0.148.0\"\n\n[[package]]\nname = \"registry\"\nversion = \"0.147.0\"\nsource = \"registry+https://example.invalid/index\"\n";

    fn ordinary_nested_bump(repo: &Path) {
        let parent = repo.join("ordinary");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::write(parent.join("Cargo.lock"), NESTED_LOCK_BEFORE).unwrap();
        assert_eq!(
            bump_nested_locks(
                repo,
                Ok(vec!["ordinary/Cargo.lock".into()]),
                "0.147.0",
                "0.148.0"
            )
            .unwrap(),
            ["ordinary/Cargo.lock"]
        );
        assert_eq!(
            std::fs::read_to_string(parent.join("Cargo.lock")).unwrap(),
            NESTED_LOCK_AFTER,
            "the local package moves and the registry version stays"
        );
    }

    #[cfg(unix)]
    #[test]
    fn nested_lock_bumps_refuse_aliases_without_touching_their_payloads() {
        use std::os::unix::fs::MetadataExt as _;

        let mut outcomes = Vec::new();
        for kind in ["symlink", "hardlink"] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let home = scratch.path().canonicalize().unwrap();
            let repo = home.join("repo");
            ordinary_nested_bump(&repo);
            std::fs::create_dir(repo.join("nested")).unwrap();
            let payload = home.join("outside-repo.lock");
            let alias = repo.join("nested/Cargo.lock");
            std::fs::write(&payload, NESTED_LOCK_BEFORE).unwrap();
            if kind == "symlink" {
                std::os::unix::fs::symlink(&payload, &alias).unwrap();
            } else {
                std::fs::hard_link(&payload, &alias).unwrap();
            }
            let identity = |path: &Path| {
                let meta = std::fs::symlink_metadata(path).unwrap();
                (meta.dev(), meta.ino(), meta.nlink())
            };
            let before = identity(&alias);
            let result = bump_nested_locks(
                &repo,
                Ok(vec!["nested/Cargo.lock".into()]),
                "0.147.0",
                "0.148.0",
            );
            outcomes.push((
                kind,
                result.is_err(),
                identity(&alias) == before,
                std::fs::read_to_string(&payload).unwrap() == NESTED_LOCK_BEFORE,
            ));
        }
        assert!(
            outcomes
                .iter()
                .all(|(_, refused, kept, untouched)| *refused && *kept && *untouched),
            "each alias must be refused and retain its external payload: {outcomes:?}"
        );
    }

    #[test]
    fn nested_lock_refusals_preserve_every_target_before_publication() {
        let mut outcomes = Vec::new();
        for kind in ["invalid-utf8", "missing", "directory", "listing-refused"] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let repo = scratch.path().canonicalize().unwrap();
            ordinary_nested_bump(&repo);
            std::fs::create_dir(repo.join("good")).unwrap();
            std::fs::write(repo.join("good/Cargo.lock"), NESTED_LOCK_BEFORE).unwrap();
            std::fs::create_dir(repo.join("unknown")).unwrap();
            match kind {
                "invalid-utf8" => std::fs::write(repo.join("unknown/Cargo.lock"), [0xff]).unwrap(),
                "directory" => std::fs::create_dir(repo.join("unknown/Cargo.lock")).unwrap(),
                "missing" | "listing-refused" => {}
                _ => unreachable!(),
            }
            let listing = if kind == "listing-refused" {
                Err("inert git enumeration refusal".to_owned())
            } else {
                Ok(vec!["good/Cargo.lock".into(), "unknown/Cargo.lock".into()])
            };
            let result = bump_nested_locks(&repo, listing, "0.147.0", "0.148.0");
            outcomes.push((
                kind,
                result.is_err(),
                std::fs::read_to_string(repo.join("good/Cargo.lock")).unwrap()
                    == NESTED_LOCK_BEFORE,
            ));
        }
        assert!(
            outcomes
                .iter()
                .all(|(_, refused, unchanged)| *refused && *unchanged),
            "unknown evidence must stop the batch before any target changes: {outcomes:?}"
        );
    }
}

/// The bump moves a nested lock's path crates with the root's and leaves a
/// registry crate that happens to share the old version where it was.
#[test]
fn a_nested_lock_follows_the_bump_in_its_path_crates_only() {
    use super::lock_follows_bump;
    let lock = "version = 4\n\n[[package]]\nname = \"aterm-font\"\nversion = \"0.147.0\"\n\
                dependencies = [\n \"aterm-hash\",\n]\n\n[[package]]\nname = \"lookalike\"\n\
                version = \"0.147.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                checksum = \"00\"\n\n[[package]]\nname = \"astream-wire\"\nversion = \"0.2.0\"\n\n\
                [[package]]\nname = \"aterm-hash\"\nversion = \"0.147.0\"\n";
    let moved = lock_follows_bump(lock, "0.147.0", "0.148.0").expect("two path crates move");
    assert_eq!(
        moved,
        lock.replacen(
            "name = \"aterm-font\"\nversion = \"0.147.0\"",
            "name = \"aterm-font\"\nversion = \"0.148.0\"",
            1
        )
        .replacen(
            "name = \"aterm-hash\"\nversion = \"0.147.0\"",
            "name = \"aterm-hash\"\nversion = \"0.148.0\"",
            1
        )
    );
    assert!(moved.contains("name = \"lookalike\"\nversion = \"0.147.0\""));
    assert_eq!(lock_follows_bump(lock, "0.107.0", "0.148.0"), None);
    let crlf = "[[package]]\r\nname = \"aterm-uds\"\r\nversion = \"0.147.0\"\r\n";
    assert_eq!(
        lock_follows_bump(crlf, "0.147.0", "0.148.0").as_deref(),
        Some("[[package]]\r\nname = \"aterm-uds\"\r\nversion = \"0.148.0\"\r\n")
    );
    let ambiguous = "[[package]]\nname = \"leaf\"\nversion = \"0.147.0\"\n\n[[package]]\nname = \"user\"\nversion = \"0.0.0\"\ndependencies = [\n \"leaf 0.147.0\",\n \"leaf 0.147.0 (registry+https://example.invalid/index)\",\n]\n";
    assert_eq!(
        lock_follows_bump(ambiguous, "0.147.0", "0.148.0").unwrap(),
        ambiguous
            .replacen("version = \"0.147.0\"", "version = \"0.148.0\"", 1)
            .replacen("\"leaf 0.147.0\"", "\"leaf 0.148.0\"", 1)
    );
}

/// Step 1 asks the released-shim judgment of a tree exactly while its `test` check has no
/// passing record: none yet, a FAIL, or records nobody can read.
/// NEGATIVE CONTROLS: a tree whose `test` record passed is not asked again, whatever else it
/// still owes or failed, so a release that qualified is not refused over a later public release.
#[test]
fn the_early_released_shim_ask_is_owed_until_the_trees_test_record_passes() {
    use crate::check_record::{Checks, Failed};
    let failed = |check: &str| Failed {
        check: check.to_owned(),
        log: String::new(),
        unrecorded: false,
    };
    assert!(super::shim_owed(&Err("the records cannot be read".into())));
    assert!(super::shim_owed(&Ok(Checks::no("no record stands"))));
    let mut red = Checks::default();
    red.failed.push(failed("test"));
    assert!(super::shim_owed(&Ok(red)));

    assert!(!super::shim_owed(&Ok(Checks::default())));
    let mut other = Checks::default();
    other.missing.push("measure".to_owned());
    other.failed.push(failed("doc"));
    assert!(!super::shim_owed(&Ok(other)));
}

/// Step 1 keeps the judgment's own row and reads its verdict as a pass, as R's red or as a
/// judgment this Mac could not make. NEGATIVE CONTROL: an undecided judgment is never a pass
/// and never R's red.
#[test]
fn step_one_reads_the_released_shim_verdict_as_a_pass_a_red_or_unanswered() {
    use aterm_verify::released_shim::{Judged, Verdict};
    let row = "released-shim: the row";
    let found = |verdict| {
        super::shim_finding(Judged {
            verdict,
            row: row.to_owned(),
            output: String::new(),
        })
    };
    assert_eq!(found(Verdict::Accepted), Ok(row.to_owned()));
    assert_eq!(found(Verdict::NotThisHost), Ok(row.to_owned()));
    assert_eq!(
        found(Verdict::Refused),
        Err(super::InputFinding::Failed(row.to_owned()))
    );
    assert_eq!(
        found(Verdict::Undecided),
        Err(super::InputFinding::Unanswered(row.to_owned()))
    );
}
