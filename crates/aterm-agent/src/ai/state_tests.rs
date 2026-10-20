// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;

use super::*;
use crate::ai::connector::{AGY, CLAUDE, CODEX, MUSE};
use crate::ai::facts::{ClaudeAccounts, Managed};
use crate::ai::store::{How, Record};

const NOW: i64 = 1_800_000_000;

fn facts(c: &'static Connector) -> Facts {
    Facts {
        connector: c,
        kept: None,
        managed: Some(Managed {
            path: PathBuf::from("/pkg/agents/x"),
            version: Some("1.0".into()),
        }),
        foreign: None,
        home: Ok(PathBuf::from("/home/me/.x")),
        home_exists: true,
        login_file: None,
        hidden_store: false,
        profile_signed_in: false,
        plan: None,
        accounts: None,
        limited_until: None,
        record: Record::default(),
        env_credentials: Vec::new(),
    }
}

fn accounts(state: Option<&'static str>) -> ClaudeAccounts {
    ClaudeAccounts {
        label: "main".into(),
        listed: false,
        refused: None,
        renews: state.is_some(),
        login: None,
        made: false,
        state,
        renew_by: None,
        limited_until: None,
        served_at: None,
        renewed_at: None,
    }
}

#[test]
fn not_wanted_excluded_removed_and_unavailable_are_off_never_a_fault() {
    assert_eq!(assess(&facts(&CODEX), false, NOW).state, State::Off);
    for kept in [Kept::Unavailable, Kept::Excluded, Kept::Removed] {
        let mut f = facts(&CODEX);
        f.kept = Some(kept);
        let row = assess(&f, true, NOW);
        assert_eq!((row.state, row.mark), (State::Off, Mark::Note), "{kept:?}");
    }
}

#[test]
fn a_missing_cli_needs_installation_even_before_its_vendor_lab_has_run() {
    let mut f = facts(&CODEX);
    f.managed = None;
    f.foreign = Some(PathBuf::from("/opt/homebrew/bin/codex"));
    let row = assess(&f, true, NOW);
    assert_eq!((row.state, row.mark), (State::NotInstalled, Mark::Fail));
    assert_eq!(row.fix.as_deref(), Some("aterm pkg install codex"));
    assert!(row.fact.contains("/opt/homebrew/bin/codex"), "{}", row.fact);
    for c in [&AGY, &MUSE] {
        let mut g = facts(c);
        g.managed = None;
        let row = assess(&g, true, NOW);
        assert_eq!((row.state, row.mark), (State::NotInstalled, Mark::Fail));
        assert!(row.state.needs_person(row.wanted));
        assert_eq!(row.fix, Some(format!("aterm pkg install {}", c.id)));
        assert!(!row.connector.measured, "the evidence metadata stays");
    }
}

#[test]
fn codex_reads_its_login_file_and_its_write_time() {
    let mut f = facts(&CODEX);
    f.home_exists = false;
    let row = assess(&f, true, NOW);
    assert_eq!((row.state, row.mark), (State::SignedOut, Mark::Fail));
    assert!(row.fact.starts_with("never signed in"), "{}", row.fact);
    f.home_exists = true;
    assert!(assess(&f, true, NOW).fact.starts_with("no login is stored"));
    f.login_file = Some((PathBuf::from("/home/me/.codex/auth.json"), Some(NOW - 3600)));
    let row = assess(&f, true, NOW);
    assert_eq!(
        (row.state, row.used),
        (State::Ok, Some((NOW - 3600, "refreshed")))
    );
    f.login_file = Some((
        PathBuf::from("/home/me/.codex/auth.json"),
        Some(NOW - 3 * 86_400),
    ));
    assert_eq!(assess(&f, true, NOW).state, State::Idle);
}

#[test]
fn extreme_stored_timestamps_have_bounded_ages() {
    let mut f = facts(&CODEX);
    f.hidden_store = true;
    f.record.used_at = Some(i64::MIN);
    f.record.used_how = Some(How::Review);
    assert_eq!(assess(&f, true, NOW).state, State::Idle);
    f.record.ended_at = Some(i64::MIN);
    assert_eq!(assess(&f, true, NOW).state, State::SignedOut);
    f.record.ended_at = None;
    f.record.used_at = Some(i64::MAX);
    assert_eq!(assess(&f, true, -1).state, State::Ok);
}

#[test]
fn a_store_aterm_cannot_see_is_unproven_never_signed_out() {
    let mut f = facts(&CODEX);
    f.hidden_store = true;
    let row = assess(&f, true, NOW);
    assert_eq!((row.state, row.mark), (State::Unproven, Mark::Note));
    // A review that worked proves it.
    f.record
        .apply(How::Review, crate::ai::store::Outcome::Worked, NOW - 60);
    assert_eq!(assess(&f, true, NOW).state, State::Ok);
}

