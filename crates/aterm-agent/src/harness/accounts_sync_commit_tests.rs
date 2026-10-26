// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

struct Bed {
    root: aterm_tempfile::TempDir,
    store: Store,
    path: PathBuf,
    receipt: Receipt,
}

impl Bed {
    fn new() -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = Store::at(root.path());
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                super::super::rename_tests::seed_reference(state, logins, "old", 0);
                Ok(Vec::new())
            })
            .unwrap();
        let path = root.path().join("roster.toml");
        let old_roster =
            "[[account]]\nlabel = \"old\"\nclass = \"dedicated\"\nemail = \"owner@example.test\"\n";
        let new_roster = old_roster.replace("\"old\"", "\"new\"");
        std::fs::write(&path, old_roster).unwrap();
        let (mut state, _) = store.load(NOW).unwrap();
        let (mut logins, _) = store.load_logins(NOW).unwrap();
        let before = logins.clone();
        move_account(&mut state, &mut logins, "old", "new").unwrap();
        let events = note_auth(&mut state, "new", Auth::SignedOut, NOW);
        advance_renewal_generations(&before, &mut logins, &events).unwrap();
        let plan = Plan {
            local_logins: false,
            wake: None,
            roster_path: Some(absolute(&path).unwrap().to_str().unwrap().into()),
            roster_mode: None,
            state: Image {
                before: read(&store.state_file(), MAX_STATE_BYTES).unwrap(),
                after: Some(state.to_json() + "\n"),
            },
            logins: Image {
                before: read(&store.logins_file(), MAX_LOGINS_BYTES).unwrap(),
                after: Some(logins.to_json() + "\n"),
            },
            ledger: Image {
                before: None,
                after: Some(events.iter().map(|e| e.to_json_line() + "\n").collect()),
            },
            roster: Image {
                before: Some(old_roster.into()),
                after: Some(new_roster),
            },
        };
        let receipt = Receipt {
            schema: 1,
            id: "a".repeat(32),
            plan: Some(plan),
        };
        Self {
            root,
            store,
            path,
            receipt,
        }
    }

    fn pending(&self) {
        self.store.save_receipt(&self.receipt).unwrap();
        self.store
            .retain_witness(&self.store.dir.join(WITNESS))
            .unwrap();
    }

    fn files(&self) -> Vec<Option<String>> {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.path.clone(),
        ]
        .iter()
        .map(|p| read(p, MAX_LEDGER).unwrap())
        .collect()
    }

    fn after(&self) -> Vec<Option<String>> {
        let plan = self.receipt.plan.as_ref().unwrap();
        [&plan.state, &plan.logins, &plan.ledger, &plan.roster]
            .iter()
            .map(|i| i.after.clone())
            .collect()
    }
}

#[test]
fn roster_bound_renewal_keeps_the_read_only_roster_and_bounds_its_ledger() {
    let bed = Bed::new();
    let line = Event::new(NOW - 1, Kind::Use, "old")
        .because(&"x".repeat(200))
        .to_json_line()
        + "\n";
    let rows = usize::try_from(LEDGER_MAX_BYTES).unwrap() / line.len() + 1;
    std::fs::write(bed.store.ledger_path(), line.repeat(rows)).unwrap();
    let original = std::fs::metadata(&bed.path).unwrap();
    let mut readonly = original.permissions();
    readonly.set_readonly(true);
    std::fs::set_permissions(&bed.path, readonly).unwrap();
    let login = bed.root.path().join("new-login");
    bed.store
        .transact_roster_logins_within(&[], &bed.path, NOW, &mut None, |_, state, logins| {
            Ok(note_renewed(
                state,
                logins,
                "old",
                None,
                login.to_str().unwrap(),
                None,
                NOW,
            ))
        })
        .unwrap();
    let current = std::fs::metadata(&bed.path).unwrap();
    assert!(current.permissions().readonly());
    assert_eq!(original.modified().unwrap(), current.modified().unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(original.ino(), current.ino());
    }
    std::fs::set_permissions(&bed.path, original.permissions()).unwrap();
    let ledger = std::fs::read_to_string(bed.store.ledger_path()).unwrap();
    assert_eq!(ledger.lines().count(), LEDGER_KEEP_ROWS);
    assert_eq!(bed.store.tail(1).unwrap()[0].kind, Kind::Renewed);
    assert!(!bed.store.sync_needs_recovery());
}

