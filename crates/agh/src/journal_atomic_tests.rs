// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::github::refs::{ATOMIC_KEY, AtomicRefBatch, RefUpdate};

fn native(names: &[&str]) -> IntentDraft {
    let batch = AtomicRefBatch::new(
        names
            .iter()
            .map(|name| RefUpdate {
                reference: (*name).into(),
                before: "a".repeat(40),
                after: "b".repeat(40),
            })
            .collect(),
    )
    .unwrap();
    let bytes = crate::fx::canon::native_refs_text(&app().key(), &batch, false).into_bytes();
    draft(WriteOp::RefUpdateAtomic, ATOMIC_KEY)
        .with_refs(batch.updates().to_vec())
        .with_request(crate::fx::canon::request_digest(&bytes))
        .with_body_digest(Some(crate::model::Sha256Digest::of(&bytes).to_string()))
}

fn judged(journal: &Journal, draft: IntentDraft) -> IntentDraft {
    let judgment = journal.atomic_judgment(&draft).unwrap();
    draft.with_atomic_judgment(judgment)
}

fn unknown(journal: &Journal, draft: IntentDraft) -> IntentId {
    // The old client protocol has no live scoped judgment. Its durable request
    // and ref data nevertheless prove this ordinary write's exact domain.
    let id = begin(journal, draft);
    journal
        .settle(
            &id,
            &Settled::Unknown {
                why: "receive remained uncertain".into(),
            },
        )
        .unwrap();
    id
}

#[test]
fn an_unknown_legacy_native_line_does_not_block_main_or_lose_its_custody() {
    let root = scratch();
    let journal = journal(root.path(), &fx());
    let old = unknown(&journal, native(&["refs/aterm-release/line/0.207.0"]));
    let prefix = std::fs::read(journal.path()).unwrap();
    let main = native(&["refs/heads/main"]);
    assert!(journal.atomic_judgment(&main).unwrap().blocking().is_none());
    let main = journal.begin(&judged(&journal, main)).unwrap();
    journal
        .settle(
            &main,
            &Settled::Created {
                id: 0,
                url: String::new(),
            },
        )
        .unwrap();
    assert!(std::fs::read(journal.path()).unwrap().starts_with(&prefix));
    let old = journal.view(&old).unwrap().unwrap();
    assert!(old.open);
    assert!(matches!(old.settled, Some(Settled::Unknown { .. })));
    assert_eq!(journal.open().unwrap(), vec![old.intent]);
}

#[test]
fn overlapping_batches_and_ref_directory_prefixes_still_exclude_each_other() {
    for (held, wanted, conflict) in [
        (vec!["refs/heads/a"], vec!["refs/heads/a"], true),
        (vec!["refs/heads/a"], vec!["refs/heads/a/b"], true),
        (vec!["refs/heads/a/b"], vec!["refs/heads/a"], true),
        (vec!["refs/heads/a"], vec!["refs/heads/ab"], false),
        (
            vec!["refs/heads/a", "refs/heads/b"],
            vec!["refs/heads/b", "refs/heads/c"],
            true,
        ),
    ] {
        let root = scratch();
        let journal = journal(root.path(), &fx());
        let old = unknown(&journal, native(&held));
        let next = native(&wanted);
        let judgment = journal.atomic_judgment(&next).unwrap();
        assert_eq!(judgment.blocking().is_some(), conflict);
        let result = journal.begin(&next.with_atomic_judgment(judgment));
        if conflict {
            assert!(
                matches!(&result.unwrap_err().sub, Sub::IntentOpen { intent } if intent == &old.to_string())
            );
            assert_eq!(journal.open().unwrap().len(), 1);
        } else {
            result.unwrap();
            assert_eq!(journal.open().unwrap().len(), 2);
        }
    }
}

#[test]
fn generic_batches_cannot_remove_release_control_namespace_exclusion() {
    for control in [
        crate::git::RELEASE_LEASE_REF,
        crate::git::PUBLISHER_FENCE_REF,
    ] {
        for (name, global) in [
            (control.to_string(), true),
            (format!("{control}/child"), true),
            ("refs/tags".to_string(), true),
            (format!("{control}-other"), false),
        ] {
            for reverse in [false, true] {
                let root = scratch();
                let journal = journal(root.path(), &fx());
                let (held, wanted) = if reverse {
                    ("refs/heads/main", name.as_str())
                } else {
                    (name.as_str(), "refs/heads/main")
                };
                let held = unknown(&journal, native(&[held]));
                let before = std::fs::read(journal.path()).unwrap();
                let wanted = native(&[wanted]);
                let judgment = journal.atomic_judgment(&wanted).unwrap();
                assert_eq!(judgment.blocking().is_some(), global, "{name} {reverse}");
                let result = journal.begin(&wanted.with_atomic_judgment(judgment));
                if global {
                    assert!(matches!(
                        &result.unwrap_err().sub,
                        Sub::IntentOpen { intent } if intent == &held.to_string()
                    ));
                    assert_eq!(std::fs::read(journal.path()).unwrap(), before);
                } else {
                    result.unwrap();
                    assert_eq!(journal.open().unwrap().len(), 2);
                }
                assert!(journal.view(&held).unwrap().unwrap().open);
            }
        }
    }
}

