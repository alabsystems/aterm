// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EVERY MODE'S SENTENCE, ONE TABLE (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.4.1).
//!
//! The window says each Autopilot's sentence on the chip's title, in the records and on
//! Settings ▸ Harness's mode buttons; the forever lab (`aterm-lab`) judges the chip's title
//! against the same rows. The table lives beside [`Autopilot`] so both read the one source.

use super::Autopilot;
use crate::harness::acts::{FACT_SEPARATOR, LandReceipt, ReceiptLeg};
use crate::harness::hold_said::NeedTail;

/// One mode's sentence in the form each surface renders it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeWords {
    /// The mode the sentence says.
    pub mode: Autopilot,
    /// The Autopilot chip's title, after `autopilot: <mode> — `.
    pub chip: &'static str,
    /// The `Autopilot set to <mode>` record's line, after its tab.
    pub record: &'static str,
    /// Settings ▸ Harness's mode button, as its description says what a press does.
    pub button: &'static str,
}

/// EVERY MODE'S SENTENCE, ONE TABLE (§12.4.1), in [`Autopilot::ALL`]'s order.
pub const MODE_WORDS: [ModeWords; 3] = [
    ModeWords {
        mode: Autopilot::Cruise,
        chip: "keeps going for ever, finding more to do",
        record: "the harness keeps going for ever, finding more to do",
        button: "keep going for ever, finding more to do: nothing the agent says ends it",
    },
    ModeWords {
        mode: Autopilot::Land,
        chip: "wrapping up: landing everything on the remote main branch, then stopping",
        record: "the harness wraps it up: it lands everything on the remote main branch, then \
                 stops",
        button: "wrap it up: land everything on the remote main branch, then stop",
    },
    ModeWords {
        mode: Autopilot::Off,
        chip: "hands off",
        record: "the harness keeps its hands off",
        button: "the harness keeps its hands off this session",
    },
];

/// A TIMED CRUISE'S SENTENCES (§12.4.1's third row): Cruise until a time, then Land.
pub const TIMED_CRUISE_CHIP: &str = "keeps going until then, finding more to do";
/// The record of a timed Cruise set.
pub const TIMED_CRUISE_RECORD: &str =
    "the harness keeps going until that time, then wraps it up on the remote main branch";

/// WHAT A LANDING LEAVES (§14.9): the last words of the chip's `✓ landed` title and its
/// description, after what was checked ([`landed_brief`], [`landed_words`]).
pub const LANDED_REST: &str = "the harness types nothing more until someone else's turn";

/// A LANDING WITH NO RECEIPT (§14.9, slice 9c): aterm holds no record of a check there. That is
/// codex-alab's own landing (a proven self-claim's `landed@v1`, whose check is its own: aterm
/// checked nothing), a task its worker marked done itself, and an aterm landing that a loop
/// before the receipt (slice 9b) noted and a later loop restates (aterm checked it and kept no
/// receipt). The words hold on each, so no surface has to know whose landing it was.
pub const NO_RECORD: &str = "aterm has no record of checking this landing";

/// THE LEG WITH NO REPOSITORY (§14.9, slice 9c; §14.17 item 4): the probe read nothing to name
/// (`ReceiptLeg::NoRepo`: a plain folder, no commit yet, a folder macOS guards, a network
/// volume, no `git`). The words hold on each of those paths: a repository aterm does not read is
/// one of them, so they never say its files land where they are.
pub const NO_REPO_CHECKED: &str =
    "no git repository checked (a plain folder, or one aterm does not read)";

/// How the words of every leg that read a head begin ([`checked_words`]): `aterm checked `.
pub const CHECKED_LEAD: &str = "aterm checked ";

/// How the words of a folder that is gone begin, before the folder and `)` ([`checked_words`]).
pub const GONE_LEAD: &str = "the folder is gone (";

