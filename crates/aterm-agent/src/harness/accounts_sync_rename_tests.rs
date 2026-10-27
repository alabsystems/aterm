// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use accounts::rename_tests::{NOW, seed_reference};

struct Bed {
    root: aterm_tempfile::TempDir,
    store: accounts::Store,
    path: PathBuf,
    fleet: FleetRoster,
}

impl Bed {
    fn new() -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = accounts::Store::at(&root.path().join("state"));
        let path = root.path().join("accounts.toml");
        let old = Roster {
            rows: vec![Row {
                label: "m1".into(),
                email: Some("owner@example.test".into()),
                ..Row::default()
            }],
        };
        fs::write(&path, format_roster_toml(&old).unwrap()).unwrap();
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                seed_reference(state, logins, "m1", 0);
                Ok(Vec::new())
            })
            .unwrap();
        let fleet = parse_accounting_csv(
            "email,computer,claude\nreplacement@example.test,m1,1\nowner@example.test,m1,1\n",
            "fixture.csv",
        )
        .unwrap();
        assert_eq!(
            build_machine_roster(&fleet, "m1", &store, Some(&old), Some("owner@example.test"))
                .row_for_login(None)
                .unwrap()
                .label,
            "m1-2"
        );
        fs::write(root.path().join("source.toml"), "# retained source\n").unwrap();
        Self {
            root,
            store,
            path,
            fleet,
        }
    }

    fn sync(&self) -> Result<SyncedAccounts, String> {
        let source = self.root.path().join("source.toml");
        persist_accounts_sync(
            &self.store,
            &self.path,
            (&self.fleet, OptIn::Named),
            ("m1", Some("owner@example.test")),
            &BTreeMap::new(),
            NOW,
            (
                &[],
                &[FleetSnapshot {
                    path: source.clone(),
                    bytes: Some(fs::read(source).unwrap()),
                    publish: Publish::Fleet,
                }],
            ),
        )
    }

    fn records(&self) -> Vec<Option<Vec<u8>>> {
        [
            self.path.clone(),
            self.root.path().join(FLEET_ROSTER_FILE),
            self.root.path().join("source.toml"),
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.store.wake_path(),
            self.store.state_witness_file(),
            self.store.logins_witness_file(),
        ]
        .map(|path| fs::read(path).ok())
        .into()
    }
}

#[test]
fn alias_identity_survives_a_new_fleet_label_without_a_duplicate_default() {
    let mut bed = Bed::new();
    let mut old = Roster::load(&bed.path).unwrap();
    old.rows[0].email = Some("alias@example.test".into());
    old.rows[0].identity_email = Some("owner@example.test".into());
    fs::write(&bed.path, format_roster_toml(&old).unwrap()).unwrap();
    bed.fleet.accounts[1].email = "alias@example.test".into();
    let roster = build_machine_roster(
        &bed.fleet,
        "m1",
        &bed.store,
        Some(&old),
        Some("owner@example.test"),
    );
    assert_eq!(
        roster.rows.len(),
        2,
        "a relabel is not a new canonical account"
    );
    let row = roster.row_for_login(None).unwrap();
    assert_eq!(row.label, "m1-2");
    assert_eq!(row.email.as_deref(), Some("alias@example.test"));
    assert_eq!(row.identity_email.as_deref(), Some("owner@example.test"));
    let before = bed.store.load(NOW).unwrap().0;
    bed.sync().unwrap();
    let after = bed.store.load(NOW).unwrap().0;
    assert_eq!(
        after.to_json(),
        before.to_json().replace("\"m1\"", "\"m1-2\"")
    );
}

