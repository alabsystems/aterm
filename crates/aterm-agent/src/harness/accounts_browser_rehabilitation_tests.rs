// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;
const LABEL: &str = "target";
const EMAIL: &str = "target@example.test";

#[derive(Clone, Copy)]
enum Fault {
    None,
    PermanentExclusion,
    TolerantLedger,
    UnscopedWindow,
}

fn window(at: i64, end: &str, session: &str) -> accounts::Event {
    accounts::Event::new(at, accounts::Kind::SignIn, LABEL)
        .because(&format!("claude.ai window {end} {session}"))
}

fn wrong(at: i64) -> accounts::Event {
    accounts::Event::new(at, accounts::Kind::WrongAccount, LABEL)
}

fn image(dir: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            entries.push((path.clone(), None));
            entries.extend(image(&path));
        } else {
            entries.push((path.clone(), Some(fs::read(&path).unwrap())));
        }
    }
    entries.sort();
    entries
}

fn read(case: i64, fault: Fault) -> [(&'static str, i64); 5] {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(root.path()).bounded_by(&[]);
    let path = root.path().join("accounts.toml");
    let mut raw = accounts::Row {
        label: LABEL.into(),
        email: Some(EMAIL.into()),
        identity_email: (case >= 34).then(|| "owner-a@example.test".into()),
        ..Default::default()
    };
    fs::write(
        &path,
        accounts::append_rows_for(Cli::Claude, "", &[raw.clone()]).unwrap(),
    )
    .unwrap();
    let no_wrong = matches!(case, 0 | 22 | 25 | 26);
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            let mut events = accounts::note_auth(state, LABEL, accounts::Auth::SignedOut, NOW - 10);
            logins
                .labels
                .insert(LABEL.into(), accounts::Binding::default());
            if !no_wrong {
                events.extend(accounts::note_wrong_account(logins, LABEL, NOW));
            }
            Ok(events)
        })
        .unwrap();
    let profile = super::super::headless_oauth::resolve_browser_profile_dir(
        &store.dir,
        Some(LABEL),
        Some(EMAIL),
    );
    if !matches!(case, 14 | 26) {
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("vendor-session"), b"opaque existing profile").unwrap();
        let marker = profile.join(super::super::headless_oauth::PROFILE_UNPROVEN_MARK);
        match case {
            15 | 25 => fs::write(marker, b"authorization not proved").unwrap(),
            16 => fs::create_dir(marker).unwrap(),
            _ => {}
        }
    }
    let successful = || window(NOW + 1, "done", "session-kept");
    let events = match case {
        0 | 2 | 17 | 22 | 25 | 26 => Vec::new(),
        3 => vec![window(NOW, "done", "session-kept")],
        4 => vec![window(NOW - 1, "done", "session-kept")],
        5 => vec![successful(), wrong(NOW + 2)],
        6 => vec![successful(), wrong(NOW - 1)],
        7 => vec![
            successful(),
            wrong(NOW + 2),
            window(NOW + 3, "done", "session-kept"),
        ],
        8 => vec![successful(), window(NOW + 2, "done", "session-dropped")],
        9 => vec![successful(), window(NOW - 1, "done", "session-dropped")],
        10 => vec![window(NOW + 1, "done", "session-unknown")],
        11 => vec![window(NOW + 1, "quit", "session-unknown")],
        12 => vec![window(NOW + 1, "interrupted", "session-unknown")],
        13 => vec![window(NOW + 1, "done", "session-dropped")],
        18 => vec![
            successful(),
            accounts::Event::new(NOW - 1, accounts::Kind::SignIn, LABEL)
                .because("profile needs-you"),
        ],
        19 => vec![successful(), window(NOW - 1, "done", "session-kept")],
        20 => vec![
            accounts::Event::new(NOW + 1, accounts::Kind::SignIn, "foreign")
                .because("claude.ai window done session-kept"),
        ],
        21 => vec![
            successful(),
            accounts::Event::new(NOW + 2, accounts::Kind::WrongAccount, "foreign"),
        ],
        24 => vec![successful(), window(NOW + 2, "done", "tidy-failed")],
        27 => vec![window(NOW + 1, "done", "session-unchecked")],
        28 => vec![
            successful(),
            window(NOW + 2, "done", "session-kept untidied"),
        ],
        29 => vec![
            successful(),
            wrong(NOW - 1),
            window(NOW, "done", "session-kept"),
        ],
        30 => vec![
            successful(),
            wrong(NOW - 1),
            window(NOW + 2, "done", "session-kept"),
        ],
        34..=36 => vec![successful()],
        _ => vec![successful()],
    };
    if case >= 34 {
        // Legacy A history has no writer-issued custody. B retains the same label,
        // delivery address and profile while its expected provider identity changes.
        store.append(&events).unwrap();
        let legacy = store.tail_validated(10).unwrap().pop().unwrap();
        assert_eq!(legacy.workflow, None);
        raw.identity_email = Some("owner-b@example.test".into());
        fs::write(
            &path,
            accounts::append_rows_for(Cli::Claude, "", &[raw.clone()]).unwrap(),
        )
        .unwrap();
        store
            .transact_pool_logins_within(&[], &path, NOW + 2, &mut None, |_, _, _| Ok(Vec::new()))
            .unwrap();
        if case >= 35 {
            store
                .transact_pool_logins_within(&[], &path, NOW + 3, &mut None, |roster, _, logins| {
                    assert_eq!(roster.row(LABEL), Some(&raw));
                    accounts::admit_workflow(logins, LABEL)?;
                    Ok(vec![window(NOW + 3, "done", "session-kept")])
                })
                .unwrap();
        }
        if case == 36 {
            store.append(&[wrong(NOW + 4)]).unwrap();
        }
        let retained = store.tail_validated(10).unwrap();
        assert!(
            retained.contains(&legacy),
            "A's raw history is retained, never relabelled"
        );
        if case >= 35 {
            assert!(
                retained
                    .iter()
                    .any(|event| event.t == NOW + 3 && event.workflow.is_some()),
                "B's positive is a writer-scoped event"
            );
        }
    } else {
        store
            .transact_pool_logins_within(&[], &path, NOW + 10, &mut None, |roster, _, logins| {
                assert_eq!(roster.row(LABEL), Some(&raw));
                accounts::admit_workflow(logins, LABEL)?;
                Ok(events)
            })
            .unwrap();
    }
    let logins = store.load_logins(NOW + 10).unwrap().0;
    match case {
        0 | 2 => fs::remove_file(store.ledger_path()).unwrap(),
        17 | 22 => {
            fs::remove_file(store.ledger_path()).unwrap();
            fs::create_dir(store.ledger_path()).unwrap();
        }
        23 => fs::write(store.dir.join("sync-commit.json"), b"{").unwrap(),
        31..=33 => {
            use std::io::Write as _;
            let suffix = if case == 31 {
                "{\"t\":".to_owned()
            } else {
                let mut event = aterm_json::json!({
                    "t": NOW + 2, "kind": "sign-in", "account": LABEL,
                    "to": null, "sid": null, "why": 7,
                });
                if case == 33 {
                    event.as_object_mut().unwrap().remove("why");
                }
                format!("{event}\n")
            };
            fs::OpenOptions::new()
                .append(true)
                .open(store.ledger_path())
                .unwrap()
                .write_all(suffix.as_bytes())
                .unwrap();
        }
        _ => {}
    }
    let before = image(root.path());
    let captured_logins = logins.clone();
    let ready = match fault {
        Fault::None => claude_browser_profile_ready(&store, &logins, &raw),
        Fault::UnscopedWindow => {
            super::super::headless_oauth::has_browser_profile(&store.dir, LABEL, Some(EMAIL))
                && browser_window_signed_in_token(&store, LABEL, NOW)
                    .is_some_and(|(at, _)| at > NOW)
        }
        Fault::PermanentExclusion => {
            // The previous routing predicate excluded every retained wrong-account binding.
            logins
                .get(LABEL)
                .and_then(|binding| binding.wrong_at)
                .is_none()
                && super::super::headless_oauth::has_browser_profile(&store.dir, LABEL, Some(EMAIL))
        }
        Fault::TolerantLedger => {
            // The telemetry reader discards invalid rows or defaults a malformed reason.
            let wrong_at = logins
                .get(LABEL)
                .and_then(|binding| binding.wrong_at)
                .unwrap();
            let receipt = store
                .tail(accounts::LEDGER_KEEP_ROWS)
                .unwrap()
                .into_iter()
                .rev()
                .find(|event| {
                    event.account == LABEL
                        && event.kind == accounts::Kind::SignIn
                        && is_signed_in_browser_window_why(&event.why)
                });
            super::super::headless_oauth::has_browser_profile(&store.dir, LABEL, Some(EMAIL))
                && receipt.is_some_and(|event| event.t > wrong_at)
        }
    };
    let after_state =
        accounts::State::from_json(&fs::read_to_string(store.state_file()).unwrap(), NOW + 10)
            .unwrap();
    let after_logins =
        accounts::Logins::from_json(&fs::read_to_string(store.logins_file()).unwrap(), NOW + 10)
            .unwrap();
    [
        ("phase", 1),
        ("ready", i64::from(ready)),
        (
            "authentication",
            i64::from(after_state.account(LABEL).auth == accounts::Auth::Ok),
        ),
        (
            "wrong_retained",
            i64::from(
                logins == captured_logins
                    && after_logins.get(LABEL).and_then(|binding| binding.wrong_at)
                        == (!no_wrong).then_some(NOW),
            ),
        ),
        ("unchanged", i64::from(image(root.path()) == before)),
    ]
}