/// WHAT ATERM CHECKED AT A LANDING, LEG BY LEG (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §14.9, slice 9c; §14.17 item 4): THE ONE SOURCE every surface reads (the Messages record, the
/// menu bar's tips, the chip's landed title and its screen-reader description, Settings ▸
/// Harness's state line). A head is the receipt's 12 digits; "origin/main" is the ref git named,
/// never a remote main git did not name. The folder is the receipt's, as the probe named it
/// (absolute: the words read the same in every process, whatever its `HOME`).
///
/// * on `origin/main`: `aterm checked 3f9a1c2e4b7d on origin/main in /Users//me/src/proj; nothing
///   uncommitted, untracked or unpushed, and no stash on main` (`on its detached HEAD` where
///   `HEAD` names no branch, `on HEAD` on a receipt journaled before the branch was read);
/// * pushed, with no origin main: `aterm checked 3f9a1c2e4b7d pushed; no origin main to check
///   against`;
/// * no remote: `aterm checked 3f9a1c2e4b7d committed; this repository has no remote`;
/// * no repository: [`NO_REPO_CHECKED`];
/// * a folder gone: `the folder is gone (/x/proj)`;
/// * no receipt: [`NO_RECORD`].
#[must_use]
pub fn checked_words(receipt: Option<&LandReceipt>) -> String {
    let Some(r) = receipt else {
        return NO_RECORD.to_owned();
    };
    match r.leg {
        ReceiptLeg::OnMain => {
            let on = match r.branch.as_str() {
                crate::harness::repo_work::DETACHED => "its detached HEAD",
                "" => "HEAD",
                branch => branch,
            };
            format!(
                "{CHECKED_LEAD}{} on {} in {}; nothing uncommitted, untracked or unpushed, and no \
                 stash on {on}",
                r.head, r.main_ref, r.repo
            )
        }
        ReceiptLeg::Pushed => format!(
            "{CHECKED_LEAD}{} pushed; no origin main to check against",
            r.head
        ),
        ReceiptLeg::NoRemote => format!(
            "{CHECKED_LEAD}{} committed; this repository has no remote",
            r.head
        ),
        ReceiptLeg::NoRepo => NO_REPO_CHECKED.to_owned(),
        ReceiptLeg::Gone => format!("{GONE_LEAD}{})", r.repo),
    }
}

/// WHAT ATERM CHECKED, IN BRIEF (§14.9's chip title, slice 9c): on `origin/main` the head, the
/// ref and the folder alone (`aterm checked 3f9a1c2e4b7d on origin/main in /Users//me/src/proj`),
/// every other leg's [`checked_words`] whole. The chip's hovered title says it ([`landed_brief`]),
/// so the rule's room keeps it; the record, the tips, the state line and the screen-reader
/// description say the whole.
#[must_use]
pub fn checked_brief(receipt: Option<&LandReceipt>) -> String {
    match receipt {
        Some(r) if r.leg == ReceiptLeg::OnMain => {
            format!("{CHECKED_LEAD}{} on {} in {}", r.head, r.main_ref, r.repo)
        }
        other => checked_words(other),
    }
}

/// WHAT THE AGENT ANSWERED AT A LANDING THAT STOOD ON ATERM'S LAND CHECK (`land-check@v1`,
/// §14.17 item 5, slice 13a): its `LANDED` answer, in words ([`said_words`] where the receipt
/// records the check, `LandReceipt::check`).
pub const ANSWERED_CHECK: &str = "it answered the land check that nothing is left";

/// What the agent said at a landing, apart from what aterm checked: `it said “Nothing left: …”`
/// (the receipt's own words, already one title-safe line), or `it said nothing is left` where
/// the receipt kept none; where the landing stood on the land check's `LANDED` (slice 13a),
/// [`ANSWERED_CHECK`] first, then `after it said “…”` (the defense the check audited), so no
/// surface claims a check the receipt does not record ([`said_within`] with no bound).
#[must_use]
pub fn said_words(receipt: &LandReceipt) -> String {
    said_within(receipt, usize::MAX)
}

/// [`said_words`] in at most `chars` characters: a quote too long for them is cut, ending in one
/// `…` before its closing mark, so a line capped at `chars` (the Messages record's) never loses
/// the mark or doubles the `…` of a receipt already cut.
#[must_use]
pub fn said_within(receipt: &LandReceipt, chars: usize) -> String {
    const END: char = '\u{201d}';
    let lead = if receipt.check {
        format!("{ANSWERED_CHECK}, after it said \u{201c}")
    } else {
        "it said \u{201c}".to_owned()
    };
    if receipt.said.is_empty() {
        return if receipt.check {
            ANSWERED_CHECK.to_owned()
        } else {
            "it said nothing is left".to_owned()
        };
    }
    let frame = lead.chars().count() + 1;
    if frame.saturating_add(receipt.said.chars().count()) <= chars {
        return format!("{lead}{}{END}", receipt.said);
    }
    let kept: String = receipt
        .said
        .chars()
        .take(chars.saturating_sub(frame + 1))
        .collect();
    let kept = kept.trim_end().trim_end_matches('\u{2026}').trim_end();
    format!("{lead}{kept}\u{2026}{END}")
}