#[test]
fn an_end_counts_until_something_newer_shows_the_login_working() {
    let mut f = facts(&CODEX);
    f.login_file = Some((PathBuf::from("/a"), Some(NOW - 7200)));
    f.record.apply(
        How::Review,
        crate::ai::store::Outcome::Ended(crate::ai::store::Ended::ReviewAuth),
        NOW - 3600,
    );
    let row = assess(&f, true, NOW);
    assert_eq!(row.state, State::SignedOut);
    assert!(
        row.fact.contains("a review could not authenticate"),
        "{}",
        row.fact
    );
    // Codex wrote its login file after the end (someone signed in): it works again.
    f.login_file = Some((PathBuf::from("/a"), Some(NOW - 60)));
    assert_eq!(assess(&f, true, NOW).state, State::Ok);
}

#[test]
fn claude_reads_its_profile_and_accounts_v2s_bound_login() {
    let mut f = facts(&CLAUDE);
    f.accounts = Some(accounts(None));
    assert_eq!(assess(&f, true, NOW).state, State::SignedOut);
    f.profile_signed_in = true;
    f.plan = Some("Max".into());
    let row = assess(&f, true, NOW);
    assert_eq!(row.state, State::Idle);
    assert!(row.fact.starts_with("signed in (Max)"), "{}", row.fact);
    // A renewal bound the own row to a login directory: that login is the one read.
    let mut a = accounts(Some("idle"));
    a.login = Some("/state/accounts/logins/main+1".into());
    a.made = false;
    f.accounts = Some(a.clone());
    assert_eq!(
        assess(&f, true, NOW).state,
        State::SignedOut,
        "the profile does not stand for a bound login"
    );
    a.made = true;
    a.state = Some("ok");
    f.accounts = Some(a.clone());
    assert_eq!(assess(&f, true, NOW).state, State::Ok);
    a.state = Some("expired");
    f.accounts = Some(a.clone());
    assert_eq!(assess(&f, true, NOW).state, State::SignedOut);
    a.state = Some("due");
    a.renew_by = Some(NOW + 86_400);
    f.accounts = Some(a);
    let row = assess(&f, true, NOW);
    assert_eq!(
        (row.state, row.mark),
        (State::Due(NOW + 86_400), Mark::Warn)
    );
    assert_eq!(row.fix.as_deref(), Some("aterm ai login claude"));
}

#[test]
fn a_limit_ahead_is_a_note_and_one_past_is_nothing() {
    let mut f = facts(&CODEX);
    f.login_file = Some((PathBuf::from("/a"), Some(NOW - 60)));
    f.limited_until = Some(NOW + 600);
    assert_eq!(assess(&f, true, NOW).state, State::Limited(NOW + 600));
    f.limited_until = Some(NOW - 1);
    assert_eq!(assess(&f, true, NOW).state, State::Ok);
}

#[test]
fn a_credential_in_the_environment_is_named_in_the_fact() {
    let mut f = facts(&CODEX);
    f.login_file = Some((PathBuf::from("/a"), Some(NOW - 60)));
    f.env_credentials = vec!["OPENAI_API_KEY"];
    assert!(
        assess(&f, true, NOW)
            .fact
            .contains("OPENAI_API_KEY set in this environment")
    );
}

#[test]
fn unknown_keyring_evidence_needs_no_action_but_a_definite_login_end_does() {
    let mut f = facts(&AGY);
    f.home_exists = false;
    let row = assess(&f, true, NOW);
    assert_eq!((row.state, row.mark), (State::Unproven, Mark::Note));
    assert!(row.fact.contains("not measured"), "{}", row.fact);
    assert!(!row.state.needs_person(row.wanted));
    f.record.ended_at = Some(NOW);
    f.record.ended_why = Some(crate::ai::store::Ended::ReviewAuth);
    let row = assess(&f, true, NOW);
    assert_eq!((row.state, row.mark), (State::SignedOut, Mark::Fail));
    assert!(row.state.needs_person(row.wanted));
    assert!(!row.state.needs_person(false));
    assert_eq!(row.fix.as_deref(), Some("aterm ai login agy"));
}

#[test]
fn the_health_line_names_what_needs_a_person() {
    let mut a = facts(&CODEX);
    a.home_exists = false;
    let mut b = facts(&AGY);
    b.managed = None;
    let rows = [assess(&a, true, NOW), assess(&b, true, NOW)];
    let h = health(&rows);
    assert_eq!(h.word, "FAIL");
    assert_eq!(
        h.line("aterm ai doctor"),
        "FAIL — 2 CLIs need you (codex, agy); next: aterm ai doctor"
    );
    let ok = [assess(&b, true, NOW)];
    assert_eq!(
        health(&ok).line("x"),
        "FAIL — 1 CLI needs you (agy); next: x"
    );
    assert_eq!(health(&[]).line("x"), "OK");
}

