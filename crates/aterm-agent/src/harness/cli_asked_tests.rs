// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `accounts add <account>` for an account the lab knows, on a scratch HOME and store; every
//! address is synthetic (`example.test`).

use super::*;
use crate::harness::alab_roster::test_machine;
use crate::harness::alab_roster::{Lab, LabAccount, with_lab};
use aterm_db::typed::keys;

fn key(email: &str) -> String {
    keys::account_pk("claude", email).expect("an account key")
}

/// The lab's names of the Claude account at `email`, its slug `slug`, as LabView reads them.
fn names(email: &str, slug: &str) -> (String, aterm_file::AccountNames) {
    (
        key(email),
        aterm_file::AccountNames {
            realm: "claude".to_owned(),
            slug: slug.to_owned(),
            name: keys::account_name("claude", email),
            email: Some(email.to_owned()),
            login: Some(email.to_owned()),
            live: true,
        },
    )
}

/// The label LabView gives the pooled account at `email`: its reserved slug, which is never its
/// slug and names nothing.
fn reserved(email: &str) -> String {
    keys::reserved_slug("claude", &key(email))
}

/// The lab's names of each of `lab`'s accounts, its slug `slugs[i]` (the lab's own label for it,
/// never the reserved slug LabView labels its pooled row with).
fn named(mut lab: Lab, slugs: &[&str]) -> Lab {
    lab.names = lab
        .accounts
        .iter()
        .zip(slugs)
        .map(|(account, slug)| names(&account.email, slug))
        .collect();
    lab
}

/// A pooled Claude account of the lab at `<local>@example.test`, labelled with its reserved
/// slug as LabView labels it.
fn pooled(local: &str, joinable: bool) -> LabAccount {
    let email = format!("{local}@example.test");
    LabAccount {
        pk: key(&email),
        cli: Cli::Claude,
        vendor: accounts::Vendor::default(),
        label: reserved(&email),
        legacy_label: None,
        email,
        identity: None,
        browser: None,
        joinable,
        preferred: vec![(test_machine("m20"), Some(1))],
    }
}

/// The lab of these tests, naming this Mac `m22`: `ann` (slug `claude-aaaaaa`), joinable, and
/// `carol` (slug `claude-cccccc`), paused.
fn lab() -> Lab {
    named(
        Lab {
            me: Some(test_machine("m22")),
            accounts: vec![pooled("ann", true), pooled("carol", false)],
            ..Lab::default()
        },
        &["claude-aaaaaa", "claude-cccccc"],
    )
}

fn add(env: &Env, word: &str) -> (ExitCode, String, String) {
    add_listing(env, word, &Listing::default())
}

fn add_listing(env: &Env, word: &str, listing: &Listing<'_>) -> (ExitCode, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_accounts_add(
        Cli::Claude,
        env,
        &FakeHost::new(&never_asked),
        word,
        listing,
        &mut out,
        &mut err,
    );
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// An account the lab knows, asked for by any of its words (its slug, its provider word and
/// address, its address, its key) with no address of the person's, joins this Mac's roster under
/// the lab's label (its reserved slug) with the lab's address, as a pool row (another Mac prefers
/// it), and the command says no preference was written, naming the account by its slug, never its
/// label; the label itself names nothing; an account that may not be signed in is refused and
/// nothing is written. With no lab held, a would-be label names no account and is refused; an
/// account named by its provider word and address is listed under its reserved slug.
/// NEGATIVE CONTROL: with `asked_row_from` first taking the lab account whose label is the word,
/// it fails `the label names nothing` (2026-11-16, by hand on SF-07H S6's review settle, then restored).
#[test]
fn an_account_asked_for_by_name_joins_this_roster_and_writes_no_preference() {
    let ann = key("ann@example.test");
    let label = reserved("ann@example.test");
    with_lab(lab(), || {
        assert!(
            crate::harness::alab_roster::asked_row(Cli::Claude, &label).is_none(),
            "the label names nothing"
        );
    });
    for word in [
        "claude-aaaaaa",
        "claude ann@example.test",
        "ann@example.test",
        ann.as_str(),
    ] {
        let tmp = tmp_dir("accounts-asked");
        let env = env_at(&tmp);
        crate::test_config::write(env.config.as_ref().unwrap(), "").unwrap();
        let roster_path = env.accounts_roster().unwrap();
        with_lab(lab(), || {
            let (code, out, err) = add(&env, word);
            assert_eq!(code, ExitCode::SUCCESS, "{word}: {err}");
            assert!(out.contains("asked for by name"), "{word}: {out}");
            assert!(out.contains("no preference was written"), "{word}: {out}");
            assert!(
                out.contains("listed claude ann@example.test (claude-aaaaaa) (pool)"),
                "{word}: {out}"
            );
            assert!(!out.contains(&label), "no label is printed: {out}");
            let roster = accounts::Roster::load(&roster_path).unwrap();
            let row = roster.row(&label).expect("the lab's label");
            assert_eq!(
                row.email.as_deref(),
                Some("ann@example.test"),
                "the lab's address"
            );
            assert_eq!(row.class, accounts::Class::Pool);
            let before = std::fs::read(&roster_path).unwrap();
            let (code, _, err) = add(&env, "claude-cccccc");
            assert_eq!(code, ExitCode::from(1));
            assert!(
                err.contains(
                    "the lab's claude carol@example.test (claude-cccccc) may not be \
                     signed in"
                ) && err.contains("nothing was written"),
                "{err}"
            );
            assert_eq!(std::fs::read(&roster_path).unwrap(), before);
        });
    }
    let tmp = tmp_dir("accounts-asked-nolab");
    let env = env_at(&tmp);
    crate::test_config::write(env.config.as_ref().unwrap(), "").unwrap();
    let roster_path = env.accounts_roster().unwrap();
    let (code, _, err) = add(&env, "plain");
    assert_eq!(code, ExitCode::from(1));
    assert!(err.contains(&aterm_file::no_account("plain")), "{err}");
    assert!(!roster_path.exists(), "nothing was written");
    let (code, _, err) = add(&env, "claude plain@example.test");
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    let roster = accounts::Roster::load(&roster_path).unwrap();
    let label = keys::reserved_slug("claude", &key("plain@example.test"));
    assert_eq!(
        roster.row(&label).and_then(|r| r.email.clone()).as_deref(),
        Some("plain@example.test")
    );
}

