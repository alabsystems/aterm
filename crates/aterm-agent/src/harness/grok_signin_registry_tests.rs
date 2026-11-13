// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{accounts::Store, alab_roster, headless_oauth::registry_tests as fixture};
use super::*;

/// With no mailbox file present, the live rung's row finds the registry's mailbox for its
/// email, so the email is typed and the code read from that mailbox.
/// NEGATIVE CONTROL: with no registry mailbox, the row has none (`read.mailbox` false).
#[test]
fn the_live_rung_reads_its_mailbox_from_the_registry() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let roster = tmp.path().join("grok-accounts.toml");
    std::fs::write(
        &roster,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"ai1@example.test\"\n",
    )
    .unwrap();
    let store = Store::at_for(&state, Cli::Grok, &home);
    let read = |mail: Vec<alab_roster::LabMailbox>| {
        alab_roster::with_mail(mail, fixture::secrets(), || {
            LiveAuto::new(
                Some(roster.clone()),
                Some(store.clone()),
                Some(home.clone()),
                0,
            )
            .row()
            .expect("a store")
        })
    };
    let found = read(vec![fixture::mailbox()]);
    assert!(
        found.mailbox,
        "the registry's mailbox receives the row's email"
    );
    assert_eq!(found.approver.mailbox_unready_login, None);
    assert!(!read(Vec::new()).mailbox);
}
