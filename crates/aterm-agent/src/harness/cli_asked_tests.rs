// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `accounts add <name>` for an account the lab knows, on a scratch HOME and store; every address
//! is synthetic (`example.test`).

use super::*;
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
        preferred: vec![("m20".to_owned(), Some(1))],
    };
    Lab {
        me: Some("m22".into()),
        settled: false,
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
    std::fs::write(env.config.as_ref().unwrap(), "").unwrap();
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
