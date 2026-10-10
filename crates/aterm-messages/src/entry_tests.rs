// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the host-facing entry ([`crate::exceptions`]) — the words of the
//! two cases the owner named, every fault refused where the entry is written,
//! and one family pass's keys never touching another family's.

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

/// Tab 1, after a day: one entry per KIND (no subject), the words the owner
/// reads, the navigation beside the sentence, the holders behind `Details`.
fn tab_one() -> Entry {
    Entry::new(
        Family::Upgrade,
        "",
        "own-work",
        "Claude in tab 1 is still on 2.1.283",
        // The holder that outlives a turn is named (tab 1's `diskwatch.sh`, 5 h 47 m
        // when read; the other nine children were Bash-tool wrappers under four
        // minutes old), and the action is the one that ends it.
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
        // Stand-ins for aterm-gui's `consent_card::FDA_WHY` / `FDA_THEN` (that
        // crate depends on this one); the host imports them, it never retypes
        // them. Nothing here promises what the switch will do.
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

/// Every way to break the rule is refused where the entry is WRITTEN.
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

/// EVERY UPGRADE KIND, through the vocabulary: the upgrade stream's `Need` table
/// (`plan-upgrade.md` 3.5) with each `what` composed as the host composes it
/// (a place and, for one tab, the version), the resume stream's first kind, and
/// the words of the two cases the owner named. A word that leans on "waiting",
/// a sentence that offers two actions, a title past six words or 48 characters
/// is a red test HERE, not a long line in the menu bar.
#[test]
fn every_upgrade_kind_passes_the_rule() {
    use crate::text::title_words;
    let kinds: [(Family, &str, &str, &str, &str, &str); 16] = [
        // own-work: the WHY names what actually holds the move (a count of
        // short-lived tool children gives the owner nothing to stop). A job
        // that outlives a turn is named and ended; an agent that never pauses
        // is quit and resumed.
        (
            Family::Upgrade,
            "",
            "own-work",
            "Claude in tab 1 is still on 2.1.283",
            "aterm never ends running work, and a job it started 5 h ago (diskwatch.sh) still runs",
            "End that job in the tab",
        ),
        (
            Family::Upgrade,
            "",
            "own-work",
            "Claude in tab 1 is still on 2.1.283",
            "aterm never ends running work, and Claude has not been idle for over a day",
            "Quit Claude in the tab and run claude --resume",
        ),
        (
            Family::Upgrade,
            "",
            "own-work",
            "Claude in 3 tabs is behind",
            "aterm never ends running work, and Claude has not been idle for over a day",
            "Quit Claude in each tab and run claude --resume",
        ),
        (
            Family::Upgrade,
            "",
            "typed-draft",
            "A draft holds Claude in tab 3",
            "a message you typed sits unsent in its prompt, and aterm will not upgrade over it",
            "Send the draft in that tab",
        ),
        (
            Family::Upgrade,
            "s-4",
            "suspended",
            "Claude in tab 4 is suspended",
            "Claude Code is suspended or in the background, so it cannot be upgraded",
            "Run fg in that tab",
        ),
        (
            Family::Upgrade,
            "s-2",
            "no-terminal",
            "Claude in tab 2 is unreachable",
            "Claude Code runs where the upgrade cannot type to it (tmux)",
            "Quit it there and resume it in an aterm tab",
        ),
        (
            Family::Upgrade,
            "s-2",
            "refused-launch",
            "Claude in tab 2 cannot be resumed",
            "Claude Code was started through a wrapper, not directly by its shell",
            "Start it directly at a shell prompt in that tab",
        ),
        (
            Family::Upgrade,
            "s-2",
            "repeating-stop",
            "The upgrade keeps stopping in tab 2",
            "upgrading Claude Code has stopped the same way 3 times in a row",
            "Quit it and resume it in that tab",
        ),
        (
            Family::Upgrade,
            "",
            "daemon-busy",
            "A Codex thread holds the daemon",
            "a thread in its Codex daemon has run for 1d6h, which keeps it where it is",
            "Stop that thread (codex agents lists them)",
        ),
        (
            Family::Upgrade,
            "s-2",
            "other-client",
            "Another Codex holds Codex in tab 2",
            "a Codex outside this window is attached to the shared Codex daemon, which keeps it",
            "Quit that other Codex",
        ),
        (
            Family::Upgrade,
            "s-2",
            "no-integration",
            "Codex in tab 2 lacks shell integration",
            "Codex in this tab cannot be exited safely: its shell sends no prompt marks",
            "Resume Codex in a new tab",
        ),
        (
            Family::Resume,
            "",
            "spend",
            "Claude in tab 1 has no credits",
            "your seat does not include usage credits",
            "Ask your Claude admin for a seat with usage credits",
        ),
        // a stall no reader names (the resume stream's canary fired and nothing
        // changed for three hours)
        (
            Family::Resume,
            "s-1",
            "stalled",
            "Claude in tab 1 has stalled",
            "nothing this build knows explains it, and a goal is set",
            "Open the tab and type keep going",
        ),
        // a fact the upgrade's HOST raises by event: its own family, so the rows'
        // level pass can never sweep it
        (
            Family::UpgradeHost,
            "",
            "cannot-run",
            "The upgrade cannot run",
            "the upgrade cannot use its state folder",
            "Fix the folder's permissions",
        ),
        (
            Family::Update,
            "dev-build",
            "writes",
            "Release setup lost to dev build",
            "this dev build overwrote the PATH links, the shell hooks and the agent primer",
            "Run `aterm pkg repair` from the release",
        ),
        // the privacy stream's words (stand-ins for `consent_card`'s)
        (
            Family::Privacy,
            "fda",
            "needed",
            "macOS keeps asking for access",
            "macOS forgets an Allow when aterm updates",
            "Turn on aterm in Full Disk Access",
        ),
    ];
    for (family, subject, cause, what, why, then) in kinds {
        let entry = Entry::new(
            family,
            subject,
            cause,
            what,
            Why::OnlyYou(Reason::new(why)),
            Then::step(then),
        );
        let msg = entry
            .message()
            .unwrap_or_else(|fault| panic!("{cause}: {fault:?}"));
        assert!(
            title_words(what) <= 6 && what.chars().count() <= WHAT_CAP,
            "{what}"
        );
        assert!(
            why.chars().count() <= REASON_CAP && then.chars().count() <= STEP_CAP,
            "{cause}"
        );
        assert_eq!(msg.detail[0], why);
        assert_eq!(msg.detail[1], then);
        assert!(entry.key().starts_with(&family.prefix()), "{}", entry.key());
        // no restart-family word in any part
        // (spelled from parts: the guard's B12 reads this file, and the words
        // are the ones no reporter may say)
        for needle in [
            ["re", "start"].concat(),
            ["re", "launch"].concat(),
            ["re", "open"].concat(),
            ["next ", "launch"].concat(),
            ["re", "boot"].concat(),
        ] {
            assert!(
                ![what, why, then]
                    .iter()
                    .any(|t| t.to_lowercase().contains(&needle)),
                "{cause}: {needle}"
            );
        }
    }
}
