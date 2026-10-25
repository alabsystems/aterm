// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn bare() -> Read {
    Read {
        profile: Profile::None,
        mailbox: Mailbox::None,
        vault: None,
        browser: false,
        row_browser: false,
        hosts: 0,
    }
}

#[test]
fn a_machine_with_nothing_has_no_route_and_names_every_one_it_tried() {
    let read = Read {
        vault: Some(Vault::Unchanged(51)),
        ..bare()
    };
    assert_eq!(read.route(), None);
    assert_eq!(
        read.words(),
        "profile(none) mailbox(none configured) vault(seq 51, nothing new) \
         headless-browser(none installed) hosts(none configured)"
    );
    let with_desk = Read {
        row_browser: true,
        hosts: 2,
        vault: Some(Vault::NotMember),
        ..bare()
    };
    let words = with_desk.words();
    assert!(words.contains("vault(not a member)"), "{words}");
    assert!(
        words.contains("desk-browser(opens a window: not used unattended)"),
        "a row's desk browser is named and never a route: {words}"
    );
    assert!(words.contains("hosts(2: each signs in only its own logins)"));
    assert_eq!(with_desk.route(), None, "a desk browser is never a route");
}

/// Each route's words at their longest (every field's longest word, the longest vault finding,
/// many hosts, the longest way a run ends, a failed run's prefix) fit the record and the ledger
/// reason whole, so no route a no-route or failed record names is ever cut off.
#[test]
fn the_longest_words_fit_the_record_and_the_ledger_whole() {
    use super::super::accounts::{AUTO_SIGN_IN, AttemptOutcome, LEDGER_WHY_MAX, ROUTES_MAX};
    use super::super::cli::Renewal;
    let longest = |words: Vec<String>| words.into_iter().max_by_key(String::len).unwrap();
    let vaults = [
        Vault::NotMember,
        Vault::Applied(u64::MAX),
        Vault::Unchanged(u64::MAX),
        Vault::NotIn,
        Vault::of_sync("", Some(i32::MIN)),
        Vault::of_sync("", None),
        Vault::of_run_error("it will not run: No such file or directory", Duration::MAX),
        Vault::of_run_error("it ran past its 45000 ms deadline", Duration::MAX),
        Vault::of_run_error("its output was unreadable: broken pipe", Duration::MAX),
        Vault::Failed("this program is not aterm".to_owned()),
        Vault::Failed("it could not be started".to_owned()),
    ];
    let vault = vaults
        .into_iter()
        .max_by_key(|vault| vault.words().len())
        .unwrap();
    let mut reads = Vec::new();
    for profile in [Profile::Ready, Profile::WrongAccount, Profile::None] {
        for mailbox in [Mailbox::Ready, Mailbox::WebmailOnly, Mailbox::None] {
            reads.push(Read {
                profile,
                mailbox,
                vault: Some(vault.clone()),
                browser: false,
                row_browser: true,
                hosts: 99_999,
            });
        }
    }
    let read = reads.into_iter().max_by_key(|r| r.words().len()).unwrap();
    let how = longest(
        [
            Renewal::Renewed,
            Renewal::AuthFailed,
            Renewal::WrongAccount,
            Renewal::Failed,
            Renewal::NeedsPerson,
        ]
        .into_iter()
        .map(|renewal| how_it_ended(renewal, Some(i32::MIN)))
        .collect(),
    );
    let records = [
        read.words(),
        failed_words(Route::Mailbox, &how, &read),
        failed_words(Route::Profile, &how, &read),
    ];
    for record in &records {
        assert!(
            record.len() <= ROUTES_MAX,
            "{} > {ROUTES_MAX}: {record}",
            record.len()
        );
        let outcome = longest(
            [
                AttemptOutcome::NoRoute,
                AttemptOutcome::Failed,
                AttemptOutcome::SignedIn,
            ]
            .into_iter()
            .map(|o| o.as_str().to_owned())
            .collect(),
        );
        let reason = format!("{AUTO_SIGN_IN} {outcome}: {record}");
        assert!(reason.len() <= LEDGER_WHY_MAX, "{reason}");
    }
    for route in [
        "profile(",
        "mailbox(",
        "vault(",
        "headless-browser(",
        "desk-browser(",
        "hosts(",
    ] {
        assert!(records[1].contains(route), "{route}: {}", records[1]);
    }
}