#[test]
fn default_alias_move_keeps_state_without_a_live_profile_or_reused_source_label() {
    for reused in [false, true] {
        let mut bed = Bed::new();
        let mut old = Roster::load(&bed.path).unwrap();
        old.rows[0].email = Some("alias@example.test".into());
        old.rows[0].identity_email = Some("owner@example.test".into());
        fs::write(&bed.path, format_roster_toml(&old).unwrap()).unwrap();
        bed.fleet.accounts[1].email = "alias@example.test".into();
        if !reused {
            bed.fleet.accounts.remove(0);
        }
        let before = bed.store.load(NOW).unwrap().0;
        let logins = bed.store.load_logins(NOW).unwrap().0;
        let parent = accounts::RenewalParent::capture(&logins, "m1");
        persist_accounts_sync(
            &bed.store,
            &bed.path,
            (&bed.fleet, OptIn::Named),
            ("m1", None),
            &BTreeMap::new(),
            NOW,
            (&[], &[]),
        )
        .unwrap();
        let after = bed.store.load(NOW).unwrap().0;
        assert_eq!(
            after.to_json(),
            before.to_json().replace("\"m1\"", "\"m1-2\"")
        );
        let bound = bed.store.load_logins(NOW).unwrap().0;
        assert!(parent.require_current(&bound, "m1").is_err());
        assert!(bound.labels["m1-2"].renewal_generation > logins.generation_clock);
        let current = Roster::load(&bed.path).unwrap();
        let default = current.row_for_login(None).unwrap();
        assert_eq!(default.label, "m1-2");
        assert_eq!(
            default.identity_email.as_deref(),
            Some("owner@example.test")
        );
    }
}

#[test]
fn alias_relabel_refuses_an_occupied_roster_target_before_publication() {
    let mut bed = Bed::new();
    let mut old = Roster::load(&bed.path).unwrap();
    old.rows[0].email = Some("alias@example.test".into());
    old.rows[0].identity_email = Some("owner@example.test".into());
    old.rows.push(Row {
        label: "m1-2".into(),
        email: Some("replacement@example.test".into()),
        dir: Some(
            bed.root
                .path()
                .join("retained-login")
                .to_string_lossy()
                .into_owned(),
        ),
        ..Row::default()
    });
    fs::write(&bed.path, format_roster_toml(&old).unwrap()).unwrap();
    bed.fleet.accounts[1].email = "alias@example.test".into();
    let before = bed.records();
    let why = bed.sync().err().expect("old roster target is occupied");
    assert!(why.contains("previous roster already uses"), "{why}");
    assert_eq!(bed.records(), before);
    assert!(!bed.store.sync_needs_recovery());
}

#[test]
fn full_fleet_refuses_to_assign_the_known_default_login_to_another_account() {
    let mut bed = Bed::new();
    let mut old = Roster::load(&bed.path).unwrap();
    old.rows[0].email = Some("alias@example.test".into());
    old.rows[0].identity_email = Some("owner@example.test".into());
    fs::write(&bed.path, format_roster_toml(&old).unwrap()).unwrap();
    let csv = format!(
        "email,computer,claude\n{}",
        (0..accounts::MAX_ACCOUNTS)
            .map(|index| format!("other-{index}@example.test,m1,1\n"))
            .collect::<String>()
    );
    bed.fleet = parse_accounting_csv(&csv, "full.csv").unwrap();
    let before = bed.records();
    for hint in [None, Some("owner@example.test")] {
        let why = persist_accounts_sync(
            &bed.store,
            &bed.path,
            (&bed.fleet, OptIn::Named),
            ("m1", hint),
            &BTreeMap::new(),
            NOW,
            (&[], &[]),
        )
        .err()
        .expect("full roster cannot displace the known default");
        assert!(why.contains("known default login"), "{why}");
        assert_eq!(bed.records(), before);
    }
}