#[test]
fn scoped_cas_ignores_disjoint_appends_but_refuses_intervening_overlap() {
    for overlapping in [false, true] {
        let root = scratch();
        let journal = journal(root.path(), &fx());
        let ready = judged(&journal, native(&["refs/heads/a"]));
        let other = if overlapping {
            "refs/heads/a/child"
        } else {
            "refs/heads/b"
        };
        let other = journal.begin(&judged(&journal, native(&[other]))).unwrap();
        journal.settle(&other, &Settled::NeverSent).unwrap();
        let before = std::fs::read(journal.path()).unwrap();
        let result = journal.begin(&ready);
        if overlapping {
            assert_eq!(result.unwrap_err().kind, Kind::OutcomeUnknown);
            assert_eq!(std::fs::read(journal.path()).unwrap(), before);
        } else {
            result.unwrap();
        }
    }
}

#[test]
fn a_newer_closed_ref_cannot_hide_an_older_overlapping_unknown() {
    let root = scratch();
    let journal = journal(root.path(), &fx());
    let old = unknown(&journal, native(&["refs/heads/a"]));
    let later = journal
        .begin(&judged(&journal, native(&["refs/heads/b"])))
        .unwrap();
    journal.settle(&later, &Settled::NeverSent).unwrap();
    assert!(
        !journal
            .latest(WriteOp::RefUpdateAtomic, ATOMIC_KEY)
            .unwrap()
            .unwrap()
            .open
    );
    let next = native(&["refs/heads/a", "refs/heads/b"]);
    let judgment = journal.atomic_judgment(&next).unwrap();
    assert_eq!(judgment.blocking().unwrap().intent.id, old);
    assert!(journal.begin(&next.with_atomic_judgment(judgment)).is_err());
    let snapshot = journal.open().unwrap().pop().unwrap();
    assert_eq!(journal.refresh(&snapshot).unwrap().unwrap().intent.id, old);
}

#[test]
fn unavailable_or_guarded_declarations_remain_repository_wide_in_both_directions() {
    let ordinary = native(&["refs/heads/a"]);
    let guarded_batch = AtomicRefBatch::new(ordinary.refs.clone())
        .unwrap()
        .observing_main(&"c".repeat(40))
        .unwrap();
    let guarded_bytes =
        crate::fx::canon::native_refs_text(&app().key(), &guarded_batch, false).into_bytes();
    let guarded = ordinary
        .clone()
        .with_request(crate::fx::canon::request_digest(&guarded_bytes))
        .with_body_digest(Some(
            crate::model::Sha256Digest::of(&guarded_bytes).to_string(),
        ));
    let malformed = ordinary.clone().with_refs(vec![RefUpdate {
        reference: "refs/heads/a/../bad".into(),
        before: "a".repeat(40),
        after: "b".repeat(40),
    }]);
    for unsupported in [
        ordinary.clone().with_body_digest(None),
        ordinary.clone().with_request(request()),
        ordinary.clone().with_refs(Vec::new()),
        guarded,
        malformed,
    ] {
        for reverse in [false, true] {
            let root = scratch();
            let journal = journal(root.path(), &fx());
            let disjoint = native(&["refs/heads/z"]);
            let (held, wanted) = if reverse {
                (disjoint, unsupported.clone())
            } else {
                (unsupported.clone(), disjoint)
            };
            let id = unknown(&journal, held);
            let judgment = journal.atomic_judgment(&wanted).unwrap();
            assert_eq!(judgment.blocking().unwrap().intent.id, id);
            assert!(
                journal
                    .begin(&wanted.with_atomic_judgment(judgment))
                    .is_err()
            );
        }
    }
}

#[test]
fn a_live_judgment_cannot_be_transplanted_to_a_different_request_or_baseline() {
    for change_baseline in [false, true] {
        let root = scratch();
        let journal = journal(root.path(), &fx());
        let original = native(&["refs/heads/a"]);
        let mut changed = judged(&journal, original);
        if change_baseline {
            changed.after = Some(IntentId::parse("0-0123456789ab").unwrap());
        } else {
            changed.refs[0].after = "c".repeat(40);
            let batch = AtomicRefBatch::new(changed.refs.clone()).unwrap();
            let bytes =
                crate::fx::canon::native_refs_text(&app().key(), &batch, false).into_bytes();
            changed.request = crate::fx::canon::request_digest(&bytes);
            changed.body_digest = Some(crate::model::Sha256Digest::of(&bytes).to_string());
        }
        assert_eq!(journal.begin(&changed).unwrap_err().kind, Kind::Usage);
        assert!(journal.open().unwrap().is_empty());
    }
}

