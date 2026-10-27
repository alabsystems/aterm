// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    Bed::tree(root)
        .into_iter()
        .filter(|(path, _)| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("accounts.lock" | "signin.lock")
            )
        })
        .collect()
}

fn directories(root: &Path) -> std::collections::BTreeSet<PathBuf> {
    let mut found = std::collections::BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
                found.insert(entry.path());
            }
        }
    }
    found
}

fn realm_case(case: i64) -> [(&'static str, i64); 5] {
    let bed = Bed::new("provider-realm");
    let cli = match case {
        5 | 7 | 9 | 10 => Cli::Codex,
        6 | 8 => Cli::Grok,
        _ => Cli::Kimi,
    };
    let store = accounts::Store::at_for(&bed.state, cli, &bed.home);
    let original_dir = bed.home.join("original-login");
    let vendor = if cli == Cli::Kimi {
        accounts::Vendor {
            region: Some(Region::Global),
            flow: None,
        }
    } else {
        accounts::Vendor {
            region: None,
            flow: Some(Flow::Browser),
        }
    };
    let mut original = Row {
        label: "present".into(),
        class: Class::Dedicated,
        dir: Some(original_dir.to_string_lossy().into_owned()),
        email: Some("one@example.test".into()),
        vendor,
        ..Row::default()
    };
    if matches!(case, 9 | 10) {
        original.email = Some("alias@example.test".into());
        original.identity_email = Some("owner@example.test".into());
    }
    if matches!(case, 14 | 21) {
        original.email = None;
        original.vendor = accounts::Vendor::default();
    }
    if case == 20 {
        original.dir = Some(
            store
                .login_dir("next")
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
    }
    let mut originals = vec![original];
    if matches!(case, 4 | 18) {
        originals.push(Row {
            label: "existing-mainland".into(),
            dir: Some(
                bed.home
                    .join("existing-mainland")
                    .to_string_lossy()
                    .into_owned(),
            ),
            vendor: accounts::Vendor {
                region: Some(Region::MainlandCn),
                flow: None,
            },
            ..originals[0].clone()
        });
    }
    if case == 16 {
        originals.clear();
    }
    fs::write(
        bed.roster(cli),
        accounts::append_rows_for(cli, "", &originals).unwrap(),
    )
    .unwrap();
    let mut state = accounts::State {
        active: (!originals.is_empty()).then(|| "present".into()),
        ..accounts::State::default()
    };
    let mut logins = accounts::Logins {
        generation_clock: if case == 11 { 19 } else { 7 },
        ..accounts::Logins::default()
    };
    let mut credentials = Vec::new();
    for (index, row) in originals.iter().enumerate() {
        let dir = Path::new(row.dir.as_deref().unwrap());
        if case != 20 {
            fs::create_dir_all(dir).unwrap();
            let path = dir.join("retained-credential");
            let bytes = format!("inert original credential {index}").into_bytes();
            fs::write(&path, &bytes).unwrap();
            credentials.push((path, bytes));
        }
        state.accounts.insert(
            row.label.clone(),
            accounts::Account {
                auth: accounts::Auth::Ok,
                ..accounts::Account::default()
            },
        );
        logins.labels.insert(
            row.label.clone(),
            accounts::Binding {
                dir: row.dir.clone(),
                renewal_generation: 7,
                renewed_at: Some(NOW - 60),
                ..accounts::Binding::default()
            },
        );
    }
    if case == 11 {
        logins.labels.insert(
            "next".into(),
            accounts::Binding {
                dir: Some(
                    bed.home
                        .join("retained-next")
                        .to_string_lossy()
                        .into_owned(),
                ),
                renewal_generation: 19,
                ..accounts::Binding::default()
            },
        );
    }
    store.save(&state).unwrap();
    store.save_logins(&logins).unwrap();
    if case == 19 {
        let target = store.login_dir("next").unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, "an existing file blocks the login directory").unwrap();
    }
    let record_paths = [
        store.state_file(),
        store.state_witness_file(),
        store.logins_file(),
        store.logins_witness_file(),
    ];
    let record_bytes: Vec<_> = record_paths.iter().map(|p| fs::read(p).unwrap()).collect();

    let mut wanted = FleetAccount {
        label: if matches!(case, 2 | 3 | 7 | 8 | 9 | 13 | 15 | 21) {
            "present"
        } else if case == 4 {
            "existing-mainland"
        } else {
            "next"
        }
        .into(),
        machines: vec!["m16".into()],
        class: Class::Dedicated,
        email: match case {
            9 | 10 => "OWNER@example.test",
            12 | 14 | 15 => "different@example.test",
            3 => "one@example.test",
            _ => "ONE@example.test",
        }
        .into(),
        browser: None,
        vendor,
    };
    if matches!(case, 0 | 2 | 4 | 11 | 12 | 14 | 16..=21) {
        wanted.vendor.region = Some(Region::MainlandCn);
    }
    if matches!(case, 5..=8) {
        wanted.vendor.flow = Some(Flow::Device);
    }
    let mut fleet = vec![wanted.clone()];
    if matches!(case, 16 | 17) {
        fleet.push(FleetAccount {
            label: "present".into(),
            email: "one@example.test".into(),
            vendor,
            ..wanted.clone()
        });
    }
    bed.secret(
        &cli_fleet_file(cli),
        &FleetRoster {
            source: "fixture".into(),
            accounts: fleet.clone(),
        }
        .to_toml(),
    );
    let before = files(bed._dir.path());
    let target_existed = store.login_dir(&wanted.label).unwrap().exists();
    let result = bed.sync(cli, None);
    assert!(
        result.is_ok() || matches!(case, 11 | 19 | 20),
        "case {case}: {result:?}"
    );
    if let Err(why) = &result {
        let expected = match case {
            11 => "retained local login or account state",
            19 => "existing ancestor is not a directory",
            20 => "name one dir",
            _ => panic!("case {case}: unexpected refusal {why}"),
        };
        assert!(why.contains(expected), "case {case}: {why}");
    }
    let (accepted, added, kept) = match &result {
        Ok(CliSync::Synced(result)) => (1, result.added.len() as i64, result.kept.len() as i64),
        Err(_) => (0, 0, 0),
        other => panic!("unexpected sync outcome: {other:?}"),
    };
    let after = Roster::load_for(cli, &bed.roster(cli)).unwrap();
    let retained = || {
        Roster::load_for(cli, &bed.roster(cli)).is_ok_and(|roster| {
            originals
                .iter()
                .all(|old| roster.row(&old.label) == Some(old))
                && roster.rows.len()
                    == originals.len()
                        + added as usize
                        + usize::from(originals.is_empty() && added > 0)
        }) && credentials
            .iter()
            .all(|(p, bytes)| fs::read(p).is_ok_and(|actual| actual == *bytes))
            && record_paths
                .iter()
                .zip(&record_bytes)
                .all(|(p, bytes)| fs::read(p).is_ok_and(|actual| actual == *bytes))
    };
    let preserved = retained();
    if let Ok(CliSync::Synced(synced)) = &result {
        if kept > 0 {
            assert_eq!(synced.kept, [wanted.label.clone()]);
        }
        if originals.is_empty() && added > 0 {
            assert_eq!(
                after.row(ACCOUNTS_OWN_LABEL),
                Some(&Row {
                    label: ACCOUNTS_OWN_LABEL.into(),
                    ..Row::default()
                })
            );
        }
        for label in &synced.added {
            let account = fleet
                .iter()
                .find(|account| &account.label == label)
                .unwrap();
            let row = after.row(label).unwrap();
            assert_eq!(row.vendor, account.vendor);
            assert_eq!(row.email.as_deref(), Some(account.email.as_str()));
            assert_eq!(
                row.dir.as_deref().map(Path::new),
                store.login_dir(label).as_deref()
            );
            assert!(originals.iter().all(|old| row.dir != old.dir));
            assert!(Path::new(row.dir.as_deref().unwrap()).is_dir());
        }
    }
    if added == 0 {
        assert_eq!(
            files(bed._dir.path()),
            before,
            "case {case}: first sync wrote files"
        );
        assert_eq!(
            store.login_dir(&wanted.label).unwrap().exists(),
            target_existed
        );
    }
    let once = files(bed._dir.path());
    let directories_once = directories(bed._dir.path());
    let repeated = bed.sync(cli, None);
    let repeated_outcome = match (&result, repeated) {
        (Ok(CliSync::Synced(first)), Ok(CliSync::Synced(next))) => {
            next.added.is_empty() && next.kept == first.kept
        }
        (Err(_), Err(_)) => true,
        _ => false,
    };
    let stable = repeated_outcome
        && files(bed._dir.path()) == once
        && directories(bed._dir.path()) == directories_once;
    if case == 0 {
        // Exercise the actual retention projection against corrupted disk bytes,
        // in addition to the model-level output mutations below.
        for path in record_paths
            .iter()
            .chain(credentials.iter().map(|(path, _)| path))
            .chain(std::iter::once(&bed.roster(cli)))
        {
            let bytes = fs::read(path).unwrap();
            fs::write(path, "synthetic corruption").unwrap();
            assert!(
                !retained(),
                "retention missed corruption at {}",
                path.display()
            );
            assert_ne!(
                files(bed._dir.path()),
                once,
                "repeat projection missed corruption"
            );
            fs::write(path, bytes).unwrap();
        }
        assert!(retained());
    }
    if matches!(case, 0 | 18) {
        let path = bed.roster(cli);
        let bytes = fs::read(&path).unwrap();
        let mut changed = after.rows.clone();
        changed[if case == 0 { 0 } else { 1 }].email = Some("changed@example.test".into());
        let text = accounts::append_rows_for(cli, "", &changed).unwrap();
        fs::write(&path, text).unwrap();
        assert!(!retained(), "retention missed a valid modified roster");
        fs::write(path, bytes).unwrap();
        assert!(retained());
    }
    [
        ("accepted", accepted),
        ("added", added),
        ("kept", kept),
        ("preserved", i64::from(preserved)),
        ("repeat", i64::from(stable)),
    ]
}

pub(crate) fn realm_transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Sync" => {
            next.insert("phase", 1);
            for (key, value) in realm_case(state["case"]) {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn provider_realms_and_methods_conform_without_rebinding_existing_accounts() {
    account_provider_realm_environment();
    let model = aterm_models_harness::account_provider_realm_model();
    assert_eq!(model.mutant_dials(), (1..=8).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 44);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = realm_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = realm_transition(&model.init_state(), "Sync");
    for (field, value, law) in [
        ("accepted", 0, "Admission"),
        ("added", 0, "Additions"),
        ("kept", 1, "KeptReports"),
        ("preserved", 0, "ExistingPreserved"),
        ("repeat", 0, "RepeatStable"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
}
