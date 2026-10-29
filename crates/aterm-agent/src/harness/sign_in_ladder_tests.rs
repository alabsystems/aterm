// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::open_url::{Met, MetWord};

fn bare() -> Read {
    Read {
        profile: Profile::None,
        mailbox: Mailbox::None,
        mailbox_login: None,
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
        for mailbox in [
            Mailbox::Ready,
            Mailbox::WebmailOnly,
            Mailbox::NoPassword,
            Mailbox::SharedNoPassword,
            Mailbox::None,
        ] {
            for browser in [true, false] {
                reads.push(Read {
                    profile,
                    mailbox,
                    mailbox_login: Some(super::super::mailbox_routes::mask_login(
                        "\u{1F600}\u{1F600}@a-long-mail-domain.example.test",
                    )),
                    vault: Some(vault.clone()),
                    browser,
                    row_browser: true,
                    hosts: 99_999,
                });
            }
        }
    }
    // A no-route record names any read; a failed run's, a read with the route it ran.
    let longest_read = |routed: bool| {
        reads
            .iter()
            .filter(|r| r.route().is_some() == routed)
            .max_by_key(|r| r.words().len())
            .unwrap()
            .clone()
    };
    let (read, ran) = (longest_read(false), longest_read(true));
    let host = "h".repeat(super::super::open_url::Met::HOST_MAX);
    let mets: Vec<Option<Met>> = std::iter::once(None)
        .chain(
            [MetWord::BotCheck, MetWord::BlockPage, MetWord::Stall]
                .into_iter()
                .map(|word| Some(Met::new(word, Some(&host)))),
        )
        .collect();
    let how = longest(
        [
            Renewal::Renewed,
            Renewal::AuthFailed,
            Renewal::WrongAccount,
            Renewal::Failed,
            Renewal::NeedsPerson,
        ]
        .into_iter()
        .flat_map(|renewal| {
            mets.iter()
                .map(move |met| how_it_ended(renewal, Some(i32::MIN), met.as_ref()))
        })
        .collect(),
    );
    assert!(how.contains(&host), "the longest names its host: {how}");
    let records = [
        read.words(),
        failed_words(Route::Mailbox, &how, &ran),
        failed_words(Route::Profile, &how, &ran),
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
    let said = how_it_ended(Renewal::AuthFailed, Some(1), None);
    assert!(
        said.contains("exited 1") && said.contains("no login made"),
        "{said}"
    );
    assert!(!how_it_ended(Renewal::AuthFailed, None, None).contains("refused"));
    assert!(!said.contains("refused"), "{said}");
}

/// A run whose opener met Cloudflare's bot check says so on record, after how the run exited,
/// naming Cloudflare and the host and never a person; a block page and a page that reached no
/// callback are named too, and a run that signed in names nothing it met. What a run met never
/// enters the backoff key, which is what the ladder read before the run: the same read fails
/// into the next step of the same backoff whatever the page met. CONTROL: a run with no outcome
/// file says what it said before.
#[test]
fn a_run_that_met_a_bot_check_names_it_on_record_and_keeps_its_backoff_key() {
    use super::super::cli::Renewal;
    let bot = Met::new(MetWord::BotCheck, Some("claude.ai"));
    let said = how_it_ended(Renewal::AuthFailed, Some(1), Some(&bot));
    let after = |said: &str, what: &str| {
        said.find(what)
            .is_some_and(|at| said.find(&bot.words()).is_some_and(|met| at < met))
    };
    assert!(
        after(&said, "exited 1"),
        "how it exited, then what it met: {said}"
    );
    assert!(
        said.contains("Cloudflare") && said.contains("claude.ai") && !said.contains("person"),
        "{said}"
    );
    let unfinished = how_it_ended(Renewal::Failed, None, Some(&bot));
    assert!(
        unfinished.contains(&bot.words())
            && unfinished.starts_with(&how_it_ended(Renewal::Failed, None, None)),
        "{unfinished}"
    );
    for met in [
        Met::new(MetWord::BlockPage, Some("claude.ai")),
        Met::new(MetWord::Stall, Some("claude.com")),
    ] {
        let said = how_it_ended(Renewal::AuthFailed, Some(1), Some(&met));
        assert!(
            said.contains("exited 1") && said.ends_with(&met.words()),
            "{said}"
        );
    }
    assert_eq!(
        how_it_ended(Renewal::Renewed, Some(0), Some(&bot)),
        how_it_ended(Renewal::Renewed, Some(0), None),
        "a run that signed in names nothing it met"
    );
    let before = how_it_ended(Renewal::AuthFailed, Some(1), None);
    assert!(
        before.contains("exited 1") && !before.contains("Cloudflare"),
        "{before}"
    );

    let read = Read {
        profile: Profile::Ready,
        browser: true,
        ..bare()
    };
    let record = failed_words(Route::Profile, &said, &read);
    assert!(
        record.starts_with("profile: claude auth login exited 1")
            && record.contains(&bot.words())
            && record.contains("headless-browser(installed)"),
        "{record}"
    );
    let inputs = read.words();
    assert!(!inputs.contains("Cloudflare"), "{inputs}");
    let mut ladder = Ladder::default();
    let t0 = Instant::now();
    assert_eq!(ladder.failed("row", &inputs, t0), BACKOFF_FIRST);
    assert_eq!(
        ladder.failed("row", &read.words(), t0 + BACKOFF_FIRST),
        BACKOFF_FIRST * 2,
        "a run that met a stall after one that met a bot check steps the same backoff"
    );
}

/// An installed headless browser that no route drives is named as not run, never as found;
/// one a route drives is named installed. CONTROL: none installed says so.
#[test]
fn an_installed_browser_no_route_drives_is_named_not_run() {
    let idle = Read {
        browser: true,
        ..bare()
    };
    assert_eq!(idle.route(), None);
    assert!(
        idle.words().contains("headless-browser(installed; not run"),
        "{}",
        idle.words()
    );
    let driven = Read {
        profile: Profile::Ready,
        ..idle.clone()
    };
    assert_eq!(driven.route(), Some(Route::Profile));
    assert!(driven.words().contains("headless-browser(installed) "));
    assert!(
        bare().words().contains("headless-browser(none installed)"),
        "{}",
        bare().words()
    );
    assert!(!idle.words().contains("found") && !driven.words().contains("found"));
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
    assert!(!ladder.held("m2-2", "nothing", t0), "each row has its own");
    let t1 = t0 + BACKOFF_FIRST;
    assert!(!ladder.held("m2", "nothing", t1), "the wait ends");
    assert_eq!(ladder.failed("m2", "nothing", t1), BACKOFF_FIRST * 2);
    assert!(
        !ladder.held("m2", "a mailbox", t1),
        "a change of what the ladder reads ends the wait at once"
    );
    assert!(
        !ladder.held("m2", "nothing", t1),
        "returning to an earlier input cannot restore an obsolete wait"
    );
    assert_eq!(
        ladder.failed("m2", "a mailbox", t1),
        BACKOFF_FIRST,
        "new inputs start the backoff again"
    );
    ladder.signed_in("m2");
    assert!(!ladder.held("m2", "a mailbox", t1));
}

/// A move of this machine's network (docs/DESIGN-fluid-swarm-2026-10-28.md F3c) ends the vault's
/// wait, whose clock starts again from the first wait, and each row's backoff, which keeps its
/// step; a refresh running then runs on.
/// NEGATIVE CONTROL: a `moved` that leaves the waits (or the vault's doubled clock) as they were
/// fails each assertion after it.
#[test]
fn a_move_ends_the_vault_wait_and_each_rows_backoff() {
    let mut ladder = Ladder::default();
    let t0 = Instant::now();
    assert_eq!(ladder.vault_turn(Some([1; 32]), t0), VaultTurn::Due);
    ladder.vault_done(Vault::Failed("the vault host did not answer".to_owned()));
    let t1 = t0 + BACKOFF_FIRST;
    assert_eq!(ladder.vault_turn(Some([1; 32]), t1), VaultTurn::Due);
    ladder.vault_done(Vault::Failed("the vault host did not answer".to_owned()));
    assert_eq!(ladder.failed("m2", "no-route", t1), BACKOFF_FIRST);
    let away = t1 + Duration::from_secs(60);
    assert!(ladder.held("m2", "no-route", away));
    assert!(matches!(
        ladder.vault_turn(Some([1; 32]), away),
        VaultTurn::Seen(_)
    ));
    ladder.moved(away);
    assert!(!ladder.held("m2", "no-route", away), "the row runs at once");
    assert_eq!(
        ladder.vault_turn(Some([1; 32]), away),
        VaultTurn::Due,
        "the vault is asked at once"
    );
    ladder.moved(away);
    assert_eq!(
        ladder.vault_turn(Some([1; 32]), away),
        VaultTurn::Running,
        "a running refresh runs on"
    );
    ladder.vault_done(Vault::Unchanged(51));
    assert_eq!(
        ladder.vault_result([1; 32], away),
        None,
        "the refresh that crossed the network move is not reusable"
    );
    assert_eq!(ladder.vault_turn(Some([1; 32]), away), VaultTurn::Due);
    ladder.vault_done(Vault::Unchanged(51));
    assert_eq!(
        ladder.vault_turn(Some([1; 32]), away + BACKOFF_FIRST),
        VaultTurn::Due,
        "the vault's clock started again from the first wait"
    );
    assert_eq!(
        ladder.failed("m2", "no-route", away),
        BACKOFF_FIRST * 2,
        "the row's step was kept"
    );
}

#[test]
fn the_vault_is_refreshed_once_per_wait_and_its_wait_doubles_while_it_brings_nothing() {
    let mut ladder = Ladder::default();
    let t0 = Instant::now();
    assert_eq!(ladder.vault_turn(Some([1; 32]), t0), VaultTurn::Due);
    assert_eq!(
        ladder.vault_turn(Some([1; 32]), t0),
        VaultTurn::Running,
        "one refresh at a time"
    );
    ladder.vault_done(Vault::Unchanged(51));
    assert_eq!(
        ladder.vault_turn(Some([1; 32]), t0 + Duration::from_secs(60)),
        VaultTurn::Seen(Vault::Unchanged(51))
    );
    let t1 = t0 + BACKOFF_FIRST;
    assert_eq!(ladder.vault_turn(Some([1; 32]), t1), VaultTurn::Due);
    ladder.vault_done(Vault::Unchanged(51));
    assert_eq!(
        ladder.vault_turn(Some([1; 32]), t1 + BACKOFF_FIRST),
        VaultTurn::Seen(Vault::Unchanged(51)),
        "the second refresh that brought nothing doubled the wait"
    );
    let t2 = t1 + BACKOFF_FIRST * 2;
    assert_eq!(ladder.vault_turn(Some([1; 32]), t2), VaultTurn::Due);
    ladder.vault_done(Vault::Applied(52));
    assert_eq!(
        ladder.vault_turn(Some([1; 32]), t2 + BACKOFF_FIRST),
        VaultTurn::Due,
        "an applied vault sets the first wait again"
    );
}

#[test]
fn a_sync_says_what_it_did_in_its_first_line() {
    assert_eq!(
        Vault::of_sync(
            "applied seq 52: 1 written, 5 unchanged, 0 removed, 0 left in place, 0 moved aside, \
             0 failed\n",
            Some(0)
        ),
        Vault::Applied(52)
    );
    // One that left a file unwritten (or unremoved) counts it: the vault was applied, but not
    // wholly, and the row's line says so rather than reading as a clean apply. Negative control:
    // the line's seq alone takes it for `Applied`.
    let partly = Vault::of_sync(
        "applied seq 53: 1 written, 5 unchanged, 0 removed, 0 left in place, 0 moved aside, \
         1 failed\n  mail.toml: failed: Permission denied\n",
        Some(1),
    );
    assert_eq!(partly, Vault::AppliedPartly(53));
    assert_eq!(partly.words(), "applied seq 53, some files failed");
    // The count decides, not the exit status, which work after the files (the accounts the
    // same process syncs next) or a signal can make non-zero.
    let whole = "applied seq 54: 1 written, 5 unchanged, 0 removed, 0 left in place, 0 moved \
                 aside, 0 failed\n";
    assert_eq!(Vault::of_sync(whole, Some(101)), Vault::Applied(54));
    assert_eq!(Vault::of_sync(whole, None), Vault::Applied(54));
    // A line without the count (an older aterm) is read by the exit status.
    let older = "applied seq 55: 1 written, 5 unchanged, 0 removed, 0 left in place, 0 moved \
                 aside\n";
    assert_eq!(Vault::of_sync(older, Some(0)), Vault::Applied(55));
    assert_eq!(Vault::of_sync(older, Some(1)), Vault::AppliedPartly(55));
    // Either way the files it did write are read again.
    assert!(Vault::Applied(52).wrote_files() && partly.wrote_files());
    assert!(!Vault::Unchanged(51).wrote_files() && !Vault::NotIn.wrote_files());
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

#[test]
#[cfg(unix)]
fn structured_sync_requires_known_evidence_and_agreeing_exit_status() {
    use aterm_secrets::ErrorKind;
    use aterm_secrets::sync_diagnostic::SyncDiagnostic;
    for (record, expected) in [
        (
            SyncDiagnostic::Applied { seq: 4, failed: 0 },
            Vault::Applied(4),
        ),
        (
            SyncDiagnostic::Applied { seq: 4, failed: 2 },
            Vault::AppliedPartly(4),
        ),
        (SyncDiagnostic::Unchanged(5), Vault::Unchanged(5)),
        (SyncDiagnostic::PutBack(6), Vault::Unchanged(6)),
        (SyncDiagnostic::NotIn, Vault::NotIn),
        (
            SyncDiagnostic::Failed(ErrorKind::JournalBusy),
            Vault::Failed(
                aterm_secrets::sync_diagnostic::failure_reason(ErrorKind::JournalBusy).to_owned(),
            ),
        ),
    ] {
        let code = if record.success() { 0 } else { 1 };
        assert_eq!(Vault::of_sync(&record.line(), Some(code)), expected);
        assert!(matches!(
            Vault::of_sync(&record.line(), Some(1 - code)),
            Vault::Failed(_)
        ));
        assert!(matches!(
            Vault::of_sync(&record.line(), None),
            Vault::Failed(_)
        ));
    }
}

#[test]
fn malformed_sync_diagnostics_never_fall_back_to_successful_prose() {
    for malformed in [
        "aterm-secrets-sync-v1 unknown up to date at seq 99",
        "aterm-secrets-sync-v2 up to date at seq 99",
        "aterm-secrets-sync-v1 applied 7 0\napplied seq 99",
        "aterm-secrets-sync-v1 failed /private/credential-value",
    ] {
        assert!(matches!(
            Vault::of_sync(malformed, Some(0)),
            Vault::Failed(_)
        ));
    }
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