#[test]
fn alias_relabel_can_return_to_an_old_label_without_reviving_old_parents() {
    let mut bed = Bed::new();
    bed.fleet.accounts.remove(0);
    let initial = bed.store.load_logins(NOW).unwrap().0;
    let old = accounts::RenewalParent::capture(&initial, "m1");
    bed.sync().unwrap();
    let middle = bed.store.load_logins(NOW).unwrap().0;
    let moved = accounts::RenewalParent::capture(&middle, "m1-2");
    bed.fleet.accounts[0].label = "m1".into();
    bed.sync().unwrap();
    let final_logins = bed.store.load_logins(NOW).unwrap().0;
    assert!(old.require_current(&final_logins, "m1").is_err());
    assert!(moved.require_current(&final_logins, "m1-2").is_err());
    assert!(final_logins.labels["m1"].renewal_generation > middle.generation_clock);
    assert_eq!(bed.store.load(NOW).unwrap().0.active.as_deref(), Some("m1"));
}

#[test]
fn a_later_sync_cannot_reassign_a_moved_accounts_retained_directory() {
    for historical in [false, true] {
        let mut bed = Bed::new();
        let path = bed
            .store
            .login_dir("m1")
            .unwrap()
            .to_string_lossy()
            .into_owned();
        bed.store
            .transact_logins(NOW, &mut None, |_, logins| {
                let binding = logins.labels.get_mut("m1").unwrap();
                if historical {
                    binding.was.push(Some(path.clone()));
                } else {
                    binding.dir = Some(path.clone());
                }
                Ok(Vec::new())
            })
            .unwrap();
        let replacement = bed.fleet.accounts.remove(0);
        bed.sync().unwrap();
        bed.fleet.accounts.insert(0, replacement);
        let before = bed.records();
        let why = bed
            .sync()
            .err()
            .expect("a later source-label reuse retains directory ownership");
        assert!(why.contains("directory is retained"), "{why}");
        assert_eq!(bed.records(), before);
    }
}

#[test]
fn inherited_alias_identity_conflict_refuses_without_publication() {
    let mut bed = Bed::new();
    let mut old = Roster::load(&bed.path).unwrap();
    old.rows[0].email = Some("alias@example.test".into());
    old.rows[0].identity_email = Some("owner@example.test".into());
    fs::write(&bed.path, format_roster_toml(&old).unwrap()).unwrap();
    bed.fleet.accounts[0].email = "alias@example.test".into();
    let before = bed.records();
    let why = bed
        .sync()
        .err()
        .expect("two rows cannot claim the canonical identity");
    assert!(why.contains("expect one provider identity"), "{why}");
    assert_eq!(bed.records(), before);
}

#[test]
fn a_new_observed_default_does_not_leave_the_alias_using_the_default_profile() {
    let mut bed = Bed::new();
    let mut old = Roster::load(&bed.path).unwrap();
    old.rows[0].email = Some("alias@example.test".into());
    old.rows[0].identity_email = Some("owner@example.test".into());
    fs::write(&bed.path, format_roster_toml(&old).unwrap()).unwrap();
    bed.fleet.accounts[0].email = "alias@example.test".into();
    bed.fleet.accounts[1].email = "new@example.test".into();
    persist_accounts_sync(
        &bed.store,
        &bed.path,
        (&bed.fleet, OptIn::Named),
        ("m1", Some("new@example.test")),
        &BTreeMap::new(),
        NOW,
        (&[], &[]),
    )
    .unwrap();
    let roster = Roster::load(&bed.path).unwrap();
    assert_eq!(roster.row_for_login(None).unwrap().label, "m1-2");
    assert!(roster.row("m1").unwrap().dir.is_some());
    assert_eq!(
        bed.store.load(NOW).unwrap().0.account("m1").auth,
        accounts::Auth::SignedOut
    );
}