/// A run that made no login is named by how it exited, never as a refusal nobody gave.
#[test]
fn a_run_that_made_no_login_says_how_it_exited_and_claims_no_refusal() {
    use super::super::cli::Renewal;
    let said = how_it_ended(Renewal::AuthFailed, Some(1));
    assert!(
        said.contains("exited 1") && said.contains("no login made"),
        "{said}"
    );
    assert!(!how_it_ended(Renewal::AuthFailed, None).contains("refused"));
    assert!(!said.contains("refused"), "{said}");
}

#[test]
fn each_route_needs_what_it_drives() {
    assert_eq!(
        Read {
            mailbox: Mailbox::Ready,
            ..bare()
        }
        .route(),
        Some(Route::Mailbox),
        "a mailbox read over a directory, IMAP or HTTP needs no browser"
    );
    let webmail = Read {
        mailbox: Mailbox::WebmailOnly,
        ..bare()
    };
    assert_eq!(webmail.route(), None, "a webmail inbox needs a browser");
    assert_eq!(
        Read {
            browser: true,
            ..webmail
        }
        .route(),
        Some(Route::Mailbox)
    );
    let profile = Read {
        profile: Profile::Ready,
        ..bare()
    };
    assert_eq!(profile.route(), None, "a profile needs a browser to drive");
    assert_eq!(
        Read {
            browser: true,
            ..profile.clone()
        }
        .route(),
        Some(Route::Profile)
    );
    assert_eq!(
        Read {
            browser: true,
            profile: Profile::WrongAccount,
            ..profile
        }
        .route(),
        None,
        "a profile whose last sign-in was another account is not driven again"
    );
    assert_eq!(
        Read {
            browser: true,
            profile: Profile::Ready,
            mailbox: Mailbox::Ready,
            ..bare()
        }
        .route(),
        Some(Route::Mailbox),
        "the mailbox comes first"
    );
}

#[test]
fn the_backoff_doubles_from_fifteen_minutes_to_six_hours() {
    assert_eq!(backoff_after(1), BACKOFF_FIRST);
    assert_eq!(backoff_after(2), BACKOFF_FIRST * 2);
    assert_eq!(backoff_after(3), BACKOFF_FIRST * 4);
    assert_eq!(backoff_after(5), BACKOFF_MAX - BACKOFF_FIRST * 8);
    assert_eq!(backoff_after(6), BACKOFF_MAX);
    assert_eq!(backoff_after(60), BACKOFF_MAX);
    assert_eq!(backoff_after(u32::MAX), BACKOFF_MAX);
}

#[test]
fn a_failing_row_waits_until_its_backoff_ends_or_its_inputs_change() {
    let mut ladder = Ladder::default();
    let t0 = Instant::now();
    assert!(!ladder.held("m2", "nothing", t0), "a row never tried runs");
    assert_eq!(ladder.failed("m2", "nothing", t0), BACKOFF_FIRST);
    assert!(ladder.held("m2", "nothing", t0 + Duration::from_secs(60)));
    assert!(
        !ladder.held("m2", "a mailbox", t0 + Duration::from_secs(60)),
        "a change of what the ladder reads ends the wait at once"
    );
    assert!(!ladder.held("m2-2", "nothing", t0), "each row has its own");
    let t1 = t0 + BACKOFF_FIRST;
    assert!(!ladder.held("m2", "nothing", t1), "the wait ends");
    assert_eq!(ladder.failed("m2", "nothing", t1), BACKOFF_FIRST * 2);
    assert_eq!(
        ladder.failed("m2", "a mailbox", t1),
        BACKOFF_FIRST,
        "new inputs start the backoff again"
    );
    ladder.signed_in("m2");
    assert!(!ladder.held("m2", "a mailbox", t1));
}

