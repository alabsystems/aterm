// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

type Image = BTreeMap<PathBuf, (fs::FileType, Option<Vec<u8>>, Option<PathBuf>)>;

// Do not follow links or open special files while checking retained source bytes.
fn image(root: &Path) -> Image {
    let mut image = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let kind = fs::symlink_metadata(&path).unwrap().file_type();
        let bytes = kind.is_file().then(|| fs::read(&path).unwrap());
        let link = kind.is_symlink().then(|| fs::read_link(&path).unwrap());
        if kind.is_dir() {
            pending.extend(
                fs::read_dir(&path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        }
        image.insert(path, (kind, bytes, link));
    }
    image
}

/// The ways a vault entry holds its place and cannot be read as text: a byte no UTF-8 text
/// holds, and an overlong encoding.
fn faults() -> Vec<&'static str> {
    vec!["not-utf8", "overlong"]
}

/// Publish entry `name` unreadable as `kind` ([`faults`]).
fn plant_fault(bed: &Bed, name: &str, kind: &str) {
    let bytes = match kind {
        "not-utf8" => vec![0xff, 0xfe, b'\n'],
        "overlong" => vec![0xc0, 0x80, b'\n'],
        _ => panic!("unknown source fault: {kind}"),
    };
    crate::vault_entry::seed(&bed.home, &[(name, &bytes)]);
}