/// A LANDING'S SENTENCE (§14.9: 9a's fixed words, then 9c's receipt): what the chip's `✓ landed`
/// screen-reader description says: what aterm checked ([`checked_words`]), what the agent said,
/// and [`LANDED_REST`]; with no receipt, that it said nothing is left and [`NO_RECORD`]. The
/// hovered title says [`landed_brief`].
#[must_use]
pub fn landed_words(receipt: Option<&LandReceipt>) -> String {
    match receipt {
        Some(r) => format!(
            "{}; {}; {LANDED_REST}",
            checked_words(Some(r)),
            said_words(r)
        ),
        None => format!("it said nothing is left; {NO_RECORD}; {LANDED_REST}"),
    }
}

/// What the chip's `✓ landed` title says after its head: what aterm checked in brief
/// ([`checked_brief`]) and [`LANDED_REST`], and not the agent's words, which run to
/// `SAID_BYTES` and would leave the rule beside an agent's footer no room for what aterm checked
/// (the review round of slice 9c): the state line, both tips, the record and the description
/// quote them.
#[must_use]
pub fn landed_brief(receipt: Option<&LandReceipt>) -> String {
    format!("{}; {LANDED_REST}", checked_brief(receipt))
}

/// A LANDED ROW'S TIP (§14.9, slice 9c): the menu bar's Done for you row says what aterm checked,
/// then what the agent said (`aterm checked … · it said “…”`); with no receipt, [`NO_RECORD`].
#[must_use]
pub fn landed_tip(receipt: Option<&LandReceipt>) -> String {
    match receipt {
        Some(r) => format!(
            "{}{FACT_SEPARATOR}{}",
            checked_words(Some(r)),
            said_words(r)
        ),
        None => NO_RECORD.to_owned(),
    }
}

/// A `NEEDS YOU:` REPORT'S WORDS (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.2.1, §14.7):
/// what the tab's hover ends with and what the menu bar's report row says on hover, after the
/// program's own name (`Program::product`: `Claude Code`, `Antigravity CLI`, `Muse Code`). The
/// agent goes on past the report, unlike a box or a question that stops it.
pub const CARRIES_ON: &str = "carries on meanwhile";

/// …the menu bar's report row where that need is all that holds its Land session (the hold's one
/// leg is its needs, §14.7), after the program's own name and `'s`.
pub const LANDING_WAITS_ALONE: &str = "landing waits on this alone";

/// A REPORT'S WORDS, ONE FUNCTION (§14.7): `<product> carries on meanwhile`; where the need alone
/// holds its session's landing (`alone`), `<product>'s landing waits on this alone`; and where the
/// need's entry says the work waits on the person (`tail`, [`NeedTail::WaitsOnYou`]: the loop
/// stopped typing, or Land rests on the need or landed beside it), `<product> waits on you`, as
/// the entry and the chip say it (the post-landing review, 2026-11-02). The window's hover and the
/// menu bar's tip say them, and the forever lab judges the menu's tip against them.
#[must_use]
pub fn report_words(product: &str, tail: NeedTail, alone: bool) -> String {
    match (alone, tail) {
        (true, _) => format!("{product}'s {LANDING_WAITS_ALONE}"),
        (false, NeedTail::WaitsOnYou) => format!("{product} {}", tail.words()),
        (false, NeedTail::CarryingOn) => format!("{product} {CARRIES_ON}"),
    }
}