#[test]
fn a_legacy_row_without_login_identity_preserves_retained_state_on_assignment() {
    for hint in [None, Some("owner@example.test")] {
        let mut bed = Bed::new();
        let mut old = Roster::load(&bed.path).unwrap();
        old.rows[0].email = None;
        fs::write(&bed.path, format_roster_toml(&old).unwrap()).unwrap();
        bed.fleet.accounts.remove(0);
        bed.fleet.accounts[0].label = "m1".into();
        bed.store
            .transact_logins(NOW, &mut None, |_, logins| {
                logins.labels.get_mut("m1").unwrap().dir = Some(
                    bed.root
                        .path()
                        .join("private-login")
                        .to_string_lossy()
                        .into_owned(),
                );
                Ok(Vec::new())
            })
            .unwrap();
        let before = bed.records();
        let why = persist_accounts_sync(
            &bed.store,
            &bed.path,
            (&bed.fleet, OptIn::Named),
            ("m1", hint),
            &BTreeMap::new(),
            NOW,
            (&[], &[]),
        )
        .err()
        .expect("unknown prior identity retains its authority");
        assert!(why.contains("retained legacy row"), "{why}");
        assert_eq!(bed.records(), before);
        bed.store
            .transact_logins(NOW, &mut None, |state, logins| {
                *state = accounts::State::default();
                *logins = accounts::Logins::default();
                Ok(Vec::new())
            })
            .unwrap();
        persist_accounts_sync(
            &bed.store,
            &bed.path,
            (&bed.fleet, OptIn::Named),
            ("m1", hint),
            &BTreeMap::new(),
            NOW,
            (&[], &[]),
        )
        .expect("a legacy row with no retained authority can be assigned");
    }
}

#[test]
fn rename_generation_exhaustion_leaves_every_publication_unchanged() {
    let bed = Bed::new();
    let mut logins = bed.store.load_logins(NOW).unwrap().0;
    logins.generation_clock = u64::MAX;
    bed.store.save_logins(&logins).unwrap();
    let before = bed.records();
    let why = bed
        .sync()
        .err()
        .expect("generation exhaustion refuses rename");
    assert!(why.contains("exhaust"), "{why}");
    assert_eq!(bed.records(), before, "validation must precede publication");
}

#[cfg(unix)]
#[test]
fn rename_sync_refuses_fleet_output_aliased_to_account_authority() {
    let bed = Bed::new();
    std::os::unix::fs::symlink(
        bed.store.state_file(),
        bed.root.path().join(FLEET_ROSTER_FILE),
    )
    .unwrap();
    let before = bed.records();
    let why = bed
        .sync()
        .err()
        .expect("fleet output cannot replace account state");
    assert!(why.contains("overlaps"), "{why}");
    assert_eq!(bed.records(), before);
    assert!(!bed.store.sync_needs_recovery(), "no intent was published");
}

#[test]
fn rename_sync_resumes_after_wake_failure_without_repeating_the_move() {
    let bed = Bed::new();
    fs::create_dir(bed.store.wake_path()).unwrap();
    let why = bed
        .sync()
        .err()
        .expect("wake obstruction interrupts commit");
    assert!(why.contains("publication did not complete"), "{why}");
    assert!(bed.store.load(NOW).is_err());
    assert!(bed.store.load_logins(NOW).is_err());
    assert!(
        sync_called_for(
            bed.root.path(),
            bed.store.dir.parent().unwrap(),
            (&bed.path, &bed.root.path().join("aterm.toml")),
            NOW,
            &std::sync::Mutex::new(Rediscovery::default()),
        ),
        "an interrupted commit schedules recovery even when sources did not change"
    );
    let admitted_logins = fs::read(bed.store.logins_file()).unwrap();
    fs::rename(
        bed.store.wake_path(),
        bed.root.path().join("wake-obstruction"),
    )
    .unwrap();
    assert!(
        bed.sync().unwrap().changed,
        "recovery reports its publication"
    );
    assert_eq!(
        fs::read(bed.store.logins_file()).unwrap(),
        admitted_logins,
        "recovery reuses the admitted generations"
    );
    let state = bed.store.load(NOW).unwrap().0;
    assert!(state.accounts.contains_key("m1-2"));
    assert!(!state.accounts.contains_key("m1"));
    let receipt = fs::read(bed.store.dir.join("sync-commit.json")).unwrap();
    assert!(!bed.sync().unwrap().changed);
    assert_eq!(
        fs::read(bed.store.dir.join("sync-commit.json")).unwrap(),
        receipt
    );
}