/// A lab account this roster already lists under an older label (`m22-2`, which no sync renames
/// to the lab's `claude-bbbbbb`) is listed, not added again: an `add` by each of its words names
/// the account and its sign-in step, exits 1 and writes nothing; so does one by its provider word
/// and address with no lab held, in another case. The older label names nothing, so its `add` is
/// refused as no account's.
#[test]
fn an_account_listed_under_an_older_label_is_named_not_added_again() {
    let tmp = tmp_dir("accounts-asked-legacy");
    let env = env_at(&tmp);
    crate::test_config::write(env.config.as_ref().unwrap(), "").unwrap();
    let roster_path = env.accounts_roster().unwrap();
    let dir = env.accounts_store().login_dir("m22-2").unwrap();
    let legacy = accounts::Row {
        label: "m22-2".into(),
        email: Some("bob@example.test".into()),
        dir: Some(dir.to_str().unwrap().to_owned()),
        ..accounts::Row::default()
    };
    let text = accounts::append_rows_for(Cli::Claude, "", std::slice::from_ref(&legacy)).unwrap();
    std::fs::create_dir_all(roster_path.parent().unwrap()).unwrap();
    std::fs::write(&roster_path, &text).unwrap();
    let mut lab = lab();
    let bob = key("bob@example.test");
    lab.accounts.push(LabAccount {
        legacy_label: Some("m22-2".into()),
        preferred: vec![(test_machine("m22"), Some(1))],
        ..pooled("bob", true)
    });
    let lab = named(lab, &["claude-aaaaaa", "claude-cccccc", "claude-bbbbbb"]);
    // The sign-in step names the row by its account's word under the lab, never `m22-2`.
    let renew = format!(
        "`aterm harness accounts renew {}` signs it in",
        crate::harness::account_names::sign_in_word(
            Cli::Claude,
            &legacy,
            std::slice::from_ref(&legacy),
            Some(&lab)
        )
    );
    let listed = format!(
        "claude bob@example.test (claude-bbbbbb) is already listed in {}",
        roster_path.display()
    );
    with_lab(lab, || {
        for word in [
            "claude-bbbbbb",
            "claude bob@example.test",
            "claude:Bob@example.test",
            bob.as_str(),
        ] {
            let (code, out, err) = add(&env, word);
            assert_eq!(code, ExitCode::from(1), "{word}: {out}{err}");
            assert!(err.contains(&listed), "{word}: {err}");
            assert!(err.contains(&renew), "{word}: {err}");
            assert!(!err.contains("m22-2"), "{word}: {err}");
            assert_eq!(
                std::fs::read_to_string(&roster_path).unwrap(),
                text,
                "{word}"
            );
        }
        let (code, _, err) = add(&env, "m22-2");
        assert_eq!(code, ExitCode::from(1));
        assert!(err.contains(&aterm_file::no_account("m22-2")), "{err}");
    });
    let (code, _, err) = add(&env, "claude Bob@example.test");
    assert_eq!(code, ExitCode::from(1), "{err}");
    assert!(
        err.contains("claude bob@example.test")
            && err.contains("is already listed")
            && err.contains("nothing was written"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&roster_path).unwrap(), text);
}

