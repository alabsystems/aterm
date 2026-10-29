// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;

use super::*;
use crate::ai::connector::{AGY, CLAUDE, CODEX, GROK, MUSE};
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
        home_var: None,
        home_exists: true,
        login_file: None,
        hidden_store: false,
        profile_signed_in: false,
        plan: None,
        accounts: None,
        limited_until: None,
        record: Record::default(),
        env_credentials: Vec::new(),
        pool: None,
    }
}

fn accounts(state: Option<&'static str>) -> ClaudeAccounts {
    ClaudeAccounts {
        label: "main".into(),
        listed: false,
        refused: None,
        unread: None,
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
    for kept in [Kept::Unavailable, Kept::Excluded, Kept::Removed] {
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
    f.home_exists = false;
    let row = assess(&f, true, NOW);
    assert_eq!((row.state, row.mark), (State::Unproven, Mark::Note));
    assert!(row.fact.contains("not measured"), "{}", row.fact);
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
        tried: Some(tried("failed")),
        ..accounts(Some("expired"))
    });
    assert!(held(&claude, NOW).is_some());
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
