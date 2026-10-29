// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeSet;

use aterm_spec::interp::State as ModelState;

use super::*;

const TARGET: &str = "target@example.test";
const COMPANION: &str = "companion";

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    files: BTreeMap<PathBuf, Vec<u8>>,
    directories: BTreeSet<PathBuf>,
}

impl Snapshot {
    fn read(root: &Path, store: &accounts::Store) -> Self {
        let locks = [
            store.dir.join("accounts.lock"),
            store.shared_dir().join(accounts::SIGNIN_LOCK_FILE),
        ];
        let mut files = BTreeMap::new();
        let mut directories = BTreeSet::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    directories.insert(path.clone());
                    pending.push(path);
                } else if !locks.contains(&path) {
                    files.insert(path.clone(), fs::read(path).unwrap());
                }
            }
        }
        Self { files, directories }
    }
}

#[derive(Clone, Copy)]
struct Representation {
    layout: u8,
    reverse_local: bool,
    reverse_source: bool,
    ambiguous_peer: bool,
    replan: bool,
}

fn represented_sync(
    state: &ModelState,
    representation: Representation,
) -> [(&'static str, i64); 7] {
    let bed = Bed::new("alias-admission");
    let store = accounts::Store::at_for(&bed.state, Cli::Codex, &bed.home);
    let vendor = accounts::Vendor {
        region: None,
        flow: Some(Flow::Browser),
    };
    let row = |label: &str, email: Option<&str>, identity: &str| Row {
        label: label.into(),
        class: Class::Dedicated,
        dir: Some(
            bed.home
                .join(format!("retained-{label}"))
                .to_string_lossy()
                .into_owned(),
        ),
        email: email.map(str::to_owned),
        identity_email: Some(identity.into()),
        vendor,
        ..Row::default()
    };
    let candidates = state["candidates"];
    let mut originals = if candidates == 0 {
        vec![row(
            "alpha",
            Some("unrelated@example.test"),
            "owner-alpha@example.test",
        )]
    } else {
        match representation.layout {
            0 => vec![row(
                "alpha",
                Some("alias@example.test"),
                "TARGET@EXAMPLE.TEST",
            )],
            1 => vec![row("alpha", Some(TARGET), "owner-alpha@example.test")],
            2 => vec![row("alpha", Some(TARGET), TARGET)],
            3 => vec![row("alpha", None, TARGET)],
            4 => vec![
                row("alpha", Some(TARGET), "bridge@example.test"),
                row(
                    "beta",
                    Some("bridge@example.test"),
                    "owner-beta@example.test",
                ),
            ],
            5 => vec![
                row("alpha", Some("bridge@example.test"), TARGET),
                row("beta", Some("alias@example.test"), "bridge@example.test"),
            ],
            _ => unreachable!(),
        }
    };
    if candidates == 2 {
        originals.push(row("beta", Some(TARGET), "owner-beta@example.test"));
    }
    if state["label_kind"] == 3 {
        originals.push(row(
            "gamma",
            Some("gamma@example.test"),
            "owner-gamma@example.test",
        ));
    }
    if representation.reverse_local {
        originals.reverse();
    }
    let roster_path = bed.roster(Cli::Codex);
    let desired_text = accounts::append_rows_for(Cli::Codex, "", &originals).unwrap();
    let initial_text = if representation.replan {
        let mut initial = originals.clone();
        initial
            .iter_mut()
            .find(|row| row.label == "beta")
            .unwrap()
            .email = Some("before-replan@example.test".into());
        accounts::append_rows_for(Cli::Codex, "", &initial).unwrap()
    } else {
        desired_text.clone()
    };
    fs::write(&roster_path, initial_text).unwrap();
    let mut local_state = accounts::State {
        machine: "m16".into(),
        active: Some("alpha".into()),
        ..accounts::State::default()
    };
    let mut logins = accounts::Logins {
        generation_clock: 7,
        ..accounts::Logins::default()
    };
    let mut credential_paths = Vec::new();
    for old in &originals {
        let directory = Path::new(old.dir.as_deref().unwrap());
        fs::create_dir_all(directory).unwrap();
        let credential = directory.join("retained-credential");
        fs::write(&credential, format!("inert credential for {}", old.label)).unwrap();
        credential_paths.push(credential);
        local_state.accounts.insert(
            old.label.clone(),
            accounts::Account {
                auth: accounts::Auth::Ok,
                ..accounts::Account::default()
            },
        );
        logins.labels.insert(
            old.label.clone(),
            accounts::Binding {
                dir: old.dir.clone(),
                renewal_generation: 7,
                renewed_at: Some(NOW - 60),
                ..accounts::Binding::default()
            },
        );
    }
    store.save(&local_state).unwrap();
    store.save_logins(&logins).unwrap();
    fs::create_dir_all(store.logins_dir()).unwrap();
    fs::write(store.wake_path(), "retained wake").unwrap();
    drop(
        store
            .signin_now()
            .unwrap()
            .expect("fixture sign-in lock is free"),
    );
    let primary = match state["label_kind"] {
        0 => "fresh",
        1 => "alpha",
        2 => "beta",
        3 => "gamma",
        _ => unreachable!(),
    };
    let account = |label: &str, email: &str| FleetAccount {
        label: label.into(),
        email: email.into(),
        machines: vec!["m16".into()],
        class: Class::Dedicated,
        browser: None,
        vendor: accounts::Vendor {
            region: None,
            flow: Some(if state["same_vendor"] == 1 {
                Flow::Browser
            } else {
                Flow::Device
            }),
        },
    };
    let mut incoming = vec![account(primary, "Target@Example.Test")];
    if representation.ambiguous_peer {
        incoming.push(account(
            if primary == "alpha" { "beta" } else { "alpha" },
            TARGET,
        ));
    }
    if state["companion"] == 1 {
        incoming.push(account(COMPANION, "companion@example.test"));
    }
    if representation.reverse_source {
        incoming.reverse();
    }
    bed.secret(
        &cli_fleet_file(Cli::Codex),
        &FleetRoster {
            source: "fixture".into(),
            accounts: incoming.clone(),
        }
        .to_toml(),
    );

    let mut before = Snapshot::read(bed._dir.path(), &store);
    let mut replanned = false;
    let first = if representation.replan {
        sync_cli_with_replan(
            Cli::Codex,
            (&bed.home, &bed.state, &bed.aterm_toml()),
            (None, None),
            NOW,
            (&read_named, &[]),
            || {
                let _writer = store
                    .acquire_within(&[])
                    .unwrap()
                    .expect("the first writer was released before replanning");
                fs::write(&roster_path, &desired_text).unwrap();
                before = Snapshot::read(bed._dir.path(), &store);
                replanned = true;
            },
        )
    } else {
        bed.sync(Cli::Codex, None)
    };
    assert_eq!(replanned, representation.replan);
    let report = match &first {
        Ok(CliSync::Synced(report)) => Some(report),
        _ => None,
    };
    let published = Roster::load_for(Cli::Codex, &roster_path).ok();
    let source_name = bed
        .secrets()
        .join(cli_fleet_file(Cli::Codex))
        .display()
        .to_string();
    let valid_report = |report: &CliSynced| {
        let additions: BTreeSet<_> = report.added.iter().collect();
        let kept: BTreeSet<_> = report.kept.iter().collect();
        report.cli == Cli::Codex
            && report.machine == "m16"
            && report.roster == roster_path
            && report.source == source_name
            && report.fleet_accounts == incoming.len()
            && published
                .as_ref()
                .is_some_and(|roster| report.rows == roster.rows.len())
            && report.held.is_empty()
            && additions.len() == report.added.len()
            && kept.len() == report.kept.len()
            && additions.is_disjoint(&kept)
            && report
                .added
                .iter()
                .chain(&report.kept)
                .all(|label| incoming.iter().any(|account| &account.label == label))
            && report.kept.iter().all(|label| label != COMPANION)
            && (!representation.ambiguous_peer
                || report
                    .kept
                    .iter()
                    .any(|label| label == if primary == "alpha" { "beta" } else { "alpha" }))
            && (!representation.ambiguous_peer
                || !report
                    .added
                    .iter()
                    .any(|label| label == if primary == "alpha" { "beta" } else { "alpha" }))
    };
    let count = |labels: &[String], label: &str| {
        i64::try_from(
            labels
                .iter()
                .filter(|candidate| candidate.as_str() == label)
                .count(),
        )
        .unwrap()
    };
    let added = report.map_or(0, |report| count(&report.added, primary));
    let kept = report.map_or(0, |report| count(&report.kept, primary));
    let extra = report.map_or(0, |report| count(&report.added, COMPANION));
    let mut expected_rows = originals.clone();
    let mut expected_directories = before.directories.clone();
    if let Some(report) = report {
        for label in &report.added {
            let account = incoming
                .iter()
                .find(|account| &account.label == label)
                .unwrap();
            let directory = store.login_dir(&account.label).unwrap();
            expected_rows.push(Row {
                label: account.label.clone(),
                class: account.class,
                dir: Some(directory.to_string_lossy().into_owned()),
                email: Some(account.email.clone()),
                vendor: account.vendor,
                ..Row::default()
            });
            expected_directories.insert(directory);
        }
    }
    let retained = || {
        let now = Snapshot::read(bed._dir.path(), &store);
        now.files.keys().eq(before.files.keys())
            && now.directories == expected_directories
            && before.files.iter().all(|(path, bytes)| {
                path == &roster_path
                    || path == &store.wake_path()
                    || now.files.get(path) == Some(bytes)
            })
            && now
                .files
                .get(&roster_path)
                .is_some_and(|bytes| bytes.starts_with(before.files.get(&roster_path).unwrap()))
            && Roster::load_for(Cli::Codex, &roster_path)
                .is_ok_and(|roster| roster.rows == expected_rows)
            && (report.is_some_and(|report| !report.added.is_empty()) || now == before)
    };
    let preserved = retained();
    let once = Snapshot::read(bed._dir.path(), &store);
    let second = bed.sync(Cli::Codex, None);
    let stable = match (&first, &second) {
        (Ok(CliSync::Synced(first)), Ok(CliSync::Synced(second))) => {
            second.added.is_empty()
                && second.kept == first.kept
                && second.held == first.held
                && second.rows == first.rows
                && second.cli == first.cli
                && second.machine == first.machine
                && second.roster == first.roster
                && second.source == first.source
                && second.fleet_accounts == first.fleet_accounts
                && Snapshot::read(bed._dir.path(), &store) == once
        }
        _ => false,
    };
    if candidates == 2
        && state["label_kind"] == 1
        && state["same_vendor"] == 1
        && !representation.reverse_local
        && !representation.reverse_source
        && !representation.ambiguous_peer
        && !representation.replan
        && representation.layout == 0
    {
        let mut wrong_report = report.unwrap().clone();
        wrong_report.kept = vec!["wrong-label".into()];
        assert!(
            !valid_report(&wrong_report),
            "report observer accepted the wrong label"
        );
        let paths = [
            store.state_file(),
            store.state_witness_file(),
            store.logins_file(),
            store.logins_witness_file(),
            credential_paths[0].clone(),
        ];
        for path in paths {
            let bytes = fs::read(&path).unwrap();
            fs::write(&path, "synthetic corruption").unwrap();
            assert!(
                !retained(),
                "preservation observer missed {}",
                path.display()
            );
            fs::write(path, bytes).unwrap();
        }
        let bytes = fs::read(&roster_path).unwrap();
        let mut corrupted = expected_rows.clone();
        corrupted[0].identity_email = Some("changed@example.test".into());
        fs::write(
            &roster_path,
            accounts::append_rows_for(Cli::Codex, "", &corrupted).unwrap(),
        )
        .unwrap();
        assert!(
            !retained(),
            "preservation observer missed a valid changed roster"
        );
        fs::write(&roster_path, &bytes).unwrap();
        assert!(retained());
        if state["companion"] == 1 {
            let mut changed = expected_rows.clone();
            changed.last_mut().unwrap().browser = Some("changed-browser".into());
            let text = accounts::append_rows_for(Cli::Codex, "", &changed).unwrap();
            assert!(
                text.as_bytes()
                    .starts_with(before.files.get(&roster_path).unwrap())
            );
            fs::write(&roster_path, text).unwrap();
            assert!(
                !retained(),
                "preservation observer missed a changed appended row"
            );
            fs::write(&roster_path, &bytes).unwrap();
            assert!(retained());
        }
    }
    [
        ("accepted", i64::from(report.is_some())),
        ("added", added),
        ("kept", kept),
        ("extra", extra),
        ("reports", i64::from(report.is_some_and(valid_report))),
        ("preserved", i64::from(preserved)),
        ("repeat", i64::from(stable)),
    ]
}

fn sync_projection(state: &ModelState) -> [(&'static str, i64); 7] {
    let mut first = None;
    let layouts: &[u8] = if state["candidates"] == 1 && state["companion"] == 0 {
        &[0, 1, 2, 3, 4, 5]
    } else if state["candidates"] == 2 && state["companion"] == 0 {
        &[0, 3]
    } else {
        &[0]
    };
    for &layout in layouts {
        for reverse_local in [false, true] {
            let one_row = state["label_kind"] != 3
                && (state["candidates"] == 0 || (state["candidates"] == 1 && layout < 4));
            if reverse_local && one_row {
                continue;
            }
            for reverse_source in [false, true] {
                if reverse_source && state["companion"] == 0 {
                    continue;
                }
                for ambiguous_peer in [false, true] {
                    if ambiguous_peer
                        && !(state["candidates"] == 2
                            && matches!(state["label_kind"], 1 | 2)
                            && state["companion"] == 1)
                    {
                        continue;
                    }
                    for replan in [false, true] {
                        if replan
                            && !(state["candidates"] == 2
                                && state["label_kind"] == 1
                                && state["companion"] == 1)
                        {
                            continue;
                        }
                        let result = represented_sync(
                            state,
                            Representation {
                                layout,
                                reverse_local,
                                reverse_source,
                                ambiguous_peer,
                                replan,
                            },
                        );
                        if let Some(first) = first {
                            assert_eq!(
                                result, first,
                                "representation changed the decision: {state:?}"
                            );
                        } else {
                            first = Some(result);
                        }
                    }
                }
            }
        }
    }
    first.unwrap()
}

pub(crate) fn alias_transition(state: &ModelState, action: &str) -> ModelState {
    let mut next = state.clone();
    match action {
        "AddCandidate" => {
            next.insert("candidates", state["candidates"] + 1);
        }
        "NameFirst" => {
            next.insert("label_kind", 1);
        }
        "NameSecond" => {
            next.insert("label_kind", 2);
        }
        "NameOther" => {
            next.insert("label_kind", 3);
        }
        "ChangeMethod" => {
            next.insert("same_vendor", 0);
        }
        "AddCompanion" => {
            next.insert("companion", 1);
        }
        "Sync" => {
            next.insert("phase", 1);
            for (field, value) in sync_projection(state) {
                next.insert(field, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn alias_admission_conforms_and_preserves_existing_logins() {
    account_alias_admission_environment();
    let model = aterm_models_harness::account_alias_admission_model();
    assert_eq!(model.mutant_dials(), (1..=9).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 72);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = alias_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = model.successors("Sync", &model.init_state()).pop().unwrap();
    for (field, value, law) in [
        ("accepted", 0, "Admission"),
        ("added", 0, "FreshAddition"),
        ("kept", 1, "ConflictReport"),
        ("extra", 1, "CompanionAddition"),
        ("reports", 0, "ExactReports"),
        ("preserved", 0, "ExistingPreserved"),
        ("repeat", 0, "RepeatStable"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, value);
        assert!(
            !model.check_invariant(law, &bad),
            "{law} observer missed its negative control"
        );
    }
}
