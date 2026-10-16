// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the host-facing entry ([`crate::exceptions`]): the upgrade and Full Disk
//! Access entries' words, every fault refused where the entry is written, and one family
//! pass's keys never touching another family's.

use crate::center::MessageCenter;
use crate::exceptions::{Entry, EntryFault, Family, WHAT_CAP};
use crate::log::MessageLog;
use crate::model::{Intent, Severity, UpgradeWord, WallStamp};
use crate::need::{Mover, Need, REASON_CAP, Reason, Resurface, STEP_CAP, Then, Why};
use crate::{DETAIL_LINE_CAP, Instant};

fn stamp() -> WallStamp {
    WallStamp {
        unix_ms: 1_790_000_000_000,
    }
}

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

/// Every way to break the rule is refused where the entry is written.
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
    // a reason that says something is pending, or nothing
    assert!(matches!(
        base(ok.0, "it is waiting for its work", ok.2).message(),
        Err(EntryFault::Ask(_))
    ));
    assert!(matches!(
        base(ok.0, "", ok.2).message(),
        Err(EntryFault::Ask(_))
    ));
    // an action that is two actions, a snooze, or nothing
    assert!(matches!(
        base(ok.0, ok.1, "Stop that work or skip the build").message(),
        Err(EntryFault::Ask(_))
    ));
    assert!(matches!(
        base(ok.0, ok.1, "Skip this build").message(),
        Err(EntryFault::Ask(_))
    ));
    assert!(matches!(
        base(ok.0, ok.1, "").message(),
        Err(EntryFault::Ask(_))
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

/// A family pass sweeps `family.prefix()`; no family's prefix may be the start
/// of another's, or one reporter could end another's entries.
#[test]
fn no_family_prefix_starts_another() {
    let all = [
        Family::Privacy,
        Family::Update,
        Family::Upgrade,
        Family::Resume,
        Family::Supervisor,
        Family::Toolchain,
        Family::Disk,
        Family::Render,
    ];
    for a in all {
        for b in all {
            if a != b {
                assert!(!a.prefix().starts_with(&b.prefix()), "{a:?} vs {b:?}");
            }
        }
    }
    assert!(tab_one().key().starts_with(&Family::Upgrade.prefix()));
    assert!(
        full_disk_access()
            .key()
            .starts_with(&Family::Privacy.prefix())
    );
}

/// The one-line form (`what \u{2014} why \u{2192} then`) of the longest entry the
/// caps allow fits a detail line, so the menu bar, the palette and the log
/// never clip it.
#[test]
fn the_longest_entry_fits_one_detail_line() {
    let line = WHAT_CAP
        + " \u{2014} ".chars().count()
        + REASON_CAP
        + " \u{2192} ".chars().count()
        + STEP_CAP;
    assert!(line <= DETAIL_LINE_CAP, "{line}");
}

/// End to end through the engine: a family pass posts the entry once, the
/// digest reads it, a dismissal files it, and the next pass is quiet.
#[test]
fn an_entry_goes_through_the_family_pass_and_a_dismissal_holds() {
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let prefix = Family::Upgrade.prefix();
    let s = c.sync_digest(&prefix, vec![tab_one().message().unwrap()], stamp(), now);
    assert_eq!(s.posted, 1);
    let view = c.digest();
    assert_eq!(view.entries[0].key, "harness.upgrade.own-work");
    assert_eq!(view.entries[0].then, "End that job in the tab");
    assert_eq!(c.wanted_rows(), 0, "an entry never asks for a band row");
    assert_eq!(c.file_digest(stamp(), now), 1);
    let s = c.sync_digest(&prefix, vec![tab_one().message().unwrap()], stamp(), now);
    assert_eq!((s.posted, s.suppressed), (0, 1));
}
