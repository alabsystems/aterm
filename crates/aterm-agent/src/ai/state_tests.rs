// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;

use super::*;
use crate::ai::connector::{AGY, CLAUDE, CODEX, GROK, MUSE};
use crate::ai::facts::{ClaudeAccounts, Managed};
use crate::ai::store::{How, Record};

const NOW: i64 = 1_800_000_000;

fn health_scope_facts(mode: i64) -> Facts {
    use crate::ai::facts::CliAccounts;
    let mut f = facts(if mode == 2 || mode == 6 || mode == 7 {
        &CLAUDE
    } else {
        &CODEX
    });
    f.login_file = Some((PathBuf::from("/login/auth.json"), None));
    f.profile_signed_in = true;
    match mode {
        1 | 4 => {
            f.pool = Some(CliAccounts {
                serving_login: Some(("CODEX_HOME", Some("/isolated".into()))),
                state: Some("ok"),
                ..CliAccounts::default()
            });
            f.hidden_store = mode == 4;
        }
        2 => {
            let mut account = accounts(Some("ok"));
            account.login = Some("/isolated".into());
            account.made = true;
            f.accounts = Some(account);
        }
        3 => f.home_var = Some("/explicit".into()),
        5 => {
            f.pool = Some(CliAccounts {
                store: Some(crate::harness::accounts::Store::at_for(
                    std::path::Path::new("/unused"),
                    crate::harness::accounts::Cli::Codex,
                    std::path::Path::new("/unused"),
                )),
                serving_label: Some("main".into()),
                serving_login: Some(("CODEX_HOME", None)),
                state: Some("ok"),
                ..CliAccounts::default()
            });
        }
        6 | 7 => {
            let mut account = accounts(Some("ok"));
            account.listed = mode == 6;
            account.store = Some(crate::harness::accounts::Store::at(std::path::Path::new(
                "/unused",
            )));
            f.accounts = Some(account);
        }
        _ => {}
    }
    f
}

#[test]
fn account_health_is_scoped_to_the_serving_login() {
    for mode in 0..8 {
        let mut f = health_scope_facts(mode);
        let default = mode == 0 || mode == 4 || mode == 7;

        f.record.used_at = Some(NOW - 10);
        f.record.used_how = Some(How::Review);
        let row = assess(&f, true, NOW);
        assert!(row.state.usable(), "mode {mode}: {row:?}");
        assert_eq!(row.used, default.then_some((NOW - 10, "review")));

        f.record.ended_at = Some(NOW - 5);
        f.record.ended_why = Some(crate::ai::store::Ended::ReviewAuth);
        let row = assess(&f, true, NOW);
        assert_eq!(row.state == State::SignedOut, default, "mode {mode}");

        f.record.ended_at = None;
        f.record.limited_until = Some(NOW + 60);
        let row = assess(&f, true, NOW);
        assert_eq!(row.limited_until, default.then_some(NOW + 60));
        assert_eq!(row.state == State::Limited(NOW + 60), default);

        f.record = Record::default();
        f.limited_until = Some(NOW + 30);
        let row = assess(&f, true, NOW);
        assert_eq!(row.state, State::Limited(NOW + 30));
        assert_eq!(row.limited_until, Some(NOW + 30));
    }
}

pub(crate) fn health_scope_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Scope" => {
            next.insert("scope", state["scope"] + 1);
        }
        "Issue" => {
            next.insert("issue", state["issue"] + 1);
        }
        "Assess" => {
            let mut f = health_scope_facts(state["scope"]);
            match state["issue"] {
                0 => {
                    f.record.used_at = Some(NOW - 10);
                    f.record.used_how = Some(How::Review);
                }
                1 => {
                    f.record.ended_at = Some(NOW - 5);
                    f.record.ended_why = Some(crate::ai::store::Ended::ReviewAuth);
                }
                2 => f.record.limited_until = Some(NOW + 60),
                3 => f.limited_until = Some(NOW + 60),
                _ => unreachable!(),
            }
            let row = assess(&f, true, NOW);
            next.insert("phase", 1);
            next.insert("used", i64::from(row.used.is_some()));
            next.insert("ended", i64::from(row.state == State::SignedOut));
            next.insert("limited", i64::from(matches!(row.state, State::Limited(_))));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn health_assessment_conforms_to_account_scope() {
    let model = aterm_models_harness::account_health_scope_model();
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 64);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = health_scope_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (scope, issue, field, law) in [
        (1, 0, "used", "UsageScoped"),
        (2, 1, "ended", "EndsScoped"),
        (5, 2, "limited", "LimitsScoped"),
        (0, 2, "limited", "DefaultLimitRetained"),
        (6, 3, "limited", "LocalLimitRetained"),
    ] {
        let mut initial = model.init_state();
        initial.insert("scope", scope);
        initial.insert("issue", issue);
        let mut bad = health_scope_transition(&initial, "Assess");
        bad.insert(field, 1 - bad[field]);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn a_plain_isolated_claude_login_keeps_its_own_signed_out_state() {
    let mut f = facts(&CLAUDE);
    f.profile_signed_in = true;
    let mut account = accounts(None);
    account.login = Some("/isolated".into());
    account.made = true;
    account.signed_out_at = Some(NOW - 1);
    f.accounts = Some(account);
    assert_eq!(assess(&f, true, NOW).state, State::SignedOut);
}

#[test]
fn refused_account_state_never_inherits_legacy_health() {
    use crate::ai::facts::CliAccounts;
    for failure in 0..4 {
        let mut f = health_scope_facts(0);
        if failure < 2 {
            f.connector = &CLAUDE;
            let mut account = accounts(None);
            if failure == 0 {
                account.refused = Some("invalid roster".into());
            } else {
                account.unread = Some("damaged login record".into());
            }
            f.accounts = Some(account);
        } else {
            f.pool = Some(CliAccounts {
                refused: (failure == 2).then(|| "invalid roster".into()),
                launch_refused: (failure == 3).then(|| "invalid pin".into()),
                ..CliAccounts::default()
            });
        }
        f.record.ended_at = Some(NOW);
        f.record.ended_why = Some(crate::ai::store::Ended::ReviewAuth);
        f.record.limited_until = Some(NOW + 60);
        assert!(f.default_record().is_none(), "failure {failure}");
        let row = assess(&f, true, NOW);
        assert_ne!(row.state, State::SignedOut, "failure {failure}: {row:?}");
        assert_eq!(row.used, None);
    }
}

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
        home_var: None,
        home_exists: true,
        login_file: None,
        hidden_store: false,
        profile_signed_in: false,
        plan: None,
        accounts: None,
        limited_until: None,
        record: Record::default(),
        record_scope: (c.id == "codex").then_some("file"),
        credential_store_error: None,
        env_credentials: Vec::new(),
        pool: None,
        review_source: None,
    }
}

