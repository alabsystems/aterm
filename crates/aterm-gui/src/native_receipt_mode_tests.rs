// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_agent::harness::native_act::ReceiptMode;
use aterm_agent::harness::native_receipts::testing::{Journal, RecipientAppend};

fn marked(request: &mut Prepare, facts: &Facts) {
    cold_restart(request);
    request.receipt_mode = Some(ReceiptMode::Recipients5);
    request.rejection_domain = Some("12".repeat(32));
    request.boot = facts.boot();
    request.custody = Some("56".repeat(32));
    assert!(Request::prepare(request.clone()).encode().is_some());
}

fn no_admission(ctx: &crate::SessionCtx, facts: &Facts) {
    let table = ctx.fenced_writes.native.lock().unwrap();
    assert!(table.by_id.is_empty());
    assert!(table.reservations.is_empty());
    assert!(table.cancelled.is_empty());
    assert!(facts.journal.lock().unwrap().is_empty());
    assert!(facts.prepared_history.lock().unwrap().is_empty());
    assert!(facts.receipt_history.lock().unwrap().is_empty());
    assert_eq!(facts.grant_journal_calls.load(Ordering::SeqCst), 0);
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
    assert_eq!(facts.watch_count.load(Ordering::SeqCst), 0);
}

#[test]
fn recognized_complete_mode_never_enters_a_legacy_admission_path() {
    for case in 0..3 {
        let (handle, facts, mut request, actor) = closed_restart::fixture(86900 + case);
        marked(&mut request, &facts);
        if case == 1 {
            request.host_epoch = "34".repeat(16);
        } else if case == 2 {
            request.deadline_us = 1;
            facts.clock.store(2, Ordering::SeqCst);
        }
        assert_eq!(
            run(&handle.ctx, &facts, Request::prepare(request), actor),
            RECIPIENT_WRITER_UNAVAILABLE
        );
        no_admission(&handle.ctx, &facts);
    }
}

#[test]
fn refusing_new_writes_preserves_exact_complete_history_reads() {
    for (case, terminal) in [RecipientAppend::NoExec, RecipientAppend::Seal]
        .into_iter()
        .enumerate()
    {
        let home = aterm_tempfile::tempdir().unwrap();
        let (handle, mut facts, mut request, actor) = closed_restart::fixture(86910 + case as u64);
        marked(&mut request, &facts);
        request.host_epoch = "34".repeat(16);
        facts.native_journal = Some(Journal::new(home.path()));
        let binding = Binding {
            id: request.id.clone(),
            capability: "cd".repeat(32),
        };
        let journal = facts.native_journal.as_ref().unwrap();
        journal
            .record_recipient(&request, &binding, RecipientAppend::Prepare)
            .unwrap();
        journal
            .record_recipient(&request, &binding, terminal)
            .unwrap();
        let expected = if case == 0 {
            NOEXEC.to_string()
        } else {
            aterm_agent::harness::native_act::lapsed_reply(&request.id).unwrap()
        };
        assert_eq!(
            run(&handle.ctx, &facts, Request::prepare(request), actor),
            expected
        );
        assert_eq!(journal.record_count().unwrap(), 2);
        no_admission(&handle.ctx, &facts);
    }
}