#[test]
fn default_account_rename_refuses_every_retained_destination_before_publication() {
    for source_empty in [false, true] {
        for kind in 1..=10 {
            let bed = Bed::new();
            bed.store
                .transact_logins(NOW, &mut None, |state, logins| {
                    if source_empty {
                        *state = accounts::State::default();
                        *logins = accounts::Logins::default();
                    }
                    seed_reference(state, logins, "m1-2", kind);
                    Ok(Vec::new())
                })
                .unwrap();
            let state = bed.store.load(NOW).expect("retained state is valid").0;
            let logins = bed
                .store
                .load_logins(NOW)
                .expect("retained binding is valid")
                .0;
            assert!(
                format!("{}{}", state.to_json(), logins.to_json()).contains("\"m1-2\""),
                "{kind}: fixture retained its target"
            );
            let before = bed.records();
            let why = bed.sync().err().unwrap_or_else(|| {
                panic!("{kind}, empty={source_empty}: retained destination must refuse")
            });
            assert!(
                why.contains("before roster publication") && why.contains("retains"),
                "{kind}: {why}"
            );
            assert_eq!(
                bed.records(),
                before,
                "{kind}: all source and account files stay intact"
            );
            assert!(
                !bed.store.login_dir("m1").unwrap().exists(),
                "no profile created on refusal"
            );
            assert!(
                bed.store.try_acquire().unwrap().is_some(),
                "writer released"
            );
        }
    }
}

#[test]
fn default_account_rename_carries_identity_state_and_fences_previous_generation() {
    let bed = Bed::new();
    let before = bed.store.load(NOW).unwrap().0;
    let old_logins = bed.store.load_logins(NOW).unwrap().0;
    let old_parent = accounts::RenewalParent::capture(&old_logins, "m1");
    let absent_parent = accounts::RenewalParent::capture(&old_logins, "m1-2");
    let saved = bed.sync().expect("unoccupied target admits relocation");
    assert!(saved.changed);
    let after = bed.store.load(NOW).unwrap().0;
    assert_eq!(
        after.to_json(),
        before.to_json().replace("\"m1\"", "\"m1-2\"")
    );
    let logins = bed.store.load_logins(NOW).unwrap().0;
    let mut carried = old_logins.labels["m1"].clone();
    let new_generation = logins.labels["m1-2"].renewal_generation;
    assert!(new_generation > old_logins.generation_clock);
    carried.renewal_generation = new_generation;
    assert_eq!(logins.labels.get("m1-2"), Some(&carried));
    assert!(!logins.labels.contains_key("m1"));
    assert!(!old_parent.matches(&logins, "m1"));
    assert!(!absent_parent.matches(&logins, "m1-2"));
    let roster = Roster::load(&bed.path).unwrap();
    assert_eq!(roster.row_for_login(None).unwrap().label, "m1-2");
    assert_eq!(
        roster.row("m1").unwrap().email.as_deref(),
        Some("replacement@example.test")
    );
}

#[test]
fn default_account_rename_retains_damaged_or_missing_snapshots() {
    for (logins, missing) in [(false, false), (true, false), (false, true), (true, true)] {
        let bed = Bed::new();
        let path = if logins {
            bed.store.logins_file()
        } else {
            bed.store.state_file()
        };
        if missing {
            fs::rename(path, bed.root.path().join("kept-snapshot")).unwrap();
        } else {
            fs::write(path, "{\"schema\":999}").unwrap();
        }
        let before = bed.records();
        assert!(bed.sync().is_err());
        assert_eq!(bed.records(), before);
        assert!(bed.store.try_acquire().unwrap().is_some());
    }
}