#[test]
fn the_vault_is_refreshed_once_per_wait_and_its_wait_doubles_while_it_brings_nothing() {
    let mut ladder = Ladder::default();
    let t0 = Instant::now();
    assert_eq!(ladder.vault_turn(t0), VaultTurn::Due);
    assert_eq!(
        ladder.vault_turn(t0),
        VaultTurn::Running,
        "one refresh at a time"
    );
    ladder.vault_done(Vault::Unchanged(51));
    assert_eq!(
        ladder.vault_turn(t0 + Duration::from_secs(60)),
        VaultTurn::Seen(Vault::Unchanged(51))
    );
    let t1 = t0 + BACKOFF_FIRST;
    assert_eq!(ladder.vault_turn(t1), VaultTurn::Due);
    ladder.vault_done(Vault::Unchanged(51));
    assert_eq!(
        ladder.vault_turn(t1 + BACKOFF_FIRST),
        VaultTurn::Seen(Vault::Unchanged(51)),
        "the second refresh that brought nothing doubled the wait"
    );
    let t2 = t1 + BACKOFF_FIRST * 2;
    assert_eq!(ladder.vault_turn(t2), VaultTurn::Due);
    ladder.vault_done(Vault::Applied(52));
    assert_eq!(
        ladder.vault_turn(t2 + BACKOFF_FIRST),
        VaultTurn::Due,
        "an applied vault sets the first wait again"
    );
}

#[test]
fn a_sync_says_what_it_did_in_its_first_line() {
    assert_eq!(
        Vault::of_sync(
            "applied seq 52: 1 written, 5 unchanged, 0 removed, 0 left in place, 0 moved aside\n",
            Some(0)
        ),
        Vault::Applied(52)
    );
    assert_eq!(
        Vault::of_sync("up to date at seq 51\n", Some(0)),
        Vault::Unchanged(51)
    );
    assert_eq!(
        Vault::of_sync(
            "the source held seq 50, older than this Mac's: put this Mac's copy back; up to date \
             at seq 51\n",
            Some(0)
        ),
        Vault::Unchanged(51)
    );
    assert_eq!(
        Vault::of_sync(
            "this machine is not in the vault (yet): aterm secrets status\n",
            Some(1)
        ),
        Vault::NotIn
    );
    assert_eq!(
        Vault::of_sync("", Some(1)),
        Vault::Failed("exit 1".to_owned())
    );
    assert_eq!(
        Vault::of_sync("", None),
        Vault::Failed("no exit status".to_owned())
    );
}

/// A refresh that did not run to its end says which way: never started, ended at its deadline,
/// or its output unreadable; the error's own text (a path, an OS word) is not kept.
#[test]
fn a_sync_that_did_not_finish_says_how() {
    let budget = Duration::from_secs(45);
    let words = |error: &str| Vault::of_run_error(error, budget).words();
    assert!(
        words("it ran past its 45000 ms deadline").contains("timed out after 45 s"),
        "{}",
        words("it ran past its 45000 ms deadline")
    );
    let unstarted = words("it will not run: No such file or directory (os error 2) /x/aterm");
    assert!(unstarted.contains("could not be started"), "{unstarted}");
    assert!(!unstarted.contains("/x/aterm"), "{unstarted}");
    assert!(words("its output was unreadable: x").contains("could not be read"));
}

#[test]
fn the_log_line_names_the_row_how_it_ended_and_the_routes() {
    let line = log_line("m2", "no-route", "profile(none) mailbox(none configured)");
    for part in [
        "m2",
        "no-route",
        "profile(none)",
        "mailbox(none configured)",
    ] {
        assert!(line.contains(part), "{part}: {line}");
    }
    assert!(!line.contains('\n'));
}
