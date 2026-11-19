// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A warm native restart's completion naming the account intent its Execute readmitted.

use super::*;
use crate::harness::accounts_pool::{ExecutionIntent, Purpose};
use crate::harness::native_act::ClaimOutcome;

fn stored(owner: &str, generation: &str, purpose: Purpose) -> ExecutionIntent {
    ExecutionIntent::Stored {
        namespace: "11".repeat(32),
        login: Some("/logins/work-2".into()),
        owner: Some(owner.into()),
        generation: generation.repeat(32),
        purpose,
    }
}

/// A warm restart in flight whose forward command carries `planned`, the record keeping that
/// same intent, as the plan saved them.
fn in_flight(planned: &ExecutionIntent) -> St {
    let vendor = Process::observe(std::process::id()).unwrap();
    let mut shell = vendor.clone();
    shell.pid = if vendor.pid == 2 { 3 } else { 2 };
    shell.group = shell.pid;
    let mut command = super::planned();
    command.account_intent = Some(planned.clone());
    let prepare = Prepare {
        receipt_mode: None,
        rejection_domain: None,
        asked: None,
        id: "ab".repeat(16),
        host_epoch: "ef".repeat(16),
        holder: "aterm-harness@fixture".into(),
        revision: 1,
        expected_human_seq: Some(0),
        deadline_us: 1_000_000,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            process: vendor.clone(),
            shell,
            launcher: vendor.image,
            socket: "/fixture-native.sock".into(),
            forward: Box::new(command.capture().unwrap()),
            rollback: None,
            observation: None,
            cold: None,
        },
        boot: None,
        custody: None,
    };
    St {
        native_prepare: Request::prepare(prepare).encode().unwrap(),
        native: Some(super::binding()),
        account_intent: Some(planned.clone()),
        phase: Phase::Exiting { at_s: 1 },
        ..St::default()
    }
}

fn completed(intent: &ExecutionIntent) -> Reply {
    Reply::AdmittedRestartCommitted {
        claim: ClaimOutcome::Builtin,
        observation: None,
        intent: Box::new(intent.clone()),
    }
}

/// A WARM COMPLETION KEEPS THE LOGIN ITS EXECUTE READMITTED (a login renewed between a native
/// restart's signal and its Execute): the host's committed reply names the same login captured
/// afresh, its credential generation moved, and the record completes keeping that intent in place
/// of the planned one; the repeated answer acknowledges it. CONTROLS: a reply naming another
/// owner row, namespace, selector or purpose, an external route or a vault token is no
/// readmission of the planned login, and completes nothing (the record unchanged).
///
/// NEGATIVE CONTROLS (2026-11-13, each restored with cp from a backup, confirmed with cmp): with a
/// named intent answering only a cold restart (the rule before), the renewed login's completion
/// was refused; with any intent answering a warm restart, the other owner's completed it.
#[test]
fn a_warm_completion_keeps_the_login_its_execute_readmitted_and_nothing_else() {
    let planned = stored("work-2", "22", Purpose::SameLogin);
    let renewed = stored("work-2", "33", Purpose::SameLogin);
    let mut st = in_flight(&planned);
    assert_eq!(
        terminal_reply(&mut st, &completed(&renewed)),
        Some(Recovery::Completed)
    );
    assert_eq!(st.account_intent.as_ref(), Some(&renewed));
    assert!(st.native.is_none() && st.native_prepare.is_empty());
    assert!(st.native_completed_prepare.is_some());
    let kept = st.clone();
    assert_eq!(
        terminal_reply(&mut st, &completed(&renewed)),
        Some(Recovery::Completed),
        "the repeated answer acknowledges the completion"
    );
    assert_eq!(st, kept);

    let mut namespace = renewed.clone();
    let mut login = renewed.clone();
    if let ExecutionIntent::Stored {
        namespace: other, ..
    } = &mut namespace
    {
        *other = "44".repeat(32);
    }
    if let ExecutionIntent::Stored { login: other, .. } = &mut login {
        *other = Some("/logins/other".into());
    }
    let token = ExecutionIntent::Token {
        namespace: "11".repeat(32),
        login: None,
        owner: Some("work-2".into()),
        entry: crate::harness::accounts::TokenEntry::parse(
            "aterm-claude-token-ac-0123456789abcdef",
        )
        .unwrap(),
        purpose: Purpose::SameLogin,
    };
    for (what, intent) in [
        (
            "another owner row",
            stored("work-3", "33", Purpose::SameLogin),
        ),
        ("another namespace", namespace),
        ("another selector", login),
        ("another purpose", stored("work-2", "33", Purpose::Select)),
        ("an external route", ExecutionIntent::external()),
        ("a vault token", token),
    ] {
        let mut st = in_flight(&planned);
        let before = st.clone();
        assert_eq!(
            terminal_reply(&mut st, &completed(&intent)),
            None,
            "{what} is no readmission of the planned login"
        );
        assert_eq!(st, before, "{what}");
        let mut done = kept.clone();
        assert_eq!(
            terminal_reply(&mut done, &completed(&intent)),
            None,
            "{what} answers no completed restart either"
        );
        assert_eq!(done, kept, "{what}");
    }
}