#[test]
fn renamed_accounts_refuse_stale_completions_for_the_reused_label() {
    let bed = Bed::new();
    let state = bed.store.load(NOW).unwrap().0;
    let request = state.pending_pin.clone().unwrap();
    let reset = state.reset.clone().unwrap();
    let old_row = accounts::bound(
        &Roster::load(&bed.path).unwrap(),
        &bed.store.load_logins(NOW).unwrap().0,
    )
    .row("m1")
    .unwrap()
    .clone();
    let who = accounts::ProcessLogin {
        login: old_row.dir.clone(),
        default_home: true,
        stored_login: true,
    };
    bed.sync().unwrap();
    let roster = Roster::load(&bed.path).unwrap();
    let mut current = bed.store.load(NOW).unwrap().0;
    let before = current.clone();
    assert!(
        accounts::complete_requested_pin(&mut current, &roster, &BTreeSet::new(), &request, NOW)
            .is_none()
    );
    assert!(
        accounts::reset_outcome(
            &mut current,
            "m1",
            reset.until.unwrap(),
            &reset.sid,
            &accounts::ResetDone::Used {
                kind: aterm_phase::ResetKind::Full,
                left: Some(0)
            },
            NOW
        )
        .is_empty()
    );
    assert_eq!(current, before);
    let records = bed.records();
    let mut called = false;
    assert!(
        bed.store
            .observe_login_within(
                &[],
                NOW,
                accounts::LoginObservation {
                    roster_path: &bed.path,
                    row: &old_row,
                    who: &who,
                },
                |_, _| {
                    called = true;
                    Ok(Vec::new())
                }
            )
            .is_err()
    );
    assert!(!called);
    assert_eq!(bed.records(), records);
    let current_request = current.pending_pin.clone().unwrap();
    assert_eq!(current_request.label(), "m1-2");
    assert!(
        accounts::complete_requested_pin(
            &mut current,
            &roster,
            &BTreeSet::from(["m1-2".into()]),
            &current_request,
            NOW
        )
        .is_some()
    );
    assert_eq!(current.active.as_deref(), Some("m1-2"));
}

#[test]
fn a_rename_cannot_reuse_a_directory_kept_by_the_moved_login() {
    for kind in 0..3 {
        if kind == 2 && !cfg!(unix) {
            continue;
        }
        let bed = Bed::new();
        let path = bed.store.login_dir("m1").unwrap();
        let kept = if kind == 2 {
            let kept = bed.root.path().join("kept-login");
            fs::create_dir_all(&kept).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&kept, &path).unwrap();
            kept
        } else {
            path.clone()
        };
        let existed = path.exists();
        bed.store
            .transact_logins(NOW, &mut None, |_, logins| {
                let binding = logins.labels.get_mut("m1").unwrap();
                if kind == 1 {
                    binding.was.push(Some(kept.to_str().unwrap().into()));
                } else {
                    binding.dir = Some(kept.to_str().unwrap().into());
                }
                Ok(Vec::new())
            })
            .unwrap();
        let before = bed.records();
        let why = bed
            .sync()
            .err()
            .expect("a retained directory cannot be reassigned");
        assert!(
            why.contains("before roster publication") && why.contains("directory is retained"),
            "{why}"
        );
        assert_eq!(bed.records(), before);
        assert_eq!(path.exists(), existed);
    }
}

#[test]
fn a_failed_auxiliary_publication_keeps_the_rename_visible_for_retry() {
    let bed = Bed::new();
    let fleet_path = bed.root.path().join(FLEET_ROSTER_FILE);
    fs::create_dir(&fleet_path).unwrap();
    let before = bed.records();
    assert!(bed.sync().is_err());
    assert_eq!(
        bed.records(),
        before,
        "the old roster and account snapshots retain the rename edge"
    );
    fs::rename(fleet_path, bed.root.path().join("kept-blocked-fleet")).unwrap();
    bed.sync()
        .expect("the rename still applies after the obstruction moves aside");
    let state = bed.store.load(NOW).unwrap().0;
    assert_eq!(state.active.as_deref(), Some("m1-2"));
    assert!(!state.accounts.contains_key("m1"));
}
