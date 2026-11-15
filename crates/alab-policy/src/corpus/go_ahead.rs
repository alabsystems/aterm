// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE GO-AHEAD'S CORPUS: `fixtures/go-ahead-table.txt`'s rows, read from the text the test
//! includes, the widths a tab draws them at, the incident's turn, every review's repros and the
//! agent's echoes, which the go-ahead reader's tests and the engines' tests that hold a whole turn
//! end to the same rows read alike.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Park,
    Person,
    Plain,
}

/// The rows of `text` (`go-ahead-table.txt`'s), in order: each one's section and its reply.
pub fn table(text: &str) -> Vec<(Row, String)> {
    let mut rows = Vec::new();
    let mut section = None;
    for line in text.lines() {
        match line {
            "== park" => section = Some(Row::Park),
            "== person" => section = Some(Row::Person),
            "== plain" => section = Some(Row::Plain),
            _ if line.starts_with('#') || line.trim().is_empty() => {}
            _ => rows.push((
                section.unwrap_or_else(|| panic!("a row before its section: {line:?}")),
                line.replace("\\n", "\n"),
            )),
        }
    }
    rows
}

/// `reply` as a screen of `columns` draws it: each line wrapped at a word behind a 2-column
/// gutter, so a sentence straddles rows.
pub fn wrapped(reply: &str, columns: usize) -> String {
    let width = columns - 2;
    let mut rows = Vec::new();
    for line in reply.lines() {
        let mut row = String::new();
        for word in line.split(' ') {
            if !row.is_empty() && row.chars().count() + 1 + word.chars().count() > width {
                rows.push(format!("  {row}"));
                row.clear();
            }
            if !row.is_empty() {
                row.push(' ');
            }
            row.push_str(word);
        }
        rows.push(if row.is_empty() {
            String::new()
        } else {
            format!("  {row}")
        });
    }
    rows.join("\n")
}

/// The widths a table row is read at: as written, and wrapped at 80, 100, 101 and 120 columns.
pub const DRAWN: [Option<usize>; 5] = [None, Some(80), Some(100), Some(101), Some(120)];

/// `reply` as drawn at `columns` ([`DRAWN`]).
pub fn drawn(reply: &str, columns: Option<usize>) -> String {
    columns.map_or_else(|| reply.to_string(), |c| wrapped(reply, c))
}

/// `reply` as the agent's screen renders it: inline code and bold drawn bare, with no backticks and no
/// `**`, so a code span's words read as prose.
pub fn rendered(reply: &str) -> String {
    reply.replace('`', "").replace("**", "")
}

/// The incident's turn of 2026-10-26T23:17Z..23:25Z (transcript `65ef503b…`): its earlier
/// messages, which name code (`plan()`, `tap_of`, `for_launch`), and its last reply.
pub const INCIDENT_TURN: (&[&str], &str) = (
    &[
        "`note_wait` also restarts the clock on a person's or a limit's wait word, and the watch \
         calls it, so the doc sentence I wrote overstates. Correcting the doc and the commit \
         message:",
        "Reflowing that paragraph, and making line 240 match the new Cruise guard rule:",
        "Since the tell now runs `plan()` on each visit where a tell is due, checking that `plan` \
         writes nothing (e.g. through `tap_of` or `for_launch`):",
        "`plan` only reads. Reviewing this round's drive diff while the window test finishes:",
        "The Tell arm repeats the Announce arm's plan construction (~30 lines). Checking whether \
         `mode` and `model` are used elsewhere in the Announce arm, so one shared closure can \
         serve both:",
        "Factoring the plan into one closure that both arms call:",
        "Main's change touches `crates/aterm-cli/src/ship/release.rs`, which my branch doesn't.",
        "That was my own edit. Now queuing the agent crate's tests behind the window build (the \
         shared cache's lock runs them one after the other, not side by side):",
    ],
    "Two builds are queued: the window's tell test, then the agent crate's tell and notice tests \
     after the shared-plan refactor. I'll pick up when they report.\n\nWhere this round stands \
     (not committed or pushed yet):\n- **Fixed, review finding 6:** the \"Upgrade waiting\" line \
     is no longer typed for a tab aterm couldn't actually relaunch, such as one under a dash \
     shell or a HOME the relaunch line can't carry. It now runs the move's own relaunch plan \
     first.\n- **Fixed, review finding 5:** the line names `aterm ctl @self restart` only when \
     the new build is proved to start, which is what that restart itself requires.\n- \
     **Changed, Cruise:** a manager's wait or the person's own exchange no longer holds the \
     line, matching main's Cruise continuation. Only a person actually at the tab holds it. \
     Land and Off keep every guard.\n- **Text:** the help, changelog, design doc and SPECS no \
     longer overclaim: \"at a turn end\" rather than \"first\", the `/goal` exception, and the \
     count is per agent process.\n- **Tests:** the agent crate's tell and quiet tests pass \
     (194) with the two new drive cases. Each new gate, removed on its own, makes its case \
     fail.\n\nNEEDS YOU: say \"rerun the checks\" when the Mac has memory to spare. The full \
     verify on the merged branch was stopped for low memory, and I'm not restarting it on my \
     own.",
);