pub(crate) fn rehabilitation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Case" => {
            next.insert("case", state["case"] + 1);
        }
        "Read" => next.extend(read(state["case"], Fault::None)),
        _ => unreachable!(),
    }
    next
}

fn admits(
    model: &aterm_spec::derive::Model,
    before: &aterm_spec::interp::State,
    after: &aterm_spec::interp::State,
    action: &str,
) -> bool {
    let (accepted, why) = aterm_spec::verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        after,
        Some(action),
        "account browser rehabilitation",
    );
    eprintln!("{} case={} {action}: {why}", model.name, before["case"]);
    accepted
}

#[test]
fn browser_readiness_refines_ordered_rehabilitation_evidence() {
    let model = aterm_models_harness::account_browser_rehabilitation_model();
    let coverage =
        aterm_spec::verify::prove_and_catch_tiered(&model, "account browser rehabilitation")
            .expect("scalar account browser rehabilitation is executable");
    eprintln!("{}: {coverage:?}", model.name);
    if aterm_spec::verify::checkers_required() {
        assert_eq!(coverage, aterm_spec::verify::Covered::InterpreterAndTy);
    }
    let mut reads = 0;
    let mut fixtures = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = rehabilitation_transition(&state, action.name);
                assert!(
                    admits(&model, &state, &next, action.name),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                reads += usize::from(action.name == "Read");
                fixtures += usize::from(action.name == "Case");
            }
        }
    }
    assert_eq!(reads, 37);
    assert_eq!(fixtures, 36);
    let mut before = model.init_state();
    before.insert("case", 1);
    let mut obsolete = before.clone();
    obsolete.extend(read(1, Fault::PermanentExclusion));
    assert!(!admits(&model, &before, &obsolete, "Read"));
    assert!(!model.check_invariant("CurrentBrowserReceiptAdmitted", &obsolete));
    for case in 31..=33 {
        before.insert("case", case);
        let mut tolerant = before.clone();
        tolerant.extend(read(case, Fault::TolerantLedger));
        assert!(!admits(&model, &before, &tolerant, "Read"));
        assert!(!model.check_invariant("UnreadableOrMissingEvidenceRefused", &tolerant));
    }
    before.insert("case", 34);
    let mut unscoped = before.clone();
    unscoped.extend(read(34, Fault::UnscopedWindow));
    assert!(!admits(&model, &before, &unscoped, "Read"));
    assert!(!model.check_invariant("WorkflowScopedReceiptRequired", &unscoped));
    eprintln!(
        "{}: {reads} production reads, {fixtures} fixture transitions, 5 executable negative controls; {coverage:?}",
        model.name
    );
}