fn accounts(state: Option<&'static str>) -> ClaudeAccounts {
    ClaudeAccounts {
        pool_picks: false,
        store: None,
        roster: None,
        label: "main".into(),
        listed: false,
        refused: None,
        unread: None,
        launch_refused: None,
        sign_in_refused: None,
        renews: state.is_some(),
        login: None,
        made: false,
        state,
        renew_by: None,
        limited_until: None,
        served_at: None,
        renewed_at: None,
        signed_out_at: None,
        next_sign_in: None,
        tried: None,
        unattended: true,
        off: None,
    }
}

#[test]
fn not_wanted_excluded_removed_and_unavailable_are_off_never_a_fault() {
    assert_eq!(assess(&facts(&CODEX), false, NOW).state, State::Off);
    for kept in [
        Kept::Unavailable,
        Kept::Excluded,
        Kept::Removed,
        Kept::RemovalUnreadable,
    ] {
        let mut f = facts(&CODEX);
        f.kept = Some(kept);
        let row = assess(&f, true, NOW);
        assert_eq!((row.state, row.mark), (State::Off, Mark::Note), "{kept:?}");
    }
}

/// GROK BUILD NOT INSTALLED IS AN INSTALL OWED, AS ANY DEFAULT-SET PROGRAM'S IS: its row says so
/// and names atpkg's install. CONTROL: a person's exclude keeps it an off note with no fix, which
/// needs nobody; installed, it is read as any other.
#[test]
fn grok_not_installed_needs_its_install_unless_kept_off() {
    let mut f = facts(&GROK);
    f.managed = None;
    let row = assess(&f, true, NOW);
    assert_eq!((row.state, row.mark), (State::NotInstalled, Mark::Fail));
    assert_eq!(row.fix.as_deref(), Some("aterm pkg install grok"));
    assert!(row.state.needs_act(row.wanted));
    assert_eq!(
        row.actor,
        Some(Actor::Aterm),
        "an install is aterm's own act"
    );
    f.kept = Some(Kept::Excluded);
    let row = assess(&f, true, NOW);
    assert_eq!((row.state, row.mark), (State::Off, Mark::Note), "CONTROL");
    assert_eq!(row.fix, None);
    assert!(!row.state.needs_act(row.wanted));
    let mut installed = facts(&GROK);
    installed.home_exists = false;
    let row = assess(&installed, true, NOW);
    assert_eq!(
        (row.state, row.fix.as_deref()),
        (State::SignedOut, Some("aterm ai login grok")),
        "installed, it is read as any other"
    );
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
        assert!(row.state.needs_act(row.wanted));
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
    f.hidden_store = true;
    f.home_exists = false;
    let row = assess(&f, true, NOW);
    assert_eq!((row.state, row.mark), (State::Unproven, Mark::Note));
    assert!(!row.state.needs_act(row.wanted));
    f.record.ended_at = Some(NOW);
    f.record.ended_why = Some(crate::ai::store::Ended::ReviewAuth);
    let row = assess(&f, true, NOW);
    assert_eq!((row.state, row.mark), (State::SignedOut, Mark::Fail));
    assert!(row.state.needs_act(row.wanted));
    assert!(!row.state.needs_act(false));
    assert_eq!(row.fix.as_deref(), Some("aterm ai login agy"));
}

