// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole raw init must retain a verified Mac stage when an independent stage fails.

use super::*;
use crate::macos_doctor::accounts_visibility_tests::{Account, system};
use aterm_machines::init::{Held, Records, converge_workstation_for_test};
use aterm_machines::items::{Ctx, Item};
use aterm_machines::model::{ItemReport, Outcome, Probe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const PREFERENCE: &str = "init.test.independent-preference";

struct IndependentPreference {
    fails: bool,
    attempts: Arc<AtomicUsize>,
}

impl Item for IndependentPreference {
    fn id(&self) -> String {
        PREFERENCE.into()
    }

    fn family(&self) -> &'static str {
        "init"
    }

    fn title(&self) -> String {
        "Independent preference".into()
    }

    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        if self.fails {
            Probe::Change {
                from: "before".into(),
                to: "after".into(),
            }
        } else {
            Probe::Ok {
                now: "after".into(),
            }
        }
    }

    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err("injected independent preference failure".into())
    }
}

struct InitAt {
    root: PathBuf,
    whole: Mutex<Vec<bool>>,
}

impl Records for InitAt {
    fn hold(&self, whole: bool, wait: &dyn Fn(&str)) -> Result<Box<dyn Held>, String> {
        self.whole.lock().unwrap().push(whole);
        Ok(Box::new(init_hold_at(&self.root, whole, wait)?))
    }
}

struct Pass {
    rows: Vec<ItemReport>,
    accounts: Vec<String>,
    preference_attempts: usize,
    holds: Vec<bool>,
}

fn account(unreadable: bool) -> Arc<Account> {
    Arc::new(Account {
        signed_in: AtomicBool::new(true),
        unreadable,
        signouts: AtomicUsize::new(0),
    })
}

fn raw_init(
    root: &Path,
    account: &Arc<Account>,
    preference_fails: bool,
    omit_one_account: bool,
    narrowed: bool,
) -> Pass {
    let sys = system(None, account);
    let records = InitAt {
        root: root.to_path_buf(),
        whole: Mutex::default(),
    };
    let attempts = Arc::new(AtomicUsize::new(0));
    let expected = Mutex::new(Vec::new());
    let prepare = |ctx: &Ctx| {
        let mut accounts = aterm_machines::init_accounts::items(ctx)?;
        *expected.lock().unwrap() = accounts.iter().map(|item| item.id()).collect();
        if omit_one_account {
            accounts.pop().expect("two native services were built");
        }
        let preferences: Vec<Box<dyn Item>> = vec![Box::new(IndependentPreference {
            fails: preference_fails,
            attempts: Arc::clone(&attempts),
        })];
        Ok(vec![preferences, accounts])
    };
    let only = if narrowed {
        vec!["init.messages.signed-out@alice".into()]
    } else {
        Vec::new()
    };
    let rows = converge_workstation_for_test(&sys, &records, &only, &prepare).unwrap();
    Pass {
        rows,
        accounts: expected.into_inner().unwrap(),
        preference_attempts: attempts.load(Ordering::SeqCst),
        holds: records.whole.into_inner().unwrap(),
    }
}

fn native_rows(pass: &Pass) -> Vec<&ItemReport> {
    pass.rows
        .iter()
        .filter(|row| pass.accounts.contains(&row.id))
        .collect()
}

fn resume(root: &Path, account: &Arc<Account>) {
    let _profile = HeldLock::take(root).unwrap();
    initial::resume_native_accounts_after_init_for_test(root, account).unwrap();
}