/// `mode`'s sentences from [`MODE_WORDS`].
#[must_use]
pub const fn mode_words(mode: Autopilot) -> &'static ModeWords {
    match mode {
        Autopilot::Cruise => &MODE_WORDS[0],
        Autopilot::Land => &MODE_WORDS[1],
        Autopilot::Off => &MODE_WORDS[2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each row says its own mode, in [`Autopilot::ALL`]'s order, and [`mode_words`] finds it.
    #[test]
    fn each_mode_has_its_own_row_in_order() {
        for (i, mode) in Autopilot::ALL.into_iter().enumerate() {
            assert_eq!(MODE_WORDS[i].mode, mode, "row {i}");
            assert_eq!(mode_words(mode).mode, mode, "{}", mode.word());
        }
    }

    /// A receipt of `leg` over `/Users//me/src/proj`, its head `3f9a1c2e4b7d`, on `origin/main`
    /// and `main` where the leg is on main.
    fn receipt(leg: ReceiptLeg, said: &str) -> LandReceipt {
        let named = !matches!(leg, ReceiptLeg::NoRepo | ReceiptLeg::Gone);
        let on_main = leg == ReceiptLeg::OnMain;
        let pick = |yes: bool, s: &str| if yes { s.to_owned() } else { String::new() };
        LandReceipt {
            head: pick(named, "3f9a1c2e4b7d"),
            main_ref: pick(on_main, "origin/main"),
            repo: pick(leg != ReceiptLeg::NoRepo, "/Users//me/src/proj"),
            branch: pick(on_main, "main"),
            leg,
            check: false,
            said: said.to_owned(),
            at_ms: 0,
            statement: crate::harness::land_receipt::Statement::Unread,
        }
    }

    /// EACH LEG'S WORDS COME FROM ONE SOURCE AND SAY WHAT GIT SAID (§14.9, slice 9c; §14.17 item
    /// 4): every leg says its own words, each different; the head where one was read, the ref and
    /// the branch only on main, the folder where it is gone; "remote main" on none (git names
    /// `origin/main`). The description, the tip and the record's words each lead with them and
    /// carry the agent's words; the title's brief drops the clause on main alone and the agent's
    /// words on every leg. A landing with no receipt says aterm has no record of checking it, and
    /// names no head. Every leg's words begin with [`CHECKED_LEAD`], [`NO_REPO_CHECKED`] or
    /// [`GONE_LEAD`], the leads the lab reads. NEGATIVE CONTROLS (run by hand): the pushed leg's
    /// words saying "remote main", a 9b receipt's branch read as detached, a brief keeping the
    /// clause, and a brief quoting the agent each fail it.
    #[test]
    fn each_legs_words_come_from_one_source_and_say_what_git_said() {
        let said = "Nothing left: pushed to origin/main.";
        let want = [
            (
                ReceiptLeg::OnMain,
                "aterm checked 3f9a1c2e4b7d on origin/main in /Users//me/src/proj; nothing \
                 uncommitted, untracked or unpushed, and no stash on main",
            ),
            (
                ReceiptLeg::Pushed,
                "aterm checked 3f9a1c2e4b7d pushed; no origin main to check against",
            ),
            (
                ReceiptLeg::NoRemote,
                "aterm checked 3f9a1c2e4b7d committed; this repository has no remote",
            ),
            (ReceiptLeg::NoRepo, NO_REPO_CHECKED),
            (ReceiptLeg::Gone, "the folder is gone (/Users//me/src/proj)"),
        ];
        assert_eq!(
            want.map(|(leg, _)| leg),
            ReceiptLeg::ALL,
            "every leg, in order"
        );
        let mut seen = std::collections::BTreeSet::new();
        for (leg, words) in want {
            let r = receipt(leg, said);
            let checked = checked_words(Some(&r));
            assert_eq!(checked, words, "{leg:?}");
            assert!(
                seen.insert(checked.clone()),
                "{leg:?} says another leg's words"
            );
            for surface in [
                checked.clone(),
                landed_words(Some(&r)),
                landed_tip(Some(&r)),
            ] {
                assert!(!surface.contains("remote main"), "{leg:?}: {surface}");
            }
            let title = landed_words(Some(&r));
            assert_eq!(
                title,
                format!("{checked}; it said \u{201c}{said}\u{201d}; {LANDED_REST}"),
                "{leg:?}"
            );
            assert!(
                !title.contains(FACT_SEPARATOR) && !title.contains(" \u{2014} "),
                "the title splits on neither: {title}"
            );
            assert_eq!(
                landed_tip(Some(&r)),
                format!("{checked}{FACT_SEPARATOR}it said \u{201c}{said}\u{201d}")
            );
            let quiet = receipt(leg, "");
            assert_eq!(said_words(&quiet), "it said nothing is left");
        }
        let detached = LandReceipt {
            branch: crate::harness::repo_work::DETACHED.to_owned(),
            ..receipt(ReceiptLeg::OnMain, said)
        };
        assert!(
            checked_words(Some(&detached)).ends_with("no stash on its detached HEAD"),
            "{}",
            checked_words(Some(&detached))
        );
        // A receipt journaled before the branch was read (9b's) names none, and is no
        // detached HEAD.
        let unread = LandReceipt {
            branch: String::new(),
            ..receipt(ReceiptLeg::OnMain, said)
        };
        assert!(
            checked_words(Some(&unread)).ends_with("and no stash on HEAD"),
            "{}",
            checked_words(Some(&unread))
        );
        // The title's brief: on main the head, the ref and the folder, no clause; every other
        // leg its whole words; never the agent's words; with no receipt, no record.
        for leg in ReceiptLeg::ALL {
            let r = receipt(leg, said);
            let brief = checked_brief(Some(&r));
            if leg == ReceiptLeg::OnMain {
                assert_eq!(
                    brief,
                    "aterm checked 3f9a1c2e4b7d on origin/main in /Users//me/src/proj"
                );
            } else {
                assert_eq!(brief, checked_words(Some(&r)), "{leg:?}");
            }
            assert_eq!(
                landed_brief(Some(&r)),
                format!("{brief}; {LANDED_REST}"),
                "{leg:?}"
            );
            assert!(!landed_brief(Some(&r)).contains(said), "{leg:?}");
            assert!(!landed_brief(Some(&r)).contains("remote main"), "{leg:?}");
            assert!(
                [CHECKED_LEAD, NO_REPO_CHECKED, GONE_LEAD]
                    .iter()
                    .any(|lead| brief.starts_with(lead)),
                "{leg:?}: {brief}"
            );
        }
        assert_eq!(landed_brief(None), format!("{NO_RECORD}; {LANDED_REST}"));
        assert_eq!(checked_words(None), NO_RECORD);
        assert_eq!(landed_tip(None), NO_RECORD);
        let none = landed_words(None);
        assert_eq!(
            none,
            format!("it said nothing is left; {NO_RECORD}; {LANDED_REST}")
        );
        assert!(!none.contains("3f9a1c2e4b7d"), "{none}");
    }

    /// A QUOTE CUT TO ITS LINE KEEPS ITS CLOSING MARK (the review round of slice 9c): the Messages
    /// record caps its line at `aterm_messages::DETAIL_LINE_CAP` characters, and a receipt keeps
    /// up to `SAID_BYTES` of the agent's words, so `it said “` and the mark would push a long
    /// quote past the cap. Cut to the cap, a quote ends in one `…` and `”`, whether or not the
    /// receipt had already cut it; one that fits is whole. NEGATIVE CONTROL (run by hand): the
    /// whole quote at any bound fails it.
    #[test]
    fn a_quote_cut_to_its_line_keeps_its_closing_mark() {
        use crate::harness::land_receipt::SAID_BYTES;
        let cap = 240;
        let long = format!("Nothing left: {}", "x".repeat(SAID_BYTES));
        let cut_by_9b = format!("{}\u{2026}", &long[..SAID_BYTES - 3]);
        for said in [long.as_str(), cut_by_9b.as_str()] {
            let r = receipt(ReceiptLeg::OnMain, said);
            let words = said_within(&r, cap);
            assert_eq!(words.chars().count(), cap, "{words}");
            assert!(words.ends_with("x\u{2026}\u{201d}"), "{words}");
            assert_eq!(words.matches('\u{2026}').count(), 1, "{words}");
            assert!(
                words.starts_with("it said \u{201c}Nothing left: "),
                "{words}"
            );
        }
        let short = receipt(ReceiptLeg::OnMain, "Nothing left: pushed.");
        assert_eq!(
            said_within(&short, cap),
            "it said \u{201c}Nothing left: pushed.\u{201d}"
        );
        assert_eq!(said_within(&short, cap), said_words(&short));
        assert_eq!(
            said_within(&receipt(ReceiptLeg::OnMain, ""), 3),
            "it said nothing is left"
        );
    }

    /// A LANDING THAT STOOD ON THE LAND CHECK SAYS SO, AND ONLY THEN (slice 13a merged into
    /// 9c's words): with the receipt's `check`, what the agent said is [`ANSWERED_CHECK`], then
    /// the defense the check audited; without it, the defense alone; a receipt that kept no
    /// defense says the answer or that it said nothing is left. The cut to a line keeps the
    /// answer and the closing mark. NEGATIVE CONTROL (run by hand): `said_within` reading no
    /// `check` fails it.
    #[test]
    fn a_landing_on_the_land_check_says_it_answered_it() {
        let mut r = receipt(ReceiptLeg::OnMain, "Nothing left: pushed.");
        assert_eq!(
            said_words(&r),
            "it said \u{201c}Nothing left: pushed.\u{201d}"
        );
        assert!(!landed_words(Some(&r)).contains("land check"));
        r.check = true;
        assert_eq!(
            said_words(&r),
            format!("{ANSWERED_CHECK}, after it said \u{201c}Nothing left: pushed.\u{201d}")
        );
        assert!(landed_words(Some(&r)).contains(ANSWERED_CHECK));
        assert!(landed_tip(Some(&r)).contains(ANSWERED_CHECK));
        assert!(!landed_brief(Some(&r)).contains("land check"));
        r.said = "x".repeat(400);
        let cut = said_within(&r, 120);
        assert_eq!(cut.chars().count(), 120, "{cut}");
        assert!(
            cut.starts_with(ANSWERED_CHECK) && cut.ends_with("x\u{2026}\u{201d}"),
            "{cut}"
        );
        r.said.clear();
        assert_eq!(said_words(&r), ANSWERED_CHECK);
        r.check = false;
        assert_eq!(said_words(&r), "it said nothing is left");
    }
}