/// THE HEALTH LINE NAMES WHAT ATERM FIXES ITSELF AND ONLY A PERSON'S SIGN-IN NEEDS YOU (owner
/// ruling R-13): a not-installed CLI and a Claude row accounts v2 renews unattended (it has an
/// email and a route) are aterm's fixes, which `aterm ai doctor` makes itself; a CLI's own
/// sign-in (Codex with no roster) is a person's, started by its own command and on the one
/// `NEEDS YOU:` line. NEGATIVE: no line ever says "ask a person"; with every fix aterm's there is
/// no `NEEDS YOU:` line; a pool row with an email is aterm's, the same pool with no email a
/// person's, and so is Grok Build's pool row, which a person approves at this Mac (`accounts
/// login`). The words are checked once, by what they are made of.
#[test]
fn the_health_line_names_aterms_fixes_and_only_a_persons_sign_in_needs_you() {
    let mut codex = facts(&CODEX);
    codex.home_exists = false;
    let mut agy = facts(&AGY);
    agy.managed = None;
    let mut claude = facts(&CLAUDE);
    claude.accounts = Some(ClaudeAccounts {
        pool_picks: false,
        state: Some("expired"),
        ..accounts(Some("expired"))
    });
    let rows = [
        assess(&codex, true, NOW),
        assess(&agy, true, NOW),
        assess(&claude, true, NOW),
    ];
    assert_eq!(
        rows.iter().map(|r| r.actor).collect::<Vec<_>>(),
        [Some(Actor::Person), Some(Actor::Aterm), Some(Actor::Aterm)]
    );
    let h = health(&rows);
    assert_eq!(h.word, "FAIL");
    assert_eq!(
        (h.by_aterm.clone(), h.by_person.clone()),
        (vec!["agy", "claude"], vec!["codex"])
    );
    assert_eq!(h.person_steps, ["aterm ai login codex"]);
    let next = h.next().expect("acts");
    let (line, needs) = (h.line(&next), h.needs_you().expect("a person's act"));
    assert!(line.starts_with("FAIL") && line.ends_with(&next), "{line}");
    assert!(needs.starts_with("NEEDS YOU: "), "{needs}");
    for id in ["agy", "claude", "codex"] {
        assert!(next.contains(id), "{next}");
    }
    assert!(needs.contains("`aterm ai login codex`"), "{needs}");
    for words in [next.as_str(), line.as_str(), needs.as_str()] {
        assert!(!words.contains("ask a person"), "{words}");
    }

    let h = health(&[assess(&agy, true, NOW), assess(&claude, true, NOW)]);
    assert_eq!(h.needs_you(), None, "NEGATIVE: aterm makes every fix");
    assert!(h.by_person.is_empty() && h.person_steps.is_empty());

    use crate::ai::connector::KIMI;
    let mut kimi = facts(&KIMI);
    kimi.pool = Some(crate::ai::facts::CliAccounts {
        roster: PathBuf::from("/home/me/.config/aterm/accounts-kimi.toml"),
        next: Some("kimi-one".to_string()),
        ..crate::ai::facts::CliAccounts::default()
    });
    assert_eq!(assess(&kimi, true, NOW).actor, Some(Actor::Aterm));
    kimi.pool.as_mut().expect("pool").next = None;
    assert_eq!(
        assess(&kimi, true, NOW).actor,
        Some(Actor::Person),
        "NEGATIVE: a pool with no email to renew"
    );
    let mut grok = facts(&GROK);
    grok.home_exists = false;
    grok.pool = Some(crate::ai::facts::CliAccounts {
        roster: PathBuf::from("/home/me/.config/aterm/accounts-grok.toml"),
        next: Some("grok-one".to_string()),
        ..crate::ai::facts::CliAccounts::default()
    });
    assert_eq!(
        assess(&grok, true, NOW).actor,
        Some(Actor::Person),
        "a pool whose page a renewal cannot approve signs in by a person's `accounts login`"
    );
    let mut working = facts(&CODEX);
    working.login_file = Some((PathBuf::from("/home/me/.codex/auth.json"), Some(NOW - 60)));
    assert_eq!(
        health(&[assess(&working, true, NOW)]).next(),
        None,
        "no act"
    );
    assert_eq!(health(&[]).line("x"), "OK");
}

