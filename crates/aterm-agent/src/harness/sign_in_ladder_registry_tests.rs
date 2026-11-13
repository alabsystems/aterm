// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{
    accounts::Row,
    accounts_sync::mailbox_offer,
    alab_roster,
    headless_oauth::{discover_mailbox_config_files, registry_tests as fixture},
};
use super::*;

/// With no mailbox file or sheet present, the upkeep's offer for a row whose email the
/// registry's webmail mailbox receives is that mailbox with the row's password, its login
/// masked, and the ladder routes the sign-in through it, all without revealing a value; a
/// registry mailbox with no password held is named as such and never tried.
/// NEGATIVE CONTROL: with `mailbox_offer` asking `password_for` again instead of
/// `holds_for`, the `reveals() == 0` assertion reads 1.
#[test]
fn the_ladder_routes_a_row_through_its_registry_mailbox_without_a_reveal() {
    let home = aterm_tempfile::tempdir().unwrap();
    let row = Row {
        label: "claude-1".to_owned(),
        email: Some("ai1@example.test".to_owned()),
        ..Row::default()
    };
    let offer = |mail| {
        alab_roster::with_mail(mail, fixture::secrets(), || {
            let cfg = discover_mailbox_config_files(Some(home.path()), None, None);
            (mailbox_offer(false, cfg, &row), alab_roster::reveals())
        })
    };
    let ((mailbox, login), reveals) = offer(vec![fixture::mailbox()]);
    assert_eq!(mailbox, Mailbox::WebmailOnly);
    assert_eq!(login.as_deref(), Some("ow…@…"));
    assert_eq!(reveals, 0, "the upkeep's look reveals nothing");
    let read = Read {
        profile: Profile::None,
        mailbox,
        mailbox_login: login,
        vault: None,
        browser: true,
        row_browser: false,
        hosts: 0,
    };
    assert_eq!(read.route(), Some(Route::Mailbox));
    assert!(read.holds_mailbox());

    let mut unheld = fixture::mailbox();
    unheld.held.clear();
    let ((mailbox, login), _) = offer(vec![unheld]);
    assert_eq!(
        (mailbox, login.as_deref()),
        (Mailbox::SharedNoPassword, Some("ow…@…"))
    );
}
