// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn field(line: &str, key: &str) -> String {
    line.split_whitespace()
        .find_map(|part| part.strip_prefix(&format!("{key}=")))
        .map(aterm_control::wire::pct_decode)
        .expect("field")
}

#[test]
fn unauthorized_and_malformed_reads_never_fetch() {
    assert!(
        aterm_types::control_verbs::spec_for("harness", "recovery")
            .unwrap()
            .owner_only
    );
    assert!(
        !aterm_types::control_verbs::spec_for("harness", "policy")
            .unwrap()
            .owner_only
    );
    assert!(cmd("recovery", false, || panic!("private read")).starts_with("ERR denied:"));
    for rest in ["", "recovery now", "recover"] {
        assert_eq!(cmd(rest, true, || panic!("malformed read")), USAGE);
    }
    assert_eq!(cmd("recovery", true, || Err("busy".into())), "ERR busy\n");
}

#[test]
fn absence_is_distinct_from_an_observed_unknown_and_zero_capacity() {
    assert_eq!(
        cmd("recovery", true, || Ok(None)),
        "OK recovery version=1 available=0\n"
    );
    let mut progress = AccountProgress::unknown(0);
    let unknown = reply(Some(&progress));
    assert_eq!(field(&unknown, "available"), "1");
    for key in ["known", "eligible", "observed_at", "next_at"] {
        assert_eq!(field(&unknown, key), "-");
    }
    progress.known = Some(2);
    progress.eligible = Some(0);
    let known = reply(Some(&progress));
    assert_eq!(field(&known, "known"), "2");
    assert_eq!(field(&known, "eligible"), "0");
    assert_eq!(field(&known, "forecasts_paused"), "0");
    assert_eq!(field(&known, "login_diagnostic_count"), "0");
}

#[test]
fn full_details_targets_and_original_times_round_trip_on_one_line() {
    use aterm_agent::harness::{
        accounts::Row,
        accounts_sync::LoginDiagnostic,
        open_url::{Met, MetWord},
    };
    let mut progress = AccountProgress::unknown(101);
    progress.next_at = Some(102);
    progress.phase = ProgressPhase::SigningIn {
        label: "account two".into(),
        route: Some("vault → browser".into()),
    };
    progress.forecasts_paused = true;
    progress.details = vec!["a=b\nsecond line %20".repeat(100), "Last blocker".into()];
    let diagnostic = LoginDiagnostic::from_exit_for_test(
        &Row {
            label: "account two".into(),
            email: Some("private@example.test".into()),
            dir: Some("/synthetic/private".into()),
            ..Row::default()
        },
        99,
        1,
        Some(Met {
            word: MetWord::BrowserControl,
            host: Some("https://must-not-appear.invalid/token".into()),
            why: Some("/synthetic/must-not-appear someone@must-not-appear.invalid".into()),
        }),
    );
    assert!(
        diagnostic.words().ends_with(": <path> <address>"),
        "the opener's reason is kept as its closed form admits it: {}",
        diagnostic.words()
    );
    progress.login_diagnostics.push(diagnostic.clone());
    let line = reply(Some(&progress));
    assert_eq!(line.lines().count(), 1);
    assert_eq!(field(&line, "phase"), "signing-in");
    assert_eq!(field(&line, "target"), "account two");
    assert_eq!(field(&line, "route"), "vault → browser");
    assert_eq!(field(&line, "forecasts_paused"), "1");
    assert_eq!(field(&line, "detail_count"), "2");
    for (index, detail) in progress.details.iter().enumerate() {
        assert_eq!(field(&line, &format!("detail_{index}")), *detail);
    }
    // Timestamps describe the retained observation, not when this read happened.
    assert_eq!(field(&line, "observed_at"), "101");
    assert_eq!(field(&line, "next_at"), "102");
    assert_eq!(field(&line, "login_diagnostic_count"), "1");
    assert_eq!(
        field(&line, "login_diagnostic_0_account"),
        diagnostic.label()
    );
    assert_eq!(field(&line, "login_diagnostic_0_completed_at"), "99");
    assert_eq!(field(&line, "login_diagnostic_0_words"), diagnostic.words());
    for private in [
        "must-not-appear",
        "private@example.test",
        "/synthetic/private",
    ] {
        assert!(!line.contains(private));
    }
}