fn project(state: &State, action: &str, cli: Cli, variant: &str, cold: bool) -> State {
    let bed = Bed::new("source-presence");
    let store = if cold {
        if cli != Cli::Claude {
            bed.keep_roster(cli);
        }
        accounts::Store::at_for(&bed.state, cli, &bed.home)
    } else {
        bed.seed_records(cli)
    };
    let name = if cli == Cli::Claude {
        FLEET_ROSTER_FILE.to_owned()
    } else {
        cli_fleet_file(cli)
    };
    let primary = name.as_str();
    let fallback = ACCOUNTS_CSV_FILE;
    let region = if cli == Cli::Kimi {
        "region='global'\n"
    } else {
        ""
    };
    let toml = format!(
        "[[account]]\nlabel='primary'\nemail='primary@example.test'\nmachines=['m16']\n{region}"
    );
    let member = csv_columns(cli)[0];
    let value = if cli == Cli::Kimi { "global" } else { "1" };
    let csv = format!("email,computer,{member}\nfallback@example.test,m16,{value}\n");
    let case = match action {
        "Primary" => {
            bed.secret(&name, &toml);
            if variant == "broken-fallback" {
                plant_fault(&bed, fallback, "not-utf8");
            } else {
                bed.secret(ACCOUNTS_CSV_FILE, &csv);
            }
            1
        }
        "Fallback" => {
            bed.secret(fallback, &csv);
            2
        }
        "PrimaryUnavailable" => {
            bed.secret(ACCOUNTS_CSV_FILE, &csv);
            plant_fault(&bed, primary, variant);
            3
        }
        "FallbackUnavailable" => {
            plant_fault(&bed, fallback, variant);
            for (file, email) in [
                (".claude.json", "local-m16@example.test"),
                (".claude.json.backup", "local-m16-2@example.test"),
            ] {
                fs::write(
                    bed.home.join(file),
                    format!("{{\"oauthAccount\":{{\"emailAddress\":\"{email}\"}}}}\n"),
                )
                .unwrap();
            }
            4
        }
        _ => panic!("unknown source action"),
    };
    let present =
        crate::vault_entry::read_text(&bed.home, primary, SOURCE_TEXT_MAX_BYTES).occupies();
    if cli == Cli::Claude && !cold {
        fs::write(
            bed.config.join(FLEET_ROSTER_FILE),
            format!("opt_in='named'\n{toml}"),
        )
        .unwrap();
        assert!(init_sync_needed(
            &bed.home,
            &bed.state,
            &bed.roster(cli),
            NOW,
            false
        ));
    }
    assert!(has_init_accounts_source(
        &bed.home,
        &bed.state,
        &bed.roster(cli),
        NOW
    ));
    if cli == Cli::Claude {
        let origin = fleet_origin(
            &accounts::Store::at(&bed.state),
            Some(&bed.home),
            Some(&bed.aterm_toml()),
            &bed.roster(cli),
        );
        // A selected unreadable entry can refuse before an origin names it. It must still
        // refuse below without consuming a fallback or changing any retained record.
        if !matches!(&origin, FleetOrigin::SourcePending(_)) || case <= 2 {
            assert_eq!(
                origin.source(),
                Some(crate::vault_entry::source_name(if case == 1 || case == 3 {
                    primary
                } else {
                    fallback
                })),
                "{cli:?} {action} {variant} cold={cold}",
            );
        }
    }
    let paths = [
        bed.roster(cli),
        bed.config.join(FLEET_ROSTER_FILE),
        store.state_file(),
        store.state_witness_file(),
        store.logins_file(),
        store.logins_witness_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let records = || {
        paths
            .iter()
            .map(|path| fs::read(path).ok())
            .collect::<Vec<_>>()
    };
    let before = (records(), image(&bed.home));
    let result = if cli == Cli::Claude {
        sync_accounts_for_init(&bed.home, &bed.state, &bed.roster(cli), NOW).map(|_| ())
    } else {
        bed.sync(cli, None).map(|_| ())
    };
    if case <= 2 {
        assert!(result.is_ok(), "{cli:?} {action} {variant}: {result:?}");
    }
    let selected = if result.is_err() {
        0
    } else {
        let rows = bed.rows(cli);
        match (
            rows.iter()
                .any(|row| row.email.as_deref() == Some("primary@example.test")),
            rows.iter()
                .any(|row| row.email.as_deref() == Some("fallback@example.test")),
        ) {
            (true, false) => 1,
            (false, true) => 2,
            _ => 3,
        }
    };
    let kept = before == (records(), image(&bed.home));
    let mut next = state.clone();
    next.insert("case", case);
    next.insert("selected", selected);
    next.insert("present", i64::from(present));
    next.insert("kept", i64::from(kept));
    next
}

pub(crate) fn source_transition(state: &State, action: &str) -> State {
    let variants = match action {
        "Primary" => vec!["plain", "broken-fallback"],
        "Fallback" => vec!["plain"],
        "PrimaryUnavailable" | "FallbackUnavailable" => faults(),
        _ => panic!("unknown source action"),
    };
    let mut agreed = None;
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        for variant in &variants {
            // Successful selection starts a fresh pool. An unrelated retained
            // default binding correctly refuses replacing that login.
            let starts: &[bool] = if matches!(action, "Primary" | "Fallback") {
                &[true]
            } else {
                &[false, true]
            };
            for &cold in starts {
                let next = project(state, action, cli, variant, cold);
                if let Some(agreed) = &agreed {
                    assert_eq!(agreed, &next, "{cli:?} {action} {variant} cold={cold}");
                } else {
                    agreed = Some(next);
                }
            }
        }
    }
    agreed.unwrap()
}

#[test]
fn source_precedence_and_refusal_conform_with_retained_records() {
    let model = aterm_models_harness::account_source_presence_model();
    let mut witnessed = BTreeMap::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = source_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} {next:?}",
                    action.name
                );
                witnessed.insert(action.name, next);
            }
        }
    }
    for (action, field, value, invariant) in [
        ("Primary", "selected", 2, "PrimaryWins"),
        ("Fallback", "selected", 0, "AbsentAllowsFallback"),
        ("PrimaryUnavailable", "selected", 2, "UnusableRefuses"),
        ("PrimaryUnavailable", "present", 0, "Presence"),
        ("FallbackUnavailable", "kept", 0, "RefusalRetains"),
    ] {
        let mut wrong = witnessed[action].clone();
        wrong.insert(field, value);
        assert!(!model.check_invariant(invariant, &wrong));
        assert!(
            !model
                .successors(action, &model.init_state())
                .contains(&wrong)
        );
    }
}
