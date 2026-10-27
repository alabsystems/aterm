// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

struct Bed {
    _root: aterm_tempfile::TempDir,
    cli: Cli,
    store: accounts::Store,
    path: PathBuf,
    raw: Roster,
    dirs: [PathBuf; 3],
}

impl Bed {
    fn new(cli: Cli) -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store =
            accounts::Store::at_for(root.path(), cli, &root.path().join("home")).bounded_by(&[]);
        let path = root.path().join(cli.accounts().roster_file);
        let dirs = ["work-live", "next-old", "next-new"].map(|name| root.path().join(name));
        for dir in &dirs {
            let credential = dir.join(cli.connector().stores[0]);
            fs::create_dir_all(credential.parent().unwrap()).unwrap();
            fs::write(credential, b"opaque fixture").unwrap();
        }
        let raw = Roster {
            rows: ["work", "next"]
                .into_iter()
                .map(|label| Row {
                    label: label.into(),
                    email: Some(format!("{label}@example.test")),
                    dir: Some(
                        root.path()
                            .join(format!("{label}-configured"))
                            .to_str()
                            .unwrap()
                            .into(),
                    ),
                    vendor: accounts::Vendor {
                        region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                        ..accounts::Vendor::default()
                    },
                    ..Row::default()
                })
                .collect(),
        };
        fs::write(
            &path,
            accounts::append_rows_for(cli, "", &raw.rows).unwrap(),
        )
        .unwrap();
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                let mut events = accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    None,
                    dirs[0].to_str().unwrap(),
                    None,
                    NOW,
                );
                events.extend(accounts::note_renewed(
                    state,
                    logins,
                    "next",
                    None,
                    dirs[1].to_str().unwrap(),
                    None,
                    NOW,
                ));
                events.extend(accounts::note_login_end(state, logins, "next", NOW));
                events.extend(accounts::note_wall(
                    state,
                    "work",
                    "weekly",
                    Some(NOW + 3600),
                    NOW,
                    NOW,
                ));
                state.active = Some("work".into());
                Ok(events)
            })
            .unwrap();
        Self {
            _root: root,
            cli,
            store,
            path,
            raw,
            dirs,
        }
    }

    fn renew_next(&self) -> Result<Vec<accounts::Event>, String> {
        self.store.transact_logins(NOW, &mut None, |state, logins| {
            let mut events = accounts::note_renewed(
                state,
                logins,
                "next",
                None,
                self.dirs[2].to_str().unwrap(),
                None,
                NOW,
            );
            events.extend(accounts::note_login_end(state, logins, "next", NOW));
            Ok(events)
        })
    }

    fn pending(&self) {
        let witness = self.store.state_witness_file();
        fs::rename(&witness, witness.with_extension("retained")).unwrap();
        fs::create_dir(&witness).unwrap();
        assert!(self.renew_next().is_err());
        fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
        fs::rename(witness.with_extension("retained"), witness).unwrap();
    }

    fn later(&self) {
        let before = fs::read_to_string(&self.path).unwrap();
        let mut raw = self.raw.clone();
        raw.rows[1].email = Some("changed@example.test".into());
        let after = accounts::append_rows_for(self.cli, "", &raw.rows).unwrap();
        let held = self.store.acquire().unwrap();
        self.store
            .transact_sync_held(
                &held,
                NOW + 1,
                &mut None,
                accounts::SyncTarget {
                    path: &self.path,
                    before: Some(&before),
                    after: Some(&after),
                    mode: None,
                },
                |state, logins| {
                    let mut events = accounts::note_renewed(
                        state,
                        logins,
                        "next",
                        None,
                        self.dirs[2].to_str().unwrap(),
                        None,
                        NOW + 1,
                    );
                    events.extend(accounts::note_wall(
                        state,
                        "next",
                        "weekly",
                        Some(NOW + 3600),
                        NOW + 1,
                        NOW + 1,
                    ));
                    Ok(events)
                },
                || Ok(()),
            )
            .unwrap();
    }

    fn records(&self) -> [Option<Vec<u8>>; 7] {
        [
            self.path.clone(),
            self.store.state_file(),
            self.store.logins_file(),
            self.store.state_witness_file(),
            self.store.logins_witness_file(),
            self.store.ledger_path(),
            self.store.dir.join("sync-commit.json"),
        ]
        .map(|path| fs::read(path).ok())
    }
}