/// Whatever path an `add` or `adopt-default` lists an account by, the lab's word for it holds:
/// an account the lab bars from a sign-in (paused) is refused on a Mac the lab does not name yet
/// (no pooled row is asked for) as where it does, and by `adopt-default` too; an account the lab
/// retired is refused, as is one whose names hold no address; and the lab's account takes no
/// `--email` but one of the addresses the lab holds for it, so a line written for a label that is
/// now an account's slug (`add claude-aaaaaa --email work@corp.example`) lists nothing, while its
/// own login address is taken. Nothing is written by any refusal.
/// NEGATIVE CONTROL: with `listed_row`'s `bars` check removed, the paused account is listed on
/// the Mac the lab does not name (exit 0, not 1); with `lab_email_refusal` answering `None`, the
/// old label line lists ann's account (`named Mac: an --email the lab does not hold`); with
/// `listed_row` standing in an address for one the names lack, the add lists rows (`no address`)
/// (2026-11-16, by hand on SF-07H S6's review settle, then restored).
#[test]
fn the_labs_status_and_addresses_hold_on_every_path_that_lists_its_account() {
    let unnamed = Lab { me: None, ..lab() };
    let fresh = || {
        let tmp = tmp_dir("accounts-asked-paths");
        let env = env_at(&tmp);
        crate::test_config::write(env.config.as_ref().unwrap(), "").unwrap();
        (tmp, env)
    };
    for (lab, why) in [
        (lab(), "paused, named Mac"),
        (unnamed.clone(), "paused, unnamed Mac"),
    ] {
        let (_tmp, env) = fresh();
        with_lab(lab, || {
            let (code, out, err) = add(&env, "claude-cccccc");
            assert_eq!(code, ExitCode::from(1), "{why}: {out}{err}");
            assert!(
                err.contains(
                    "the lab's claude carol@example.test (claude-cccccc) may not be signed in"
                ) && err.contains("nothing was written"),
                "{why}: {err}"
            );
            assert!(!env.accounts_roster().unwrap().exists(), "{why}");
        });
    }
    let (_tmp, env) = fresh();
    with_lab(unnamed.clone(), || {
        let (code, out, err) = acc(
            AccountsCmd::AdoptDefault {
                account: Some("claude-cccccc".into()),
                email: None,
                browser: None,
            },
            &env,
            &FakeHost::new(&never_asked),
        );
        assert_eq!(code, exit(1), "adopt-default: {out}{err}");
        assert!(err.contains("may not be signed in"), "adopt-default: {err}");
        assert!(!env.accounts_roster().unwrap().exists(), "adopt-default");
    });
    let mut retired = unnamed.clone();
    let ann = key("ann@example.test");
    retired.names.get_mut(&ann).unwrap().live = false;
    let (_tmp, env) = fresh();
    with_lab(retired, || {
        let (code, out, err) = add(&env, "claude-aaaaaa");
        assert_eq!(code, ExitCode::from(1), "retired: {out}{err}");
        assert!(
            err.contains("is retired, so it joins no roster"),
            "retired: {err}"
        );
        assert!(!env.accounts_roster().unwrap().exists(), "retired");
    });
    let mut no_address = unnamed.clone();
    let entry = no_address.names.get_mut(&ann).unwrap();
    entry.email = None;
    entry.login = None;
    let (_tmp, env) = fresh();
    with_lab(no_address, || {
        let (code, out, err) = add(&env, "claude-aaaaaa");
        assert_eq!(code, ExitCode::from(1), "no address: {out}{err}");
        assert!(
            err.contains("(claude-aaaaaa) has no address here, so no row can list it"),
            "no address: {err}"
        );
        assert!(!env.accounts_roster().unwrap().exists(), "no address");
    });
    for (lab, why) in [(lab(), "named Mac"), (unnamed.clone(), "unnamed Mac")] {
        let (_tmp, env) = fresh();
        with_lab(lab, || {
            let listing = Listing {
                email: Some("work@corp.example"),
                ..Listing::default()
            };
            let (code, out, err) = add_listing(&env, "claude-aaaaaa", &listing);
            assert_eq!(
                code,
                ExitCode::from(1),
                "{why}: an --email the lab does not hold: {out}{err}"
            );
            assert!(
                err.contains("is the lab's account, which holds its addresses, and --email names none of them")
                    && !err.contains("work@corp.example"),
                "{why}: {err}"
            );
            assert!(!env.accounts_roster().unwrap().exists(), "{why}");
            let listing = Listing {
                email: Some("Ann@example.test"),
                ..Listing::default()
            };
            let (code, out, err) = add_listing(&env, "claude-aaaaaa", &listing);
            assert_eq!(code, ExitCode::SUCCESS, "{why}: its own login: {out}{err}");
            let roster = accounts::Roster::load(&env.accounts_roster().unwrap()).unwrap();
            assert!(roster.row(&reserved("ann@example.test")).is_some(), "{why}");
        });
    }
}