/// A ROW ATERM CANNOT SIGN IN UNATTENDED IS A PERSON'S, AND ITS STEP IS WHAT ONLY A PERSON
/// GIVES (the m2 state of 2026-10-27: a signed-out Claude row with an email, no mailbox and no
/// browser profile of its own, its ladder's record `no-route`): the row is not among aterm's
/// fixes, and the `NEEDS YOU:` line names the row's own browser profile signed in once (`aterm
/// harness accounts browser m2-2`), after which its renewals run headless, never `aterm ai login
/// claude`. A switch that keeps aterm's sign-ins off (`[harness] logins = "off"`) makes a
/// person's own `aterm ai login` the step, and a pool row whose last renewal stopped for a
/// person names the step that stop named. CONTROL: with a route and no switch the row is aterm's.
#[test]
fn a_row_aterm_cannot_sign_in_unattended_is_a_persons_with_the_step_only_a_person_gives() {
    use crate::ai::connector::KIMI;
    use crate::harness::accounts::SignInAction;
    let mut claude = facts(&CLAUDE);
    claude.accounts = Some(ClaudeAccounts {
        pool_picks: false,
        label: "m2-2".into(),
        unattended: false,
        tried: Some(crate::ai::facts::Tried {
            outcome: "no-route",
            routes: "profile(none) mailbox(none configured)".into(),
            since: NOW - 7200,
        }),
        ..accounts(Some("expired"))
    });
    let row = assess(&claude, true, NOW);
    assert_eq!(row.actor, Some(Actor::Person), "{}", row.fact);
    assert_eq!(
        row.fix.as_deref(),
        Some("aterm harness accounts browser m2-2")
    );
    let h = health(std::slice::from_ref(&row));
    assert!(h.by_aterm.is_empty(), "{h:?}");
    assert_eq!(h.person_steps, ["aterm harness accounts browser m2-2"]);
    assert!(
        h.needs_you()
            .is_some_and(|l| !l.contains("aterm ai login claude")),
        "{h:?}"
    );

    let mut off = claude.clone();
    off.accounts = Some(ClaudeAccounts {
        pool_picks: false,
        unattended: true,
        off: Some("`[harness] logins = \"off\"`"),
        ..accounts(Some("expired"))
    });
    let row = assess(&off, true, NOW);
    assert_eq!(row.actor, Some(Actor::Person));
    assert_eq!(row.fix.as_deref(), Some("aterm ai login claude"));
    assert!(row.fact.contains("logins = \"off\""), "{}", row.fact);

    let mut kimi = facts(&KIMI);
    kimi.pool = Some(crate::ai::facts::CliAccounts {
        roster: PathBuf::from("/home/me/.config/aterm/accounts-kimi.toml"),
        next: Some("kimi-one".to_string()),
        state: Some("expired"),
        person_task: Some(SignInAction::Browser),
        ..crate::ai::facts::CliAccounts::default()
    });
    let row = assess(&kimi, true, NOW);
    assert_eq!(row.actor, Some(Actor::Person));
    assert_eq!(
        row.fix.as_deref(),
        Some("aterm harness accounts browser kimi-one --cli kimi")
    );

    let mut control = claude.clone();
    control.accounts = Some(ClaudeAccounts {
        pool_picks: false,
        unattended: true,
        ..accounts(Some("expired"))
    });
    assert_eq!(
        assess(&control, true, NOW).actor,
        Some(Actor::Aterm),
        "CONTROL"
    );
    kimi.pool.as_mut().expect("pool").person_task = None;
    assert_eq!(
        assess(&kimi, true, NOW).actor,
        Some(Actor::Aterm),
        "CONTROL"
    );
}

/// A ROW WHOSE SIGN-IN STARTS NOTHING UNTIL A PERSON DOES SOMETHING ELSE IS NAMED BY THAT, BESIDE
/// ANOTHER ROW'S SIGN-IN: a signed-out Claude row whose accounts.toml is refused offers no
/// command ([`Row::fix`]) and its fact says why; the health line's next act and the one
/// `NEEDS YOU:` line name Codex's own sign-in and what a person does for Claude
/// ([`Health::mends`]), never `aterm ai login claude`, and Claude is not among the rows a person
/// signs in by a command. CONTROL: the same row with accounts.toml admitted is signed in by
/// `aterm ai login claude`.
#[test]
fn a_row_whose_sign_in_starts_nothing_names_what_a_person_does_instead() {
    let mut codex = facts(&CODEX);
    codex.home_exists = false;
    let mut claude = facts(&CLAUDE);
    claude.home_exists = false;
    claude.accounts = Some(ClaudeAccounts {
        pool_picks: false,
        refused: Some("row 1: no label".into()),
        ..accounts(None)
    });
    let row = assess(&claude, true, NOW);
    assert_eq!((row.state, row.fix.as_deref()), (State::SignedOut, None));
    assert_eq!(row.actor, Some(Actor::Person));
    assert!(
        row.fact.contains("accounts.toml is refused"),
        "{}",
        row.fact
    );
    let mend = row.mend.clone().expect("what a person does");
    let h = health(&[assess(&codex, true, NOW), row]);
    assert_eq!(
        (h.by_person.as_slice(), h.person_steps.as_slice()),
        (&["codex"][..], &["aterm ai login codex".to_string()][..])
    );
    assert_eq!(h.mends, std::slice::from_ref(&mend));
    let (next, needs) = (h.next().expect("acts"), h.needs_you().expect("a person"));
    for words in [&next, &needs] {
        assert!(words.contains("`aterm ai login codex`"), "{words}");
        assert!(words.contains(&mend), "{words}");
        assert!(!words.contains("aterm ai login claude"), "{words}");
    }

    claude.accounts = Some(accounts(None));
    let control = assess(&claude, true, NOW);
    assert_eq!(
        (control.fix.as_deref(), control.mend.as_deref()),
        (Some("aterm ai login claude"), None),
        "CONTROL"
    );
}