#[test]
fn a_renewal_after_a_recorded_end_clears_it() {
    let mut f = facts(&CLAUDE);
    f.profile_signed_in = true;
    f.record.apply(
        How::Review,
        crate::ai::store::Outcome::Ended(crate::ai::store::Ended::ReviewAuth),
        NOW - 600,
    );
    let mut a = accounts(Some("idle"));
    a.login = Some("/state/accounts/logins/main+1".into());
    a.made = true;
    f.accounts = Some(a.clone());
    assert_eq!(
        assess(&f, true, NOW).state,
        State::SignedOut,
        "the control: the end stands"
    );
    a.renewed_at = Some(NOW - 60);
    f.accounts = Some(a);
    let row = assess(&f, true, NOW);
    assert_eq!(row.state, State::Ok);
    assert_eq!(row.used, Some((NOW - 60, "renewed")));
}

fn persisted_facts(record: &str) -> Facts {
    let dir = aterm_tempfile::tempdir().expect("private store root");
    let store = crate::ai::store::Store::at(dir.path());
    store.ensure().expect("private store");
    let text = format!("{{\"schema\":1,\"clis\":{{\"codex\":{record}}}}}");
    std::fs::write(store.dir().join(crate::ai::store::RECORD_FILE), text).expect("record");
    let mut f = facts(&CODEX);
    f.record = store.load().expect("actual record admission").of("codex");
    f
}

#[test]
fn a_persisted_extreme_end_has_a_bounded_age() {
    let ordinary = persisted_facts(&format!(
        "{{\"ended_at\":{},\"ended_why\":\"review-auth\"}}",
        NOW - 3600
    ));
    let row = assess(&ordinary, true, NOW);
    assert_eq!(row.state, State::SignedOut);
    assert!(row.fact.ends_with("1 h ago"), "{}", row.fact);
    let future = persisted_facts(&format!("{{\"ended_at\":{}}}", i64::MAX));
    assert!(assess(&future, true, NOW).fact.ends_with("1 min ago"));

    let ancient = persisted_facts(&format!("{{\"ended_at\":{}}}", i64::MIN));
    let row = assess(&ancient, true, NOW);
    assert_eq!(row.state, State::SignedOut);
    assert!(row.fact.ends_with(" d ago"), "{}", row.fact);
    assert!(
        assess(&future, true, i64::MIN).fact.ends_with("1 min ago"),
        "a clock before the future record still has zero displayed age"
    );
}

#[test]
fn a_persisted_extreme_use_has_a_bounded_display_age() {
    let mut ordinary = persisted_facts(&format!(
        "{{\"used_at\":{},\"used_how\":\"review\"}}",
        NOW - 3600
    ));
    ordinary.hidden_store = true;
    let row = assess(&ordinary, true, NOW);
    assert_eq!(row.state, State::Ok);
    assert!(row.fact.contains("review 1 h ago"), "{}", row.fact);
    let mut future = persisted_facts(&format!(
        "{{\"used_at\":{},\"used_how\":\"review\"}}",
        i64::MAX
    ));
    future.hidden_store = true;
    let row = assess(&future, true, NOW);
    assert_eq!(row.state, State::Ok);
    assert!(row.fact.contains("review 1 min ago"), "{}", row.fact);

    let mut ancient = persisted_facts(&format!(
        "{{\"used_at\":{},\"used_how\":\"review\"}}",
        i64::MIN
    ));
    ancient.hidden_store = true;
    let row = assess(&ancient, true, NOW);
    assert_eq!(row.state, State::Idle, "an ancient use cannot become fresh");
    assert!(row.fact.contains(" d ago"), "{}", row.fact);
    let row = assess(&future, true, i64::MIN);
    assert_eq!(row.state, State::Ok);
    assert!(row.fact.contains("review 1 min ago"), "{}", row.fact);
}

#[test]
fn a_persisted_extreme_use_without_how_does_not_become_fresh() {
    let mut ordinary = persisted_facts(&format!("{{\"used_at\":{}}}", NOW - OK_FRESH_S - 1));
    ordinary.hidden_store = true;
    assert_eq!(assess(&ordinary, true, NOW).state, State::Idle);
    let mut future = persisted_facts(&format!("{{\"used_at\":{}}}", i64::MAX));
    future.hidden_store = true;
    assert_eq!(assess(&future, true, NOW).state, State::Ok);

    let mut ancient = persisted_facts(&format!("{{\"used_at\":{}}}", i64::MIN));
    ancient.hidden_store = true;
    let row = assess(&ancient, true, NOW);
    assert_eq!(
        row.used, None,
        "no display-age subtraction precedes freshness"
    );
    assert_eq!(row.state, State::Idle, "an ancient use cannot become fresh");
    assert_eq!(assess(&future, true, i64::MIN).state, State::Ok);
}