#[test]
fn init_stage_outcomes_verified_accounts_survive_an_independent_preference_failure() {
    let root = aterm_tempfile::tempdir().unwrap();
    let account = account(false);
    let pass = raw_init(root.path(), &account, true, false, false);
    assert_eq!(pass.holds, [true]);
    assert_eq!(pass.preference_attempts, 1);
    assert!(
        pass.rows
            .iter()
            .any(|row| { row.id == PREFERENCE && row.outcome == Outcome::Failed })
    );
    let accounts = native_rows(&pass);
    assert_eq!(accounts.len(), 2);
    assert!(
        accounts
            .iter()
            .all(|row| { matches!(row.outcome, Outcome::Ok | Outcome::Changed) })
    );
    assert_eq!(account.signouts.load(Ordering::SeqCst), 1);
    assert!(!account.signed_in.load(Ordering::SeqCst));
    assert!(!root.path().join(HELD_RECORD).exists());
    let before = initial::initial_setup_snapshot_at(root.path(), Default::default()).unwrap();
    assert!(before.run_allowed && !before.finished);

    // A later login belongs to the person. The failed preference may resume, but
    // the same admitted run must retain the native sign-outs it already verified.
    account.signed_in.store(true, Ordering::SeqCst);
    resume(root.path(), &account);
    assert_eq!(
        account.signouts.load(Ordering::SeqCst),
        1,
        "the later startup must not repeat a verified account stage"
    );
    assert!(account.signed_in.load(Ordering::SeqCst));
}

#[test]
fn init_stage_outcomes_a_fully_verified_whole_pass_preserves_a_later_login() {
    let root = aterm_tempfile::tempdir().unwrap();
    let account = account(false);
    let pass = raw_init(root.path(), &account, false, false, false);
    assert_eq!(pass.holds, [true]);
    assert_eq!(pass.preference_attempts, 0);
    assert_eq!(native_rows(&pass).len(), 2);
    assert!(
        pass.rows
            .iter()
            .all(|row| { matches!(row.outcome, Outcome::Ok | Outcome::Changed) })
    );
    assert_eq!(account.signouts.load(Ordering::SeqCst), 1);
    account.signed_in.store(true, Ordering::SeqCst);
    resume(root.path(), &account);
    assert_eq!(account.signouts.load(Ordering::SeqCst), 1);
    assert!(account.signed_in.load(Ordering::SeqCst));
}

#[test]
fn init_stage_outcomes_unreadable_accounts_are_still_resumed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let unreadable = account(true);
    let pass = raw_init(root.path(), &unreadable, false, false, false);
    assert_eq!(pass.holds, [true]);
    assert_eq!(native_rows(&pass).len(), 2);
    assert!(
        native_rows(&pass)
            .iter()
            .all(|row| { row.outcome == Outcome::Unknown })
    );
    assert_eq!(unreadable.signouts.load(Ordering::SeqCst), 0);

    let readable = account(false);
    resume(root.path(), &readable);
    assert_eq!(readable.signouts.load(Ordering::SeqCst), 1);
    assert!(!readable.signed_in.load(Ordering::SeqCst));
}

#[test]
fn init_stage_outcomes_missing_native_service_cannot_credit_the_whole_account_stage() {
    let root = aterm_tempfile::tempdir().unwrap();
    let account = account(false);
    let pass = raw_init(root.path(), &account, false, true, false);
    assert_eq!(pass.holds, [true]);
    assert_eq!(pass.accounts.len(), 2);
    assert_eq!(native_rows(&pass).len(), 1);
    assert!(
        pass.rows
            .iter()
            .all(|row| { matches!(row.outcome, Outcome::Ok | Outcome::Changed) })
    );
    assert_eq!(account.signouts.load(Ordering::SeqCst), 1);
    account.signed_in.store(true, Ordering::SeqCst);
    resume(root.path(), &account);
    assert_eq!(
        account.signouts.load(Ordering::SeqCst),
        2,
        "one verified native service cannot complete both selected services"
    );
    assert!(!account.signed_in.load(Ordering::SeqCst));
}

#[test]
fn init_stage_outcomes_a_narrowed_pass_does_not_admit_the_whole_profile() {
    let root = aterm_tempfile::tempdir().unwrap();
    let account = account(false);
    let pass = raw_init(root.path(), &account, false, false, true);
    assert_eq!(pass.holds, [false]);
    assert_eq!(pass.preference_attempts, 0);
    assert_eq!(pass.rows.len(), 1);
    assert_eq!(native_rows(&pass).len(), 1);
    assert_eq!(account.signouts.load(Ordering::SeqCst), 1);
    for record in [
        "initial-setup.json",
        "automatic-plan.json",
        "first-run.json",
        HELD_RECORD,
    ] {
        assert!(!root.path().join(record).exists(), "{record}");
    }
}