#[test]
fn sync_commit_recovers_each_durable_prefix_without_reallocating_generations() {
    for cut in 0..=5 {
        let bed = Bed::new();
        let before = bed.store.load_logins(NOW).unwrap().0;
        let parent = RenewalParent::capture(&before, "old");
        bed.pending();
        let held = bed.store.acquire().unwrap();
        let why = bed
            .store
            .recover_sync_with(&held, &bed.path, &mut |step| {
                if step == cut {
                    Err("injected interruption".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(why, "injected interruption");
        assert!(bed.store.load(NOW).is_err());
        assert!(bed.store.load_logins(NOW).is_err());
        assert!(bed.store.check_signin_state(NOW).is_err());
        let mut edited = false;
        assert!(
            bed.store
                .transact_logins_held(&held, NOW, &mut None, |_, _| {
                    edited = true;
                    Ok(Vec::new())
                })
                .is_err()
        );
        assert!(!edited, "pending commit admits no new account edit");
        assert!(bed.store.recover_sync_held(&held, &bed.path).unwrap());
        assert_eq!(bed.files(), bed.after(), "cut {cut}: exact images restored");
        let current = bed.store.load_logins(NOW).unwrap().0;
        assert!(parent.require_current(&current, "old").is_err());
        assert!(current.generation_clock > before.generation_clock);
        assert!(!bed.store.recover_sync_held(&held, &bed.path).unwrap());
        assert_eq!(
            bed.files(),
            bed.after(),
            "completed replay writes no duplicate events"
        );
        assert_eq!(bed.store.tail(10).unwrap().len(), 1);
    }
}

#[test]
fn sync_commit_refuses_conflicts_before_replacing_any_other_file() {
    for index in 0..4 {
        let bed = Bed::new();
        bed.pending();
        let held = bed.store.acquire().unwrap();
        let path = [
            bed.store.state_file(),
            bed.store.logins_file(),
            bed.store.ledger_path(),
            bed.path.clone(),
        ][index]
            .clone();
        std::fs::write(path, "outside edit\n").unwrap();
        let before = bed.files();
        assert!(
            bed.store
                .recover_sync_held(&held, &bed.path)
                .unwrap_err()
                .contains("outside")
        );
        assert_eq!(bed.files(), before);
        assert!(bed.store.load(NOW).is_err());
    }
}

#[test]
fn sync_commit_rejects_changed_target_corruption_and_read_epoch() {
    let bed = Bed::new();
    let clean = Receipt {
        schema: 1,
        id: "b".repeat(32),
        plan: None,
    };
    bed.store.save_receipt(&clean).unwrap();
    let observed = bed.store.sync_read_receipt().unwrap();
    bed.pending();
    let held = bed.store.acquire().unwrap();
    let before = bed.files();
    assert!(
        bed.store
            .recover_sync_held(&held, &bed.path.with_file_name("other.toml"))
            .is_err()
    );
    assert_eq!(bed.files(), before);
    bed.store.recover_sync_held(&held, &bed.path).unwrap();
    assert!(bed.store.check_sync_read_receipt(&observed).is_err());
    std::fs::write(bed.store.dir.join(RECEIPT), "not JSON").unwrap();
    assert!(bed.store.load(NOW).is_err());
    assert!(bed.store.load_logins(NOW).is_err());
    assert!(bed.store.recover_sync_held(&held, &bed.path).is_err());
}

#[test]
fn sync_commit_rejects_linked_receipt_and_preserves_target() {
    let bed = Bed::new();
    let receipt = bed.store.dir.join(RECEIPT);
    let retained = bed.root.path().join("retained-receipt");
    std::fs::rename(&receipt, &retained).unwrap();
    let before = std::fs::read(&retained).unwrap();
    let target = bed.root.path().join("target");
    std::fs::write(&target, &before).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, bed.store.dir.join(RECEIPT)).unwrap();
    #[cfg(not(unix))]
    std::fs::create_dir(bed.store.dir.join(RECEIPT)).unwrap();
    assert!(bed.store.load(NOW).is_err());
    assert_eq!(std::fs::read(target).unwrap(), before);
    assert_eq!(std::fs::read(retained).unwrap(), before);
}

#[test]
fn sync_receipt_missing_after_retention_refuses_reads() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path());
    store.ensure().unwrap();
    store.retain_witness(&store.dir.join(WITNESS)).unwrap();
    assert!(store.load(1).is_err());
    assert!(store.load_logins(1).is_err());
}

#[test]
fn sync_receipt_valid_json_corruption_does_not_clear_pending_fence() {
    let bed = Bed::new();
    bed.pending();
    let held = bed.store.acquire().unwrap();
    let path = bed.store.dir.join(RECEIPT);
    let mut stored: StoredReceipt =
        aterm_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    stored.receipt.plan = None;
    std::fs::write(&path, aterm_json::to_string(&stored).unwrap()).unwrap();
    assert!(bed.store.load(NOW).unwrap_err().contains("digest"));
    assert!(bed.store.load_logins(NOW).is_err());
    let before = bed.files();
    assert!(bed.store.recover_sync_held(&held, &bed.path).is_err());
    assert_eq!(bed.files(), before);
}

#[test]
fn sync_commit_blocks_direct_ledger_append_and_recovers_without_source_data() {
    let bed = Bed::new();
    bed.pending();
    let before = bed.files();
    assert!(
        bed.store
            .append(&[Event::new(NOW, Kind::Use, "old")])
            .is_err()
    );
    assert_eq!(bed.files(), before);
    let held = bed.store.acquire().unwrap();
    assert_eq!(
        bed.store.recover_sync_within(&[], &bed.path).unwrap_err(),
        LOCK_HELD
    );
    assert_eq!(bed.files(), before);
    drop(held);
    assert!(bed.store.recover_sync_within(&[], &bed.path).unwrap());
    assert_eq!(bed.files(), bed.after());
    assert!(!bed.store.recover_sync_within(&[], &bed.path).unwrap());
}

pub(crate) fn sync_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "AnotherCase" {
        next.insert("case", state["case"] + 1);
        return next;
    }
    assert_eq!(action, "Recover");
    let case = state["case"];
    let bed = Bed::new();
    let old_read = bed.store.sync_read_receipt().unwrap();
    bed.pending();
    let held = bed.store.acquire().unwrap();
    let mut completed = false;
    let mut fenced = bed.store.load(NOW).is_err() && bed.store.load_logins(NOW).is_err();
    let mut preserved = true;
    let mut exact = true;
    let mut once = true;
    let mut epoch = true;
    if case <= 6 {
        let cut = usize::try_from(case - 1).unwrap();
        assert!(
            bed.store
                .recover_sync_with(&held, &bed.path, &mut |step| {
                    if step == cut {
                        Err("interrupted".into())
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        fenced &= bed.store.load(NOW).is_err() && bed.store.load_logins(NOW).is_err();
        completed = bed.store.recover_sync_held(&held, &bed.path).unwrap();
        exact = bed.files() == bed.after();
        once =
            !bed.store.recover_sync_held(&held, &bed.path).unwrap() && bed.files() == bed.after();
        epoch = bed.store.check_sync_read_receipt(&old_read).is_err();
    } else if case == 16 {
        bed.store.recover_sync_held(&held, &bed.path).unwrap();
        epoch = bed.store.check_sync_read_receipt(&old_read).is_err();
    } else {
        let mut target = bed.path.clone();
        match case {
            7..=10 => {
                let path = [
                    bed.store.state_file(),
                    bed.store.logins_file(),
                    bed.store.ledger_path(),
                    bed.path.clone(),
                ][usize::try_from(case - 7).unwrap()]
                .clone();
                std::fs::write(path, "outside edit").unwrap();
            }
            11 => target = target.with_file_name("other.toml"),
            12 => {
                let mut receipt = bed.receipt.clone();
                receipt.plan.as_mut().unwrap().state.after = Some("not JSON".into());
                bed.store.save_receipt(&receipt).unwrap();
            }
            13 => std::fs::write(bed.store.dir.join(RECEIPT), "not JSON").unwrap(),
            14 | 15 => {
                std::fs::rename(
                    bed.store.dir.join(RECEIPT),
                    bed.store.dir.join("receipt-backup"),
                )
                .unwrap();
                if case == 15 {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(
                        bed.store.dir.join("receipt-backup"),
                        bed.store.dir.join(RECEIPT),
                    )
                    .unwrap();
                    #[cfg(not(unix))]
                    std::fs::create_dir(bed.store.dir.join(RECEIPT)).unwrap();
                }
            }
            _ => unreachable!(),
        }
        let before = bed.files();
        completed = bed.store.recover_sync_held(&held, &target).is_ok();
        preserved = bed.files() == before;
    }
    next.insert("phase", 1);
    for (field, value) in [
        ("completed", completed),
        ("fenced", fenced),
        ("preserved", preserved),
        ("exact", exact),
        ("once", once),
        ("epoch", epoch),
    ] {
        next.insert(field, i64::from(value));
    }
    next
}

#[test]
fn sync_commit_native_conforms_to_crash_and_conflict_model() {
    let model = aterm_models_harness::account_sync_commit_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = sync_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let recovered = sync_transition(&model.init_state(), "Recover");
    for (field, law) in [
        ("fenced", "PendingFencesReaders"),
        ("preserved", "OutsideEditsPreserved"),
        ("exact", "AdmittedImagesOnly"),
        ("once", "ReplayIdempotent"),
        ("epoch", "SpanningReadRefused"),
    ] {
        let mut bad = recovered.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut bad = recovered.clone();
    bad.insert("case", 7);
    assert!(!model.check_invariant("ConflictRefused", &bad));
    let mut bad = recovered;
    bad.insert("completed", 0);
    assert!(!model.check_invariant("ValidPrefixCompletes", &bad));
}