/// WITH NOBODY AT THIS MAC, A FAILED SIGN-IN ON RECORD OR A BOT CHECK WITHIN THE UPKEEP'S HOLD
/// LEAVES THE ROW TO THE UPKEEP ([`held`]): its ladder retries on its own backoff, so a repeated
/// agent's run never reloads the vendor's page. NEGATIVE: a `no-route` record (which a route
/// found since makes stale), a bot check older than the hold, and no record hold nothing.
#[test]
fn a_failed_sign_in_or_a_bot_check_on_record_holds_the_row_for_the_upkeep() {
    use crate::ai::connector::KIMI;
    let tried = |outcome| crate::ai::facts::Tried {
        outcome,
        routes: "profile: the sign-in did not finish; profile(ready)".into(),
        since: NOW - 600,
    };
    let mut claude = facts(&CLAUDE);
    claude.accounts = Some(ClaudeAccounts {
        pool_picks: false,
        tried: Some(tried("failed")),
        ..accounts(Some("expired"))
    });
    assert!(held(&claude, NOW).is_some());
    assert_eq!(bar(&claude), None, "CONTROL: a failure stays aterm's");
    claude.accounts.as_mut().expect("view").tried = Some(tried("blocked"));
    let stopped = held(&claude, NOW).expect("a page's stop holds the row too");
    assert!(stopped.contains("was stopped by its page"), "{stopped}");
    let person = bar(&claude).expect("a page's stop is a person's");
    assert_eq!(person.step, "aterm harness accounts renew main");
    assert_eq!(sign_in_actor(&claude), Actor::Person);
    claude.accounts.as_mut().expect("view").tried = Some(tried("no-route"));
    assert_eq!(held(&claude, NOW), None, "NEGATIVE: no-route");
    claude.accounts.as_mut().expect("view").tried = None;
    assert_eq!(held(&claude, NOW), None, "NEGATIVE: nothing on record");

    let hold = crate::harness::accounts_sync::PERSON_BACKOFF_SECS;
    let mut kimi = facts(&KIMI);
    kimi.pool = Some(crate::ai::facts::CliAccounts {
        next: Some("kimi-one".to_string()),
        bot_check_at: Some(NOW - hold + 60),
        ..crate::ai::facts::CliAccounts::default()
    });
    assert!(held(&kimi, NOW).is_some());
    kimi.pool.as_mut().expect("pool").bot_check_at = Some(NOW - hold);
    assert_eq!(held(&kimi, NOW), None, "NEGATIVE: past the hold");
}

