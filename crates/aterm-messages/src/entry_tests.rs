// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the host-facing entry ([`crate::exceptions`]): the upgrade and Full Disk
//! Access entries' words and every fault refused where the entry is written.

use crate::exceptions::{Entry, EntryFault, Family, WHAT_CAP};
use crate::model::{Intent, Severity, UpgradeWord};
use crate::need::{Mover, Need, Reason, Resurface, Then, Why};

/// Tab 1, after a day: one entry per kind (no subject), the words the owner reads, the
/// navigation beside the sentence, the holders behind `Details`.
fn tab_one() -> Entry {
    Entry::new(
        Family::Upgrade,
        "",
        "own-work",
        "Claude in tab 1 is still on 2.1.283",
        // The holder that outlives a turn is named, and the action is the one that
        // ends it.
        Why::OnlyYou(Reason::new(
            "aterm never ends running work, and a job it started 5 h ago (diskwatch.sh) still runs",
        )),
        Then::step("End that job in the tab"),
    )
    .show(Intent::ShowTab { tab: 1, window: 1 })
    .more("held by pid 36855 (zsh, 5h47m) and 8 tool calls under 4 minutes old")
    .more("aterm harness upgrade s-3e79b5bde172ad0088ad --status shows the whole picture")
}

/// Full Disk Access: one entry, the button that does the action first.
fn full_disk_access() -> Entry {
    Entry::new(
        Family::Privacy,
        "fda",
        "needed",
        "macOS keeps asking for access",
        // Stand-ins for aterm-gui's `consent_card::FDA_WHY` / `FDA_THEN` (that crate
        // depends on this one); the host imports them, it never retypes them.
        Why::OnlyYou(Reason::new("macOS forgets an Allow when aterm updates")),
        Then::press(
            Mover::open_system_pane("full-disk-access"),
            "turn on aterm in Full Disk Access",
        ),
    )
    .resurface(Resurface::CHOICE)
}

#[test]
fn the_tab_one_entry_says_why_the_owner_and_the_one_action() {
    let e = tab_one();
    assert_eq!(e.key(), "harness.upgrade.own-work");
    let m = e.message().expect("the words pass");
    assert!(matches!(m.need(), Need::Later(_)));
    assert_eq!(m.title, "Claude in tab 1 is still on 2.1.283");
    assert_eq!(
        m.detail[0],
        "aterm never ends running work, and a job it started 5 h ago (diskwatch.sh) still runs"
    );
    assert_eq!(m.detail[1], "End that job in the tab");
    assert!(m.detail[2].starts_with("held by pid 36855"));
    assert_eq!(m.actions, vec![Intent::ShowTab { tab: 1, window: 1 }]);
    assert_eq!(m.severity, Severity::Info);
}

#[test]
fn the_full_disk_access_entry_leads_with_the_button_that_does_it() {
    let e = full_disk_access();
    assert_eq!(e.key(), "privacy.fda.needed");
    let m = e.message().expect("the words pass");
    assert_eq!(
        m.actions,
        vec![Intent::OpenSystemPane {
            pane: "full-disk-access".into()
        }]
    );
    assert_eq!(m.detail[1], "turn on aterm in Full Disk Access");
}

/// Every way to break the rule is refused where the entry is written (the
/// reason/action word rule itself is the door's:
/// `need_tests::a_snooze_a_pair_of_actions_or_a_wait_is_no_need`).
#[test]
fn every_fault_is_refused_where_the_entry_is_written() {
    let base = |what: &str, why: &str, then: &str| {
        Entry::new(
            Family::Upgrade,
            "",
            "x",
            what,
            Why::OnlyYou(Reason::new(why)),
            Then::step(then),
        )
    };
    let ok = (
        "Claude is behind",
        "aterm never ends running work",
        "Stop that work",
    );
    assert!(base(ok.0, ok.1, ok.2).message().is_ok());
    // a title that is a sentence, a clause or too long
    assert!(matches!(
        base("Claude is behind.", ok.1, ok.2).message(),
        Err(EntryFault::What(_))
    ));
    assert!(matches!(
        base("Claude \u{2014} behind", ok.1, ok.2).message(),
        Err(EntryFault::What(_))
    ));
    let long = "a ".repeat(WHAT_CAP);
    assert!(matches!(
        base(&long, ok.1, ok.2).message(),
        Err(EntryFault::What(_))
    ));
    // a decline beside the action
    for decline in [
        Intent::AgentUpgrade {
            tab: "s-1".into(),
            to: "2".into(),
            word: UpgradeWord::NotToday,
        },
        Intent::AgentUpgrade {
            tab: "s-1".into(),
            to: "2".into(),
            word: UpgradeWord::Skip,
        },
        Intent::Details,
    ] {
        assert_eq!(
            base(ok.0, ok.1, ok.2).show(decline).message().unwrap_err(),
            EntryFault::Decline
        );
    }
}
