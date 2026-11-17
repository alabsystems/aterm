// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `accounts add <name>` for an account the lab knows, on a scratch HOME and store; every address
//! is synthetic (`example.test`).

use super::*;
use crate::harness::alab_roster::test_machine;
use crate::harness::alab_roster::{Lab, LabAccount, with_lab};

fn lab() -> Lab {
    let account = |local: &str, label: &str, joinable: bool| LabAccount {
        pk: format!("ac-{:0>16}", local.len()),
        cli: Cli::Claude,
        vendor: accounts::Vendor::default(),
        label: label.to_owned(),
        legacy_label: None,
        email: format!("{local}@example.test"),
        identity: None,
        browser: None,
        joinable,
        preferred: vec![(test_machine("m20"), Some(1))],
    };
    Lab {
        me: Some(test_machine("m22")),
        accounts: vec![
            account("ann", "claude-aaaaaa", true),
            account("carol", "claude-cccccc", false),
        ],
        ..Lab::default()
    }
}

fn add(env: &Env, label: &str) -> (ExitCode, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_accounts_add(
        Cli::Claude,
        env,
        &accounts::Row {
            label: label.into(),
            ..accounts::Row::default()
        },
        &mut out,
        &mut err,
    );
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// An account the lab knows, asked for by name with no address, joins this Mac's roster under its
/// lab label with its address, as a pool row (another Mac prefers it), and the command says no
/// preference was written; an account that may not be signed in is refused and nothing is
/// written. With no lab held, the name is a new label as it always was.
/// NEGATIVE CONTROL: with `run_accounts_add`'s `asked_row` arm removed, the row has no address
/// (`the lab's address`).
#[test]
fn an_account_asked_for_by_name_joins_this_roster_and_writes_no_preference() {
    let tmp = tmp_dir("accounts-asked");
    let env = env_at(&tmp);
    crate::test_config::write(env.config.as_ref().unwrap(), "").unwrap();
    let roster_path = env.accounts_roster().unwrap();
    with_lab(lab(), || {
        let (code, out, err) = add(&env, "claude-aaaaaa");
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(out.contains("asked for by name"), "{out}");
        assert!(out.contains("no preference was written"), "{out}");
        let roster = accounts::Roster::load(&roster_path).unwrap();
        let row = roster.row("claude-aaaaaa").expect("the lab's label");
        assert_eq!(
            row.email.as_deref(),
            Some("ann@example.test"),
            "the lab's address"
        );
        assert_eq!(row.class, accounts::Class::Pool);
        let before = std::fs::read(&roster_path).unwrap();
        let (code, _, err) = add(&env, "claude-cccccc");
        assert_eq!(code, ExitCode::from(1));
        assert!(err.contains("nothing was written"), "{err}");
        assert_eq!(std::fs::read(&roster_path).unwrap(), before);
    });
    let (code, _, err) = add(&env, "plain");
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    let roster = accounts::Roster::load(&roster_path).unwrap();
    assert_eq!(roster.row("plain").and_then(|r| r.email.clone()), None);
}

/// A lab account this roster already lists under its legacy label (`m22-2`, which no sync renames
/// to the lab's `claude-bbbbbb`) is listed, not added again: `add claude-bbbbbb` and `add m22-2`
/// each name the `m22-2` row and its sign-in step, exit 1 and write nothing, as an `add` of a
/// label the roster lists always did. So is an `add` of a new label with that row's address.
/// NEGATIVE CONTROL: with `accounts_add_admitted`'s lookups by the name asked for and by account
/// (`roster.row(named)`, `Roster::row_of_account`) removed, `add claude-bbbbbb` appends a second
/// row of the account and the roster's own refusal answers (`name one email`), telling the person
/// to remove a row that is not there (`already listed`).
#[test]
fn an_account_listed_under_its_legacy_label_is_named_not_added_again() {
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
    let text = accounts::append_rows_for(Cli::Claude, "", &[legacy]).unwrap();
    std::fs::create_dir_all(roster_path.parent().unwrap()).unwrap();
    std::fs::write(&roster_path, &text).unwrap();
    let mut lab = lab();
    lab.accounts.push(LabAccount {
        pk: "ac-000000000000000b".into(),
        cli: Cli::Claude,
        vendor: accounts::Vendor::default(),
        label: "claude-bbbbbb".into(),
        legacy_label: Some("m22-2".into()),
        email: "bob@example.test".into(),
        identity: None,
        browser: None,
        joinable: true,
        preferred: vec![(test_machine("m22"), Some(1))],
    });
    with_lab(lab, || {
        for (name, says) in [
            (
                "claude-bbbbbb",
                "the account `claude-bbbbbb` names is already listed",
            ),
            ("m22-2", "`m22-2` is already listed"),
        ] {
            let (code, out, err) = add(&env, name);
            assert_eq!(code, ExitCode::from(1), "{name}: {out}{err}");
            assert!(err.contains(says), "{name}: {err}");
            assert!(
                err.contains("`aterm harness accounts renew m22-2` signs it in"),
                "{name}: {err}"
            );
            assert_eq!(
                std::fs::read_to_string(&roster_path).unwrap(),
                text,
                "{name}"
            );
        }
    });
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_accounts_add(
        Cli::Claude,
        &env,
        &accounts::Row {
            label: "bob".into(),
            email: Some("Bob@example.test".into()),
            ..accounts::Row::default()
        },
        &mut out,
        &mut err,
    );
    let err = String::from_utf8(err).unwrap();
    assert_eq!(code, ExitCode::from(1), "{err}");
    assert!(
        err.contains("the account `bob` names is already listed") && err.contains("as `m22-2`"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&roster_path).unwrap(), text);
}