/// A SIGNED-OUT ROW SAYS WHAT ATERM'S OWN SIGN-IN OF IT LAST CAME TO, where it failed and stands
/// on record: the outcome, how long ago, and the routes its ladder tried.
#[test]
fn a_signed_out_row_says_what_aterms_sign_in_tried() {
    let mut claude = facts(&CLAUDE);
    claude.accounts = Some(ClaudeAccounts {
        pool_picks: false,
        tried: Some(crate::ai::facts::Tried {
            outcome: "no-route",
            routes: "profile(none) mailbox(none configured) vault(sync failed: exit 1)".into(),
            since: NOW - 7200,
        }),
        ..accounts(Some("expired"))
    });
    let row = assess(&claude, true, NOW);
    assert_eq!(row.state, State::SignedOut);
    assert!(
        row.fact.contains(
            " \u{b7} aterm's sign-in found no route 2 h ago: tried profile(none) mailbox(none \
             configured) vault(sync failed: exit 1)"
        ),
        "{}",
        row.fact
    );
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
    a.state = Some("expired");
    a.signed_out_at = Some(NOW - 600);
    f.accounts = Some(a.clone());
    assert_eq!(
        assess(&f, true, NOW).state,
        State::SignedOut,
        "the control: this login's own end stands"
    );
    a.state = Some("idle");
    a.signed_out_at = None;
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

#[test]
fn claude_limited_with_unenrolled_successor_is_due_and_actionable() {
    let mut f = facts(&CLAUDE);
    f.profile_signed_in = true;
    f.limited_until = Some(NOW + 86_400);
    let mut a = accounts(Some("ok"));
    a.label = "m27-3".into();
    a.limited_until = Some(NOW + 86_400);
    f.accounts = Some(a.clone());
    let row = assess(&f, true, NOW);
    assert_eq!(
        (row.state, row.mark),
        (State::Limited(NOW + 86_400), Mark::Note)
    );
    assert_eq!(row.fix, None);

    a.next_sign_in = Some("m27".into());
    f.accounts = Some(a);
    let row = assess(&f, true, NOW);
    assert_eq!(
        (row.state, row.mark),
        (State::Due(NOW + 86_400), Mark::Warn)
    );
    assert!(row.state.needs_act(row.wanted));
    assert_eq!(row.fix.as_deref(), Some("aterm ai login claude"));
    assert!(
        row.fact.contains("next account `m27` needs sign-in"),
        "{}",
        row.fact
    );
}

/// ANOTHER CLI'S POOL is named in its row: how many accounts its roster lists, how many are
/// signed in here and which are due, or why the roster is refused; signed in or out alike. A
/// row with no pool, Claude Code's included, says nothing of one.
#[test]
fn a_clis_pool_is_named_in_its_row_and_a_row_without_one_says_nothing_of_it() {
    use crate::ai::connector::KIMI;
    use crate::ai::facts::CliAccounts;
    let mut f = facts(&KIMI);
    f.login_file = Some((
        PathBuf::from("/home/me/.kimi-code/credentials/kimi-code.json"),
        Some(NOW - 60),
    ));
    let plain = assess(&f, true, NOW);
    assert!(!plain.fact.contains("pool"), "{}", plain.fact);
    f.pool = Some(CliAccounts {
        roster: PathBuf::from("/home/me/.config/aterm/accounts-kimi.toml"),
        rows: 2,
        enrolled: 1,
        signed_in: 1,
        due: vec!["kimi-2".to_string()],
        ..CliAccounts::default()
    });
    let pooled = assess(&f, true, NOW);
    assert!(
        pooled
            .fact
            .contains("pool: 2 accounts (1 signed in here); renewal due: kimi-2"),
        "{}",
        pooled.fact
    );
    assert_eq!(
        (pooled.state, pooled.mark),
        (plain.state, plain.mark),
        "the pool's words change no verdict"
    );
    f.login_file = None;
    let out = assess(&f, true, NOW);
    assert_eq!(out.state, State::SignedOut);
    assert!(
        out.fact.starts_with("no login is stored") && out.fact.contains(" \u{b7} pool: 2 accounts"),
        "{}",
        out.fact
    );
    f.pool = Some(CliAccounts {
        roster: PathBuf::from("/home/me/.config/aterm/accounts-kimi.toml"),
        refused: Some("accounts-kimi.toml: row 1 has no label".to_string()),
        ..CliAccounts::default()
    });
    assert!(
        assess(&f, true, NOW).fact.contains(
            "its pool's roster (accounts-kimi.toml) is refused: accounts-kimi.toml: row 1"
        ),
        "a refused roster says why"
    );
    let mut claude = facts(&CLAUDE);
    claude.profile_signed_in = true;
    assert!(
        !assess(&claude, true, NOW).fact.contains("pool"),
        "Claude Code's row names no pool"
    );
}

fn settings_of(toml: &str) -> Settings {
    let (local, notes) = crate::ai::config::read(toml);
    crate::ai::config::resolve(&local, None, None, notes)
}

/// WHO ASKED: the built-in preferred CLI, a config that names one, and what used one here are asks.
/// NEGATIVE: an installed CLI with a home, a synced pool row and a `use` left at its default are
/// none, and neither is a login aterm's own ladder made under a row.
#[test]
fn a_cli_is_asked_for_by_a_name_or_a_use_never_by_a_synced_row() {
    use crate::ai::facts::CliAccounts;
    let none = settings_of("");
    let mut codex = facts(&CODEX);
    codex.login_file = None;
    assert!(!asked(&codex, &none), "installed, with a home");
    codex.pool = Some(CliAccounts {
        rows: 2,
        emails: 2,
        ..CliAccounts::default()
    });
    assert!(!asked(&codex, &none), "a row the sheet synced");
    codex.login_file = Some((PathBuf::from("/row/auth.json"), None));
    codex.pool.as_mut().expect("pool").serving_login = Some(("CODEX_HOME", Some("/row".into())));
    assert!(!asked(&codex, &none), "the ladder's own login of a row");

    let mut claude = facts(&CLAUDE);
    claude.profile_signed_in = false;
    claude.login_file = None;
    assert!(asked(&claude, &none), "the preferred CLI");
    assert!(
        !asked(&claude, &settings_of("[ai]\npreferred = \"codex\"\n")),
        "NEGATIVE: preferred is the only thing that asked for it"
    );

    let mut bare = facts(&CODEX);
    bare.login_file = None;
    for toml in [
        "[ai]\npreferred = \"codex\"\n",
        "[ai.codex]\nuse = true\n",
        "[ai]\nreviewers = [\"codex\"]\n",
    ] {
        assert!(asked(&bare, &settings_of(toml)), "{toml}");
    }
    let mut used = facts(&CODEX);
    used.login_file = None;
    used.record.ended_at = Some(NOW);
    assert!(asked(&used, &none), "a review met its end");
    let mut served = facts(&CODEX);
    served.login_file = None;
    served.pool = Some(CliAccounts {
        served_at: Some(NOW),
        ..CliAccounts::default()
    });
    assert!(asked(&served, &none), "a launch its pool served");
    let mut own = facts(&CODEX);
    own.login_file = Some((PathBuf::from("/home/.codex/auth.json"), None));
    assert!(asked(&own, &none), "a login in its default home");
}

/// A signed-out or missing CLI nobody asked for leaves the verdict OK with no act and no
/// `NEEDS YOU:`, and its fix stays on the row. CONTROL: the same row asked for fails.
#[test]
fn a_row_nobody_asked_for_is_a_note_with_its_fix_kept() {
    let mut codex = facts(&CODEX);
    codex.home_exists = false;
    let mut agy = facts(&AGY);
    agy.managed = None;
    let asked_rows = [assess(&codex, true, NOW), assess(&agy, true, NOW)];
    assert_eq!(health(&asked_rows).word, "FAIL", "CONTROL");
    let rows = asked_rows.map(Row::not_asked);
    let h = health(&rows);
    assert_eq!(h.word, "OK", "{h:?}");
    assert_eq!(h.noted, ["codex", "agy"]);
    assert_eq!((h.next(), h.needs_you()), (None, None));
    assert_eq!(rows[0].state, State::SignedOut);
    assert_eq!(rows[0].fix.as_deref(), Some("aterm ai login codex"));
    assert!(
        rows[0].fact.contains("nobody has asked"),
        "{}",
        rows[0].fact
    );
    let ok = Row {
        state: State::Ok,
        mark: Mark::Ok,
        fact: "signed in".to_string(),
        ..rows[0].clone()
    }
    .not_asked();
    assert_eq!(
        (ok.mark, ok.fact.as_str()),
        (Mark::Ok, "signed in"),
        "working stays ok"
    );
}

/// THE DOCTOR'S CLAIM: before a run the health line says what it tries, never that it fixes; after
/// a run that tried a row and left it, the line says so instead of offering the same run again.
/// CONTROL: a row that run did not try is still offered.
#[test]
fn the_doctor_never_promises_a_fix_and_owns_one_it_did_not_finish() {
    let mut agy = facts(&AGY);
    agy.managed = None;
    let mut muse = facts(&MUSE);
    muse.managed = None;
    let h = health(&[assess(&agy, true, NOW), assess(&muse, true, NOW)]);
    let before = h.next().expect("acts");
    assert!(before.contains("tries agy and muse itself"), "{before}");
    let after = h.next_after(&["agy"]).expect("acts");
    assert!(after.contains("did not finish agy"), "{after}");
    assert!(after.contains("tries muse itself"), "CONTROL: {after}");
    let all = h.next_after(&["agy", "muse"]).expect("acts");
    assert!(!all.contains("`aterm ai doctor`"), "{all}");
}

/// A PERSON'S STEP THAT ENDS A REFUSED LAUNCH STANDS WHATEVER ELSE THE ROW IS: on a row whose login
/// is due or at a limit, as on one that is well, the owner's pin or roster is the step (a mend, no
/// sign-in fix), so `aterm ai login` says it. The pool's own sign-in of the row the pin holds stays
/// the fix where it signs that row in. CONTROL: a refusal the pool's records end by themselves asks
/// nobody, and leaves a due row's fix.
#[test]
fn a_launch_refused_for_a_persons_step_says_it_whatever_the_row_is() {
    use crate::ai::facts::{CliAccounts, LaunchEnd};
    let row_under = |renew_by: Option<i64>,
                     limited: Option<i64>,
                     end: LaunchEnd,
                     step: Option<&str>,
                     next: Option<&str>| {
        let mut f = facts(&CODEX);
        f.login_file = Some((PathBuf::from("/login/auth.json"), None));
        f.profile_signed_in = true;
        f.limited_until = limited;
        f.pool = Some(CliAccounts {
            state: Some("ok"),
            renew_by,
            launch_refused: Some("every launch is refused".into()),
            launch_end: Some(end),
            launch_step: step.map(str::to_string),
            next: next.map(str::to_string),
            serving_label: Some("main".into()),
            ..CliAccounts::default()
        });
        assess(&f, true, NOW)
    };
    let due = Some(NOW + 3600);
    for (case, renew_by, limited) in [
        ("well", None, None),
        ("due", due, None),
        ("limited", None, Some(NOW + 3600)),
    ] {
        let owner = row_under(renew_by, limited, LaunchEnd::Owner, None, None);
        assert!(owner.mend.is_some(), "{case}: {}", owner.fact);
        assert_eq!(owner.fix, None, "{case}");
        assert_eq!(owner.actor, Some(Actor::Person), "{case}");
        assert!(owner.mark >= Mark::Warn, "{case}: {:?}", owner.mark);
        let step = row_under(renew_by, limited, LaunchEnd::Owner, Some("repin it"), None);
        assert!(
            step.mend
                .as_deref()
                .is_some_and(|mend| mend.contains("repin it")),
            "{case}: {:?}",
            step.mend
        );
        let elsewhere = row_under(renew_by, limited, LaunchEnd::Pool, None, None);
        assert_eq!(elsewhere.mend, None, "{case}: CONTROL");
    }
    let signs_it = row_under(
        due,
        None,
        LaunchEnd::SignIn("work".into()),
        None,
        Some("work"),
    );
    assert_eq!(signs_it.mend, None, "the fix signs in the pinned row");
    assert!(signs_it.fix.is_some(), "{}", signs_it.fact);
    let another = row_under(
        due,
        None,
        LaunchEnd::SignIn("work".into()),
        None,
        Some("main"),
    );
    assert!(another.mend.is_some(), "the fix signs in another row");
    assert_eq!(another.fix, None);
}

/// A ROSTER THE POOL REFUSES WHOLE IS RATED AS WHAT IT IS, WHATEVER THE LOGIN THIS MACHINE HOLDS
/// SAYS: no launch, sync or sign-in of it runs, so the row is `refused`, a warning a person's edit
/// mends, naming that file. CONTROL: the same login under an admitted roster is well.
#[test]
fn a_refused_roster_is_rated_as_what_it_is() {
    use crate::ai::facts::CliAccounts;
    let row_under = |refused: Option<&str>| {
        let mut f = facts(&CODEX);
        f.login_file = Some((PathBuf::from("/login/auth.json"), None));
        f.profile_signed_in = true;
        f.pool = Some(CliAccounts {
            roster: PathBuf::from("/config/accounts-codex.toml"),
            state: Some("ok"),
            refused: refused.map(str::to_string),
            ..CliAccounts::default()
        });
        assess(&f, true, NOW)
    };
    let refused = row_under(Some("two rows name one email"));
    assert_eq!(refused.state, State::Refused, "{}", refused.fact);
    assert_eq!(refused.mark, Mark::Warn);
    assert_eq!(refused.fix, None);
    assert_eq!(refused.actor, Some(Actor::Person));
    let mend = refused.mend.as_deref().expect("a person's step");
    assert!(mend.contains("/config/accounts-codex.toml"), "{mend}");
    assert!(
        health(std::slice::from_ref(&refused))
            .mends
            .iter()
            .any(|step| step == mend),
        "the verdict carries the step"
    );
    let admitted = row_under(None);
    assert_eq!(admitted.state, State::Ok, "CONTROL: {}", admitted.fact);
    assert_eq!(admitted.mend, None, "CONTROL");
}

/// A REFUSED ROSTER'S REASON IS SAID WHOLE, AND THE DOCTOR PRINTS ITS STEP WHETHER ANYONE ASKED FOR
/// THAT CLI OR NOT: at a note, which alerts nobody, the row still has `fix:` under it. CONTROL: an
/// admitted row at a note with a fix has none.
#[test]
fn a_refused_roster_says_its_whole_reason_and_the_doctor_prints_its_step_unasked() {
    use crate::ai::facts::CliAccounts;
    let why = "word ".repeat(60);
    let why = why.trim();
    let mut f = facts(&CODEX);
    f.login_file = Some((PathBuf::from("/login/auth.json"), None));
    f.profile_signed_in = true;
    f.pool = Some(CliAccounts {
        roster: PathBuf::from("/config/accounts-codex.toml"),
        state: Some("ok"),
        refused: Some(why.to_string()),
        ..CliAccounts::default()
    });
    assert!(
        pool_words(&f).expect("its pool's words").ends_with(why),
        "{:?}",
        pool_words(&f)
    );
    let mut unasked = assess(&f, true, NOW);
    unasked.asked = false;
    unasked.mark = Mark::Note;
    assert_eq!(unasked.state, State::Refused);
    assert_eq!(
        crate::ai::cli::doctor_fix(&unasked),
        unasked.mend.as_ref(),
        "the step"
    );
    assert!(unasked.mend.is_some());
    let mut control = assess(&f, true, NOW);
    control.state = State::Ok;
    control.wanted = false;
    control.mark = Mark::Note;
    assert_eq!(crate::ai::cli::doctor_fix(&control), None, "CONTROL");
}

/// A REFUSED ROSTER IS RATED AS WHAT IT IS WHATEVER THE LOGIN IS, THERE OR NOT: recorded signed out,
/// with no login stored, or in a home that is not there, the row is `refused`, its step the person's
/// edit of that file and never `aterm ai login`, which refuses such a roster.
/// CONTROL: under an admitted roster, a row with no login is signed out, with that sign-in.
#[test]
fn a_refused_roster_with_no_login_is_rated_as_what_it_is() {
    use crate::ai::facts::CliAccounts;
    let row_under = |refused: Option<&str>, change: &dyn Fn(&mut crate::ai::facts::Facts)| {
        let mut f = facts(&CODEX);
        f.login_file = None;
        f.pool = Some(CliAccounts {
            roster: PathBuf::from("/config/accounts-codex.toml"),
            refused: refused.map(str::to_string),
            ..CliAccounts::default()
        });
        change(&mut f);
        assess(&f, true, NOW)
    };
    type Change<'a> = &'a dyn Fn(&mut crate::ai::facts::Facts);
    let worlds: [(&str, Change<'_>); 3] = [
        ("no login stored", &|_| {}),
        ("no home", &|f| f.home_exists = false),
        ("recorded signed out", &|f| {
            if let Some(pool) = &mut f.pool {
                pool.state = Some("expired");
            }
        }),
    ];
    for (world, change) in worlds {
        let refused = row_under(Some("two rows name one email"), change);
        assert_eq!(refused.state, State::Refused, "{world}: {}", refused.fact);
        assert_eq!(refused.fix, None, "{world}");
        assert_eq!(refused.actor, Some(Actor::Person), "{world}");
        let mend = refused.mend.as_deref().expect("a person's step");
        assert!(
            mend.contains("/config/accounts-codex.toml"),
            "{world}: {mend}"
        );
    }
    let admitted = row_under(None, &|_| {});
    assert_eq!(
        admitted.state,
        State::SignedOut,
        "CONTROL: {}",
        admitted.fact
    );
    assert_eq!(
        admitted.fix.as_deref(),
        Some("aterm ai login codex"),
        "CONTROL"
    );
}