#[test]
fn old_client_judgments_keep_global_exclusion_among_new_scoped_records() {
    let root = scratch();
    let journal = journal(root.path(), &fx());
    let held = journal
        .begin(&judged(&journal, native(&["refs/heads/a"])))
        .unwrap();
    let later = journal
        .begin(&judged(&journal, native(&["refs/heads/b"])))
        .unwrap();
    journal.settle(&later, &Settled::NeverSent).unwrap();
    let legacy = native(&["refs/heads/c"]).with_after(Some(later));
    assert!(
        matches!(&journal.begin(&legacy).unwrap_err().sub, Sub::IntentOpen { intent } if intent == &held.to_string())
    );
}

#[test]
fn a_foreign_repository_record_cannot_be_treated_as_a_disjoint_local_ref() {
    let root = scratch();
    let journal = journal(root.path(), &fx());
    let other = Slug::parse("fixture-dev/other").unwrap();
    let foreign = Journal::open(&Dirs::under(root.path()), &other, &fx()).unwrap();
    let mut draft = native(&["refs/heads/a"]);
    draft.slug = other;
    let batch = AtomicRefBatch::new(draft.refs.clone()).unwrap();
    let bytes = crate::fx::canon::native_refs_text(&draft.slug.key(), &batch, false).into_bytes();
    draft.request = crate::fx::canon::request_digest(&bytes);
    draft.body_digest = Some(crate::model::Sha256Digest::of(&bytes).to_string());
    let id = begin(&foreign, draft);
    let (records, _) = foreign.read().unwrap();
    journal.append(&records[0].1).unwrap();
    let wanted = native(&["refs/heads/z"]);
    let judgment = journal.atomic_judgment(&wanted).unwrap();
    assert_eq!(judgment.blocking().unwrap().intent.id, id);
    assert!(
        journal
            .begin(&wanted.with_atomic_judgment(judgment))
            .is_err()
    );
}

#[test]
fn replay_rejudges_the_same_domains_without_dispatch_or_a_persisted_admission() {
    let root = scratch();
    let tape = root.path().join("atomic-domains.atape");
    let header = crate::fx::tape::TapeHeader::for_run(
        &["atomic-domains".into()],
        &crate::wire::Endpoints::github_com(),
    );
    let exercise = |fx: Fx| {
        let fx = Arc::new(fx);
        let journal = Journal::taped(&app(), &fx);
        let held = unknown(&journal, native(&["refs/heads/a"]));
        let independent = journal
            .begin(&judged(&journal, native(&["refs/heads/b"])))
            .unwrap();
        journal.settle(&independent, &Settled::NeverSent).unwrap();
        let same = judged(&journal, native(&["refs/heads/a"]));
        let error = journal.begin(&same).unwrap_err();
        assert!(matches!(&error.sub, Sub::IntentOpen { intent } if intent == &held.to_string()));
        let result = journal.view(&held).unwrap().unwrap();
        assert!(result.open);
        assert!(fx.children_run().is_empty());
        fx.finish(0).unwrap();
        (result, error.kind, error.sub.clone())
    };
    let recorded = exercise(Fx::record_for_tests(&tape, &header).unwrap());
    let (fx, _) = Fx::replay_for_tests(&tape).unwrap();
    assert_eq!(exercise(fx), recorded);
}

#[test]
fn the_locked_append_admits_disjoint_racers_and_only_one_overlapping_racer() {
    for same_ref in [false, true] {
        let root = scratch();
        let journal = Arc::new(journal(root.path(), &fx()));
        let ready = Arc::new(std::sync::Barrier::new(3));
        let workers: Vec<_> = [
            "refs/heads/a",
            if same_ref {
                "refs/heads/a"
            } else {
                "refs/heads/b"
            },
        ]
        .into_iter()
        .map(|name| {
            let journal = journal.clone();
            let ready = ready.clone();
            std::thread::spawn(move || {
                let draft = judged(&journal, native(&[name]));
                ready.wait();
                journal.begin(&draft)
            })
        })
        .collect();
        ready.wait();
        let passed = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(Result::is_ok)
            .count();
        assert_eq!(passed, if same_ref { 1 } else { 2 });
        assert_eq!(journal.open().unwrap().len(), passed);
    }
}