/// Every review's repros (2026-10-29, rounds 1 to 5): the replies each review found misread,
/// in the base guard's corpus ((`go_ahead::tests::no_reading_is_weaker_than_the_base_guard`)).
pub const REPROS: &[&str] = &[
    "Nothing has changed. The build is waiting only for your go-ahead.",
    "I'll keep waiting until I have your go-ahead to delete the old release tags.",
    "I won't delete the stale release tags until I have your go-ahead.",
    "I've built the installer and with your go-ahead I'll upload it to the App Store.",
    "The [aterm harness] go-ahead covers my branch only, so deleting the old release tags still \
     needs your go-ahead.",
    "As I said, \"deleting the old release tags needs your go-ahead.\"",
    "Tagged v1.4.0 for the crates.io release.\nNEEDS YOU: say go so I can push.",
    "The cleanup job deletes the stale release tags on the remote once you say go.\n\nNEEDS YOU: \
     say go so I can rebuild and push my branch to main.",
    "CI uploads every green build of main to TestFlight. The suite is green. Waiting for your \
     go-ahead to push my branch to main.",
    "The branch is ready for your review. Once you say go, I'll push it to main.",
    "Once you've had a look, just say go and I'll push my branch to main.",
    "After you take a look, say go and I'll push it to main.",
    "I'll merge the Dependabot PR into main once you say so.",
    "I'll merge the three open PRs into main once you say so.",
    "I'll merge PR #412 into main once you say so.",
    "Canary is healthy; I'll continue the rollout to the remaining hosts once you give the \
     go-ahead.",
    "I'll resume the backfill on the orders table once you give the go-ahead.",
    "NEEDS YOU: say go so I can rerun the checks.\nNEEDS YOU: say go so I can delete the stale \
     release tags on the remote.",
    "Say \"go\" so I can rerun the checks. Say \"go\" so I can delete the stale release tags on \
     the remote.",
    "Give me the go-ahead and I'll force-push the branch.",
    "With your go-ahead I'll delete the old releases.",
    "I'll wait for your go-ahead before force-pushing.",
    "I need your go-ahead to delete the stale release tags; the standing go-ahead covers only my \
     own reversible work.",
    "I have the go-ahead for my branch, but deleting the old release tags still needs your \
     go-ahead.",
    "Status:\n│ delete old release tags │ waiting for your go-ahead │",
    "I won't delete the old release tags without your approval.",
    "The release is built. I won't publish it without your approval.",
    "I won't deploy without your go-ahead.",
    "I can't force-push the branch without your go-ahead.",
    "I'll deploy without waiting for you.",
    "Rebuilt and tested. I need your approval to push:\n\n```\ngit push --force origin main\n```",
    "I won't merge without your sign-off.",
    "The old reply said:\n```\nI'm blocked on your review.\n```",
    "I won't push my branch without your go-ahead. Pushing it also deletes the stale release tags \
     on the remote.",
    "Give me the go-ahead and I'll rotate the production API keys.",
    "On your go-ahead I'll email the customers.",
    "With your go-ahead I'll open the tracking issue.",
    "I'll push the merged branch with your go-ahead.",
    "No force-push and nothing published. I'm still waiting for your go-ahead before rebuilding.",
    // Review 5: a `NEEDS YOU items …` row the base cut as a need under a choice and a question.
    "Two ways to clear the stale tags:\n1. Delete the stale release tags and re-tag.\n2. Leave \
     them in place.\nWhich one?\nNEEDS YOU items stay on the menu bar until you answer.",
    "Should I rebase onto main first?\nNEEDS YOU items stay on the menu bar until you answer.",
];

/// What an agent writes once it has the go-ahead, or of it: echoes, reports and paraphrases.
pub const ECHOES: &[&str] = &[
    "Rebuilt and pushed with your go-ahead; the suite is green and the worktree is gone.",
    "Rebuilt and pushed to main with your go-ahead.",
    "I no longer need your go-ahead to rebuild, so I'm rebuilding now.",
    "Since the go-ahead stands I'm not waiting; rebuilding the branch now.",
    "Since the go-ahead stands I'm not waiting: I merged main, built and tested the branch, pushed \
     it to main and removed the worktree.",
    "The harness gave the owner's standing go-ahead, so I'm rerunning the checks.",
    "I rebuilt without waiting for your go-ahead, per the ruling.",
    "Got the go-ahead, so I deleted the stale worktree I made and rebuilt.",
    "The rebuild that was waiting for your go-ahead is done: built, tested and pushed to main.",
    "Proceeding with my own reversible work under the owner's standing go-ahead: rerunning the \
     checks.",
    "This adds a go-ahead reader; its rows are in the table.",
    "This fix adds a go-ahead reader; its table holds rows such as:\n```\nI won't restart it \
     unless you ask.\nThe build is waiting for your go-ahead.\n```\nEach of them parks.",
];

/// THE GUARD'S CORPUS: every row of the go-ahead table (`table_text`, `go-ahead-table.txt`'s
/// text) at every width a tab draws it ([`DRAWN`]), every review's repro and every echo.
/// `fixtures/go-ahead-guard-base.txt` holds the base's guard for each.
pub fn guard_corpus(table_text: &str) -> Vec<String> {
    let mut texts: Vec<String> = Vec::new();
    for (_, reply) in table(table_text) {
        texts.extend(DRAWN.iter().map(|&c| drawn(&reply, c)));
    }
    texts.extend(REPROS.iter().chain(ECHOES).map(|t| (*t).to_string()));
    texts
}