fn case(provider: i64, fault: i64, later: bool) -> (bool, bool, bool) {
    let cli =
        [Cli::Claude, Cli::Codex, Cli::Kimi, Cli::Grok][usize::try_from(provider - 1).unwrap()];
    let bed = Bed::new(cli);
    match fault {
        1 | 6 => bed.pending(),
        2 => fs::write(&bed.path, "[[account]").unwrap(),
        3 => fs::write(bed.store.logins_file(), "{").unwrap(),
        4 => fs::write(bed.store.state_file(), "{").unwrap(),
        5 => fs::write(bed.store.dir.join("sync-commit.json"), "{").unwrap(),
        _ => {}
    }
    let _held = (fault == 6).then(|| bed.store.acquire().unwrap());
    let before = bed.records();
    let result = read_renewal_candidates(cli, &bed.store, &bed.path, NOW);
    if fault != 1 {
        assert_eq!(bed.records(), before, "only admitted recovery may publish");
    }
    let (available, admitted, retained) = match result {
        Err(_) => (false, false, true),
        Ok(candidates) => {
            let captured = candidates.records.clone();
            let ranked = candidates.tiered.clone();
            assert_eq!(captured.raw_roster, bed.raw);
            assert_eq!(ranked.len(), 1);
            assert_eq!(ranked[0].0, 0);
            assert_eq!(ranked[0].1.label, "next");
            assert_eq!(ranked[0].1, *captured.roster.row("next").unwrap());
            assert_eq!(
                ranked[0].1.dir.as_deref(),
                bed.dirs[if fault == 1 { 2 } else { 1 }].to_str()
            );
            assert_ne!(captured.raw_roster.row("next"), Some(&ranked[0].1));
            assert!(
                bed.store
                    .admit_renewal_attempt(
                        accounts::RenewalPolicy {
                            roster_path: &bed.path,
                            roster: &captured.roster,
                            row: &ranked[0].1,
                            logins: &captured.logins,
                        },
                        None,
                        NOW,
                    )
                    .is_err(),
                "the effective roster must not substitute for the raw policy"
            );
            if later {
                bed.later();
                let fresh = read_renewal_candidates(cli, &bed.store, &bed.path, NOW + 1).unwrap();
                assert!(fresh.tiered.iter().all(|(tier, _)| *tier >= UNNEEDED_TIER));
                assert_ne!(fresh.records.raw_roster, captured.raw_roster);
            }
            let admitted = bed
                .store
                .admit_renewal_attempt(
                    accounts::RenewalPolicy {
                        roster_path: &bed.path,
                        roster: &captured.raw_roster,
                        row: &ranked[0].1,
                        logins: &captured.logins,
                    },
                    None,
                    NOW,
                )
                .unwrap_or(false);
            (
                true,
                admitted,
                candidates.records == captured && candidates.tiered == ranked,
            )
        }
    };
    for dir in &bed.dirs {
        assert_eq!(
            fs::read(dir.join(cli.connector().stores[0])).unwrap(),
            b"opaque fixture"
        );
    }
    (available, admitted, retained)
}

pub(crate) fn renewal_read_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "LaterChange" => {
            next.insert("later", 1);
        }
        "Read" => {
            let (available, admitted, retained) =
                case(state["provider"], state["fault"], state["later"] == 1);
            next.insert("phase", 1);
            next.insert("available", i64::from(available));
            next.insert("admitted", i64::from(admitted));
            next.insert("retained", i64::from(retained));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn renewal_candidates_keep_one_publication_and_recheck_worker_policy() {
    let model = aterm_models_harness::account_renewal_read_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3, 4, 5]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = renewal_read_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                cases += usize::from(action.name == "Read");
            }
        }
    }
    assert_eq!(cases, 56);
    let good = renewal_read_transition(&model.init_state(), "Read");
    for (field, value, law) in [
        ("available", 0, "RecoveredCandidatesAvailable"),
        ("admitted", 0, "CapturedPolicyAdmissible"),
        ("retained", 0, "CapturedRankingRetained"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut bad = good;
    bad.insert("fault", 2);
    assert!(!model.check_invariant("UnknownCandidatesRefused", &bad));
    assert!(!model.check_invariant("ChangedPolicyRefused", &bad));
}

#[test]
fn renewal_admission_checks_policy_and_generation_but_allows_unrelated_budget_updates() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Kimi, Cli::Grok] {
        for change in 0..3 {
            let bed = Bed::new(cli);
            let captured = read_renewal_candidates(cli, &bed.store, &bed.path, NOW).unwrap();
            match change {
                0 => {
                    let mut raw = bed.raw.clone();
                    raw.rows[1].email = Some("changed@example.test".into());
                    fs::write(
                        &bed.path,
                        accounts::append_rows_for(cli, "", &raw.rows).unwrap(),
                    )
                    .unwrap();
                }
                1 => {
                    bed.renew_next().unwrap();
                }
                2 => {
                    bed.store
                        .transact(NOW + 1, &mut None, |state| {
                            Ok(accounts::note_wall(
                                state,
                                "work",
                                "weekly",
                                Some(NOW + 7200),
                                NOW + 1,
                                NOW + 1,
                            ))
                        })
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let admitted = bed
                .store
                .admit_renewal_attempt(
                    accounts::RenewalPolicy {
                        roster_path: &bed.path,
                        roster: &captured.records.raw_roster,
                        row: &captured.tiered[0].1,
                        logins: &captured.records.logins,
                    },
                    None,
                    NOW + 1,
                )
                .unwrap_or(false);
            assert_eq!(admitted, change == 2, "{cli:?} change={change}");
        }
    }
}
