// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::cli::shared_provider::MailboxInputs;

#[test]
fn explicit_approved_mailbox_refusals_never_reach_authorization() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let target = "approved@example.test";
    let inputs = MailboxInputs::decode(br#"{"schema":1,"kind":"mailbox","target":"approved@example.test","transport":"https","endpoint":"https://bound.example.test/inbox","bearer_token":"synthetic"}"#, target).unwrap();
    let staged = inputs.stage_in(temp.path()).unwrap();
    let called = std::cell::Cell::new(0);
    with_approved_mailbox(
        staged.path(),
        Some(target),
        None,
        |cfg, named, profile_store| {
            called.set(called.get() + 1);
            assert_eq!(named, target);
            assert_eq!(
                cfg.http_url.as_deref(),
                Some("https://bound.example.test/inbox")
            );
            assert!(cfg.dir.is_none());
            assert!(
                profile_store.is_none(),
                "the ambient browser store is never selected"
            );
            Ok(())
        },
    )
    .unwrap();
    for target in [None, Some("someone-else@example.test")] {
        assert!(
            with_approved_mailbox(staged.path(), target, None, |_, _, _| {
                called.set(called.get() + 1);
                Ok(())
            })
            .is_err()
        );
    }
    assert!(
        with_approved_mailbox(
            staged.path(),
            Some(target),
            Some("required-profile"),
            |_, _, _| {
                called.set(called.get() + 1);
                Ok(())
            }
        )
        .is_err()
    );
    aterm_uds::private_file::write_private(
        staged.path().parent().unwrap(),
        "mailbox.json",
        b"not JSON",
    )
    .unwrap();
    assert!(
        with_approved_mailbox(staged.path(), Some(target), None, |_, _, _| {
            called.set(called.get() + 1);
            Ok(())
        })
        .is_err()
    );
    std::fs::remove_file(staged.path()).unwrap();
    assert!(
        with_approved_mailbox(staged.path(), Some(target), None, |_, _, _| {
            called.set(called.get() + 1);
            Ok(())
        })
        .is_err()
    );
    assert_eq!(called.get(), 1);
}
