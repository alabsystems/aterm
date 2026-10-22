// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_needs_line_is_read_under_every_list_marker_and_emphasis() {
    for line in [
        "NEEDS YOU: the release credential",
        "- NEEDS YOU: the release credential",
        "* NEEDS YOU: the release credential",
        "+ NEEDS YOU: the release credential",
        "• NEEDS YOU: the release credential",
        "1. NEEDS YOU: the release credential",
        "2) NEEDS YOU: the release credential",
        "**NEEDS YOU:** the release credential",
        "**NEEDS YOU**: the release credential",
        "__NEEDS YOU:__ the release credential",
        "- **NEEDS YOU:** the release credential",
        "*NEEDS YOU:* the release credential",
        "**NEEDS YOU: the release credential**",
        "  needs you: the release credential",
        "Needs You : the release credential",
    ] {
        assert_eq!(
            needs_you(&format!("Landed the parser.\n{line}")),
            ["the release credential"],
            "{line:?}"
        );
    }
}

#[test]
fn a_needs_line_in_a_fence_or_named_as_a_thing_is_no_need() {
    for tail in [
        "```\nNEEDS YOU: the release credential\n```",
        "~~~\nNEEDS YOU: the release credential\n~~~",
        "The primer says a NEEDS YOU: line reaches the person.",
        "NEEDS YOUR review, as the brief says.",
        "“NEEDS YOU:” lines reach the person.",
        "⎿ NEEDS YOU: the release credential",
        "Nothing left: no NEEDS YOU, no open work.",
        "See `done. NEEDS YOU: x` in the brief.",
    ] {
        assert!(needs_you(tail).is_empty(), "{tail:?}");
        assert_eq!(without_needs(tail), tail, "{tail:?}");
    }
    // A fence closed before the line leaves it read.
    assert_eq!(
        needs_you("```\nlog\n```\nNEEDS YOU: a decision on the schema"),
        ["a decision on the schema"]
    );

    // The other mark, a shorter run, and a run carrying text are code
    // inside the current block. Only its matching bare close ends it.
    for (open, inner, close) in [
        ("~~~~markdown", "```", "~~~~"),
        ("````markdown", "~~~", "````"),
        ("````markdown", "```", "````"),
        ("~~~~markdown", "~~~", "~~~~"),
        ("```markdown", "``` not a close", "```"),
        ("~~~markdown", "~~~ not a close", "~~~"),
        ("```markdown", "~~~", "````"),
    ] {
        let tail = format!(
            "{open}\n{inner}\nNEEDS YOU: the release credential\n\
             Nothing left: all of it is on origin/main.\n{close}"
        );
        assert!(needs_you(&tail).is_empty(), "{tail:?}");
        assert_eq!(without_needs(&tail), tail, "{tail:?}");
        assert_eq!(defense_line(&tail), None, "{tail:?}");
        assert_eq!(defense_body(&tail), tail, "{tail:?}");

        let outside = format!(
            "{tail}\nNEEDS YOU: a decision on the schema\n\
             Nothing left: all of it is on origin/main."
        );
        assert_eq!(needs_you(&outside), ["a decision on the schema"]);
        assert_eq!(
            defense_line(&outside),
            Some("Nothing left: all of it is on origin/main.")
        );
    }
}

/// A NEEDS YOU TOKEN A LINE'S START NAMES IS NO NEED ([`names_a_head`]; the
/// review of b9/spawn-residuals, F1): aterm's own instruction (`put what only
/// a person can give on a line starting NEEDS YOU: and carry on`), in every
/// act it types and a worker's first prompt, read back where a screen
/// cannot bound the message (agy) or quoted by a worker, on its row or
/// wrapped at any word of it. Read as a head, the words after the token stood
/// as a need nobody said; so did the top row of a reply cut there
/// (`starting NEEDS YOU: and land the rest`) and the ack's bare token naming
/// the line (`NEEDS YOU line is on the person's screen now`). NEGATIVE
/// CONTROLS: the token said — at a line's start, after a sentence, under a
/// row ending `start` or `starting` with no line's noun before it, opening
/// the text after `Starting` in capitals, in the bare form before its need,
/// joined on by `and` below the text's first row — is a need.
#[test]
fn a_needs_token_a_lines_start_names_is_no_need() {
    for tail in [
        "Put what only a person can give on a line starting NEEDS YOU: and carry on.",
        "Never stop to ask: put what only a person can give on a line starting NEEDS YOU: and \
         land the rest. Once it is all on remote main, defend that in a few lines starting \
         Nothing left:",
        "(Started by `s-1`. A line starting `NEEDS YOU:` reaches it and the person.)",
        "I put it on a line beginning with NEEDS YOU: as the brief asks.",
        "Lines starting **NEEDS YOU:** reach the person.",
        "Put what only a person can give on a line starting\nNEEDS YOU: and carry on.",
        "Put what only a person can give on a line\nstarting NEEDS YOU: and carry on.",
        "Put what only a person can give on a line starting with\nNEEDS YOU: and carry on.",
        // The top row of a reply cut there: the sentence began above it.
        "starting NEEDS YOU: and land the rest. Once it is all on remote main, defend that",
        "NEEDS YOU: and carry on with\neverything else.\n\nImplemented the parser.",
        // An owner's rule naming the line, its article on the row above.
        "Never force-push; a\nNEEDS YOU: line is for credentials only.",
        // aterm's ack, wrapped onto a row of its own: the bare token naming
        // the line.
        "Your\nNEEDS YOU line is on the person's screen now. Do not wait for it: carry on.",
    ] {
        assert!(
            needs_you(tail).is_empty(),
            "{tail:?}: {:?}",
            needs_you(tail)
        );
    }
    for tail in [
        "Landed the parser.\nNEEDS YOU: the release credential",
        "Everything is on main. NEEDS YOU: the release credential",
        "Here is what I need before I can start\nNEEDS YOU: the release credential",
        "The deploy is starting\nNEEDS YOU: the release credential",
        "Starting now. NEEDS YOU: the release credential",
        "The deploy is starting; NEEDS YOU: the release credential",
        "Starting NEEDS YOU: the release credential",
        "NEEDS YOU the release credential",
    ] {
        assert_eq!(needs_you(tail), ["the release credential"], "{tail:?}");
    }
    // A small `and` after the token below the text's first row is a need.
    assert_eq!(
        needs_you("Implemented the parser.\nNEEDS YOU: and the release credential"),
        ["and the release credential"]
    );
}

/// A NEEDS YOU line in a quote, in ticks, inside a sentence or spelled
/// `NEED YOU` is a need (each was
/// lost, and a lost need lands with the need off the screen). NEGATIVE
/// CONTROLS: the mentions in `a_needs_line_in_a_fence_or_named_as_a_thing_is_no_need`.
#[test]
fn a_needs_line_quoted_ticked_or_inside_a_sentence_is_a_need() {
    for tail in [
        "`NEEDS YOU: the release credential`",
        "> NEEDS YOU: the release credential",
        "▎ NEEDS YOU: the release credential",
        "> **NEEDS YOU:** the release credential",
        "`NEEDS YOU:` the release credential",
        "NEED YOU: the release credential",
        "NEEDS YOU... the release credential",
        "One thing NEEDS YOU: the release credential",
        "- `release`: NEEDS YOU: the release credential",
    ] {
        assert_eq!(needs_you(tail), ["the release credential"], "{tail:?}");
        assert!(!says_no_needs(tail), "{tail:?}");
    }
    assert_eq!(
        needs_you("Nothing left (NEEDS YOU: the TCC grant)."),
        ["the TCC grant)."]
    );
}

#[test]
fn needs_are_one_a_line_each_once_and_the_items_under_a_bare_head() {
    let tail = "Done with the parser.\nNEEDS YOU: the release credential\nNEEDS YOU: a \
                decision on the schema\nNEEDS YOU: the release credential";
    assert_eq!(
        needs_you(tail),
        ["the release credential", "a decision on the schema"]
    );
    let bare = "NEEDS YOU:\n\n- the release credential\n- a decision on the schema\nAll else \
                is landed.";
    assert_eq!(
        needs_you(bare),
        ["the release credential", "a decision on the schema"]
    );
    assert_eq!(without_needs(bare), "All else is landed.");
}

/// ONE NEED A LINE: a head whose words name a need is that one need, and the list under it is
/// the report's own. The lab stub's tall reply (`aterm-lab`'s `forever::tall_reply`, Land's L6
/// and r10's land-d: `NEEDS YOU: rotate the deploy key` over 80 `- check <n> passed` rows) was
/// read as 80 needs (`NEEDS … count=80`, "(+79 more)"); it names one, and the 80 rows stay in
/// the words every other reader judges. CONTROLS: two NEEDS YOU lines are two needs; words
/// ending in a colon introduce the list under them, its items the needs; a bare head takes its
/// list; a none over a list still reads the list (FAIL TOWARD A KEPT NEED).
#[test]
fn a_head_that_names_its_need_is_one_need_over_any_list() {
    let clean = "Nothing left: the parser is committed and pushed to origin/main.";
    let mut tall = String::from("Here is the landing report.\nNEEDS YOU: rotate the deploy key\n");
    for i in 1..=80 {
        tall.push_str(&format!("- check {i} passed\n"));
    }
    tall.push_str(clean);
    assert_eq!(needs_you(&tall), ["rotate the deploy key"]);
    assert!(!says_no_needs(&tall));
    let kept = without_needs(&tall);
    assert_eq!(
        kept.lines().filter(|l| l.starts_with("- check ")).count(),
        80
    );
    assert!(!kept.contains("NEEDS YOU"), "{kept}");
    assert_eq!(defense_line(&tall), Some(clean));
    // CONTROLS.
    assert_eq!(
        needs_you(
            "NEEDS YOU: rotate the deploy key\nNEEDS YOU: approve the release\n- check 1 passed"
        ),
        ["rotate the deploy key", "approve the release"]
    );
    assert_eq!(
        needs_you("NEEDS YOU: two things:\n- the deploy key\n- the release approval"),
        ["the deploy key", "the release approval"]
    );
    assert_eq!(
        needs_you("NEEDS YOU:\n- the deploy key\n- the release approval"),
        ["the deploy key", "the release approval"]
    );
    assert_eq!(
        needs_you("NEEDS YOU: none\n- the signing key"),
        ["the signing key"]
    );
}

/// A BARE HEAD WITH NO LIST UNDER IT names the paragraph under it, blank rows
/// between them passed over, its wrapped rows joined; with no words under it
/// at all (another head, a fence, the report's end) it still names a need
/// ([`UNREAD_NEED`]), so no defense lands beside a head nobody can read as
/// `nothing`. A paragraph that says none clears, as a `nothing` line does.
/// NEGATIVE CONTROLS: a quoted or backticked head names nothing, and a bare
/// head over a list keeps naming the list.
#[test]
fn a_bare_head_names_the_paragraph_under_it_or_a_need_it_cannot_read() {
    let defense = "Nothing left: every branch is merged into the remote main branch and pushed.";
    for (said, want) in [
        (
            "NEEDS YOU:\nThe release needs your signing credential to publish.",
            "The release needs your signing credential to publish.",
        ),
        (
            "NEEDS YOU:\n\nYour signing credential, to publish the\nrelease to the mirror.",
            "Your signing credential, to publish the release to the mirror.",
        ),
        (
            "**NEEDS YOU:**\nThe signing credential for the release.",
            "The signing credential for the release.",
        ),
        (
            "NEEDS YOU: nothing\nNEEDS YOU:\nThe signing credential for the release.",
            "The signing credential for the release.",
        ),
    ] {
        for tail in [format!("{defense}\n{said}"), format!("{said}\n{defense}")] {
            assert_eq!(needs_you(&tail), [want], "{tail:?}");
            assert!(!says_no_needs(&tail), "{tail:?}");
            assert_eq!(without_needs(&tail).trim(), defense, "{tail:?}");
        }
    }
    for tail in [
        format!("{defense}\nNEEDS YOU:"),
        format!("{defense}\nNEEDS YOU:\n\n"),
        format!("NEEDS YOU:\n{defense}"),
        format!("{defense}\nNEEDS YOU:\n```\nlog\n```"),
    ] {
        assert_eq!(needs_you(&tail), [UNREAD_NEED], "{tail:?}");
        assert_eq!(defense_line(&tail), Some(defense), "{tail:?}");
    }
    assert!(says_no_needs("Landed.\nNEEDS YOU:\nNothing."));
    assert!(says_no_needs("Landed.\nNEEDS YOU:\n- none"));
    // A quoted or ticked bare head takes its paragraph too.
    for tail in [
        format!("{defense}\n> NEEDS YOU:\nThe signing credential."),
        format!("{defense}\n`NEEDS YOU:`\nThe signing credential."),
    ] {
        assert_eq!(needs_you(&tail), ["The signing credential."], "{tail:?}");
    }
    assert_eq!(
        needs_you("NEEDS YOU:\n- the credential\nAll else is landed."),
        ["the credential"]
    );
}

/// A HEADING AND A DASH OPEN EITHER HEAD: `## NEEDS YOU: …`, `NEEDS YOU — …`,
/// `## Nothing left:`, `### Nothing left` (a heading that is the head alone)
/// and `Nothing left to do:` are read as the plain heads are. NEGATIVE
/// CONTROLS: a word that only begins as the head (`Nothing leftover`,
/// `NEEDS YOURSELF`), a hyphen inside a word, and prose with no separator.
#[test]
fn a_heading_or_a_dash_opens_either_head() {
    for line in [
        "## NEEDS YOU: the signing credential for the release",
        "### NEEDS YOU — the signing credential for the release",
        "NEEDS YOU — the signing credential for the release",
        "NEEDS YOU – the signing credential for the release",
        "NEEDS YOU - the signing credential for the release",
        "**NEEDS YOU** — the signing credential for the release",
    ] {
        assert_eq!(
            needs_you(&format!("Landed.\n{line}")),
            ["the signing credential for the release"],
            "{line:?}"
        );
    }
    for tail in [
        "## Nothing left:\nEverything is merged into origin/main and pushed.",
        "### Nothing left\nEverything is merged into origin/main and pushed.",
        "Nothing left — everything is merged into origin/main and pushed.",
        "Nothing left to do: everything is merged into origin/main and pushed.",
        "Nothing left to land: everything is merged into origin/main and pushed.",
        "**Nothing left**: everything is merged into origin/main and pushed.",
    ] {
        assert!(defense_line(tail).is_some(), "{tail:?}");
    }
    // NEGATIVE CONTROLS.
    for tail in [
        "Nothing leftover: the cache is warm.",
        "Nothing left-handed here.",
        "Nothing left in the cache, so it rebuilt.",
        "#Nothing left: not a heading without its space",
    ] {
        assert_eq!(defense_line(tail), None, "{tail:?}");
    }
    for tail in [
        "NEEDS YOURSELF: a break",
        "NEEDS YOU-know-what",
        "NEEDS YOU'RE here",
    ] {
        assert!(needs_you(tail).is_empty(), "{tail:?}");
    }
}

/// EVERY HEAD FORM A WORKER WRITES NAMES ITS NEED, before the defense line
/// and after it, and no defense lands beside it: `**NEEDS YOU**` or a bare
/// `NEEDS YOU` alone on its line over a list (the items the needs), the same
/// heads with nothing under them ([`UNREAD_NEED`]), a head led by an emoji
/// (`🚨`, `⚠️`) or a task list's checkbox (`- [ ]`), a qualifier in
/// parentheses (`NEEDS YOU (blocking):`), a double dash (`NEEDS YOU --`), and
/// the need's words straight after the token. The review of 2026-10-20 found
/// each read as no need, so a defense landed beside it. NEGATIVE CONTROLS:
/// the same heads saying none stands clear, and the defense alone lands.
#[test]
fn every_head_form_names_its_need_beside_a_defense() {
    use crate::supervise::policy::turn_end::clean_defense;
    let defense = "Nothing left: every branch is merged into the remote main branch and pushed.";
    let two = ["the signing credential", "a decision on the schema"];
    let forms: [(&str, &[&str]); 13] = [
        (
            "**NEEDS YOU**\n- the signing credential\n- a decision on the schema",
            &two,
        ),
        (
            "NEEDS YOU\n\n- the signing credential\n- a decision on the schema",
            &two,
        ),
        (
            "## NEEDS YOU\n1. the signing credential\n2. a decision on the schema",
            &two,
        ),
        ("🚨 NEEDS YOU: the signing credential", &two[..1]),
        ("⚠️ NEEDS YOU: the signing credential", &two[..1]),
        ("- ⚠️ **NEEDS YOU:** the signing credential", &two[..1]),
        ("NEEDS YOU (blocking): the signing credential", &two[..1]),
        (
            "**NEEDS YOU (blocking):** the signing credential",
            &two[..1],
        ),
        ("- [ ] NEEDS YOU: the signing credential", &two[..1]),
        ("- [x] NEEDS YOU: the signing credential", &two[..1]),
        ("NEEDS YOU -- the signing credential", &two[..1]),
        ("NEEDS YOU the signing credential", &two[..1]),
        (
            "NEEDS YOU (blocking)\n- the signing credential\n- a decision on the schema",
            &two,
        ),
    ];
    for (said, want) in forms {
        for tail in [format!("{defense}\n{said}"), format!("{said}\n{defense}")] {
            assert_eq!(needs_you(&tail), want, "{tail:?}");
            assert!(!clean_defense(Some(&tail)), "{tail:?}");
            assert_eq!(without_needs(&tail).trim(), defense, "{tail:?}");
        }
    }
    for bare in [
        "**NEEDS YOU**",
        "NEEDS YOU",
        "🚨 NEEDS YOU",
        "NEEDS YOU (blocking)",
    ] {
        for tail in [format!("{defense}\n{bare}"), format!("{bare}\n{defense}")] {
            assert_eq!(needs_you(&tail), [UNREAD_NEED], "{tail:?}");
            assert!(!clean_defense(Some(&tail)), "{tail:?}");
        }
    }
    // NEGATIVE CONTROLS.
    for none in [
        "🚨 NEEDS YOU: nothing",
        "- [ ] NEEDS YOU: none",
        "NEEDS YOU (blocking): nothing",
        "NEEDS YOU (none)",
        "**NEEDS YOU** -- none",
        "NEEDS YOU none",
    ] {
        for tail in [format!("{defense}\n{none}"), format!("{none}\n{defense}")] {
            assert!(needs_you(&tail).is_empty(), "{tail:?}");
            assert!(says_no_needs(&tail), "{tail:?}");
            assert!(clean_defense(Some(&tail)), "{tail:?}");
        }
    }
    assert!(clean_defense(Some(defense)));
}

#[test]
fn a_wrapped_need_is_joined_and_a_new_sentence_is_not() {
    let wrapped = "NEEDS YOU: the owner's release credential for the staging\nmirror and the \
                   signing key\nEverything else is on main.";
    assert_eq!(
        needs_you(wrapped),
        ["the owner's release credential for the staging mirror and the signing key"]
    );
    assert_eq!(without_needs(wrapped), "Everything else is on main.");
    let stops = "NEEDS YOU: the release credential.\nthen the tag goes out";
    assert_eq!(needs_you(stops), ["the release credential."]);
    assert_eq!(without_needs(stops), "then the tag goes out");
}

/// A NONE LINE'S CLAUSE IS READ FOR OPEN WORK ([`without_needs`] keeps it,
/// the head dropped): `NEEDS YOU: none — all of it is pushed to the branch`
/// names no need, but what its restatement leaves open (a branch, not main)
/// is open, under Land's strict reading as on an ordinary line. The recheck
/// of ea564676a3 found such a clause dropped with the line, so the defense
/// landed over work left on a branch. A clause that says more than a
/// restatement (`nothing — the docs fix is parked on a branch for later`)
/// is a need now ([`none_said`]: FAIL TOWARD A KEPT NEED), and lands
/// nothing either. NEGATIVE CONTROL: a clause that restates the landing
/// leaves nothing open, in its place with a capital.
#[test]
fn a_none_lines_clause_is_read_for_open_work() {
    use crate::supervise::policy::turn_end::{clean_defense, open_work_strict};
    let defense = "Nothing left: all of it is on the remote main branch.";
    for line in [
        "NEEDS YOU: none — all of it is pushed to the branch",
        "NEEDS YOU: nothing; everything is pushed to the remote branch",
    ] {
        let tail = format!("Merged and pushed main.\n{line}\n{defense}");
        assert!(needs_you(&tail).is_empty(), "{tail:?}");
        assert!(says_no_needs(&tail), "{tail:?}");
        assert!(!clean_defense(Some(&tail)), "{tail:?}");
        let found = open_work_strict(Some(&defense_body(&tail)));
        assert!(found.contains(&"to a branch"), "{tail:?}: {found:?}");
    }
    for line in [
        "NEEDS YOU: nothing — the docs fix is parked on a branch for later",
        "NEEDS YOU: none; the e2e suite is not run yet and the parser branch is unmerged",
    ] {
        let tail = format!("Merged and pushed main.\n{line}\n{defense}");
        assert_eq!(needs_you(&tail).len(), 1, "{tail:?}");
        assert!(!clean_defense(Some(&tail)), "{tail:?}");
    }
    // NEGATIVE CONTROL: the clause restates the landing.
    let tail = format!("Merged.\nNEEDS YOU: nothing — everything is on main.\n{defense}");
    assert_eq!(
        without_needs(&tail),
        format!("Merged.\nEverything is on main.\n{defense}")
    );
    assert!(clean_defense(Some(&tail)));

    // Opposite case-fold width changes can cancel in total while moving
    // the first clause boundary. Both orders use the consistent lower copy.
    for (rest, kept) in [
        (
            "no KKKK needed; no ȺȺȺȺȺȺȺȺ needed; all LANDED",
            "No ⱥⱥⱥⱥⱥⱥⱥⱥ needed; all landed",
        ),
        (
            "no ȺȺȺȺȺȺȺȺ needed; no KKKK needed; all LANDED",
            "No kkkk needed; all landed",
        ),
        ("no KKKK needed; all LANDED", "All landed"),
        ("no ȺȺȺȺȺȺȺȺ needed; all LANDED", "All landed"),
        // Width-preserving Unicode and ASCII keep the original spelling.
        ("no Γ needed; all LANDED", "All LANDED"),
        ("no key needed; all LANDED", "All LANDED"),
    ] {
        let line = format!("NEEDS YOU: {rest}");
        assert!(needs_you(&line).is_empty(), "{line:?}");
        assert!(says_no_needs(&line), "{line:?}");
        assert_eq!(without_needs(&line), kept, "{line:?}");
    }
    // A real request after the same none clauses stays a need.
    let rest = "no KKKK needed; no ȺȺȺȺȺȺȺȺ needed; please sign the release";
    let line = format!("NEEDS YOU: {rest}");
    assert_eq!(needs_you(&line), [rest]);
    assert!(!says_no_needs(&line));
}

/// PROSE THAT WRAPS BEFORE `needs you` IS NO HEAD ([`need_rest`]): a
/// sentence a screen row cut right before `needs` puts `needs you.` at the
/// start of a row, and `Needs you? No.` asks. The bare form, with no
/// separator, is the head only written in capitals as the convention spells
/// it (or closed by emphasis or a qualifier), and never where punctuation
/// other than a separator follows the token. The recheck of ea564676a3
/// found each read as a need (`.`), so the landing was blocked and Cruise
/// raised it. NEGATIVE CONTROLS: the same words with a separator, in any
/// case, and the bare head in capitals, are heads.
#[test]
fn prose_wrapped_before_needs_you_is_no_head() {
    use crate::supervise::policy::turn_end::clean_defense;
    let defense = "Nothing left: all of it is merged into the remote main branch.";
    for tail in [
        // `nothing else`, not `nothing here`: Land's strict reading takes a
        // none a place qualifies (`nothing here`) for work kept elsewhere.
        format!("Merged the parser and pushed main; nothing else\nneeds you.\n{defense}"),
        format!("{defense} And nothing\nneeds you, the suites pass on main."),
        format!("Merged and pushed main; no step\nneeds you to act.\n{defense}"),
        format!("Needs you? No.\n{defense}"),
        format!("NEEDS YOU. That is the convention.\n{defense}"),
    ] {
        assert!(needs_you(&tail).is_empty(), "{tail:?}");
        assert!(clean_defense(Some(&tail)), "{tail:?}");
    }
    // NEGATIVE CONTROLS.
    for (line, want) in [
        ("needs you: the signing key", "the signing key"),
        ("NEEDS YOU the signing key", "the signing key"),
        ("**Needs you** the signing key", "the signing key"),
        ("🚨 NEEDS YOU! the signing key", "the signing key"),
    ] {
        assert_eq!(needs_you(&format!("Landed.\n{line}")), [want], "{line:?}");
    }
}

#[test]
fn a_dashed_or_qualified_head_still_names_a_need() {
    for line in [
        "NEEDS YOU — the release credential",
        "NEEDS YOU – the release credential",
        "NEEDS YOU - the release credential",
        "NEEDS YOU—the release credential",
        "**NEEDS YOU** — the release credential",
        "NEEDS YOU (owner): the release credential",
        "**NEEDS YOU (owner):** the release credential",
        "- NEEDS YOU (Andrew) — the release credential",
    ] {
        assert_eq!(
            needs_you(&format!("Landed the parser.\n{line}")),
            ["the release credential"],
            "{line:?}"
        );
    }
    // A hyphen joined to the head is a word, not a head's end.
    assert!(needs_you("NEEDS YOU-related notes are in the brief.").is_empty());
}

#[test]
fn a_need_is_sanitized_and_cut() {
    let long = format!("NEEDS YOU: {}", "word ".repeat(80));
    let need = &needs_you(&long)[0];
    assert!(need.chars().count() <= NEED_CHARS, "{need}");
    assert!(need.ends_with('…'), "{need}");
    assert_eq!(
        needs_you("NEEDS YOU: the\u{7}  release\tcredential  **"),
        ["the release credential"]
    );
}

#[test]
fn the_defense_line_is_read_by_the_same_line_test() {
    for line in [
        "Nothing left: every branch is on remote main.",
        "- Nothing left: every branch is on remote main.",
        "**Nothing left:** every branch is on remote main.",
        "1. NOTHING LEFT: every branch is on remote main.",
        "nothing left: every branch is on remote main.",
    ] {
        let tail = format!("Merged and pushed.\n{line}\nThe worktree is removed.");
        assert_eq!(defense_line(&tail), Some(line.trim()), "{line:?}");
    }
    for tail in [
        "There is nothing left: every branch is on remote main.",
        "`Nothing left:` is the head the harness reads.",
        "```\nNothing left: x\n```",
        "> Nothing left: quoted from the worker before",
        "Nothing left to do.",
    ] {
        assert_eq!(defense_line(tail), None, "{tail:?}");
    }
}

#[test]
fn the_defense_body_drops_the_head_and_the_needs() {
    let tail = "Merged.\nNothing left: the WIP branch is parked\nNEEDS YOU: the credential";
    assert_eq!(defense_body(tail), "Merged.\nThe WIP branch is parked");
}

#[test]
fn equal_sets_key_equal_and_none_keys_zero() {
    let a = vec!["x".to_string(), "y".to_string()];
    assert_eq!(needs_key(&a), needs_key(&a.clone()));
    assert_ne!(needs_key(&a), needs_key(&["x".to_string()]));
    assert_eq!(needs_key(&[]), 0);
}

/// The word lists of [`NoneWords`] by name, each to be mutated in turn.
fn none_list<'b, 'a>(
    by: &'b mut NoneWords<'a>,
    k: usize,
) -> Option<(&'static str, &'b mut &'a [&'a str])> {
    Some(match k {
        0 => ("NONE_HEADS", &mut by.heads),
        1 => ("NOT_NEEDED", &mut by.not_needed),
        2 => ("NONE_MODS", &mut by.mods),
        3 => ("NONE_STILL", &mut by.still),
        4 => ("NONE_NEEDED", &mut by.needed),
        5 => ("NONE_KEEPS", &mut by.keeps),
        6 => ("RESTATE_SUBJECTS", &mut by.subjects),
        7 => ("RESTATE_NAMED", &mut by.named),
        8 => ("RESTATE_THE", &mut by.the),
        9 => ("RESTATE_LINKS", &mut by.links),
        10 => ("LANDED_SAYS", &mut by.landed),
        11 => ("RESTATE_AT", &mut by.at),
        12 => ("RESTATE_PLACES", &mut by.places),
        13 => ("NONE_TIMES", &mut by.times),
        14 => ("YOU_SET", &mut by.set),
        15 => ("NONE_OBJECTS", &mut by.objects),
        16 => ("NONE_JOINS", &mut by.joins),
        17 => ("NONE_ASKED", &mut by.asked),
        _ => return None,
    })
}

/// The needs table's NONE and NEED rows the needs reader misreads with the
/// word lists `by`: a NONE row it names a need in (a phantom), a NEED row it
/// names none in, or none carrying the row's words (a lost need).
fn misread_by(by: &NoneWords<'_>) -> Vec<String> {
    needs_table()
        .iter()
        .filter_map(|(row, report)| misread(row, report, by))
        .collect()
}

/// How the needs reader misreads the table's `report`, labelled `row`, with
/// the word lists `by` ([`misread_by`]), if it does.
fn misread(row: &NeedsRow, report: &str, by: &NoneWords<'_>) -> Option<String> {
    let named: Vec<String> = needs_lines_by(report, by)
        .needs
        .iter()
        .map(|n| sanitized(n))
        .filter(|n| !n.is_empty())
        .collect();
    let misread = match row {
        NeedsRow::None => !named.is_empty(),
        NeedsRow::Need(words) => !named
            .iter()
            .any(|n| n.to_lowercase().contains(&words.to_lowercase())),
        NeedsRow::Headless(_) => false,
    };
    misread.then(|| format!("{row:?} {report:?} -> {named:?}"))
}

/// EVERY WORD OF THE NONE READER'S LISTS IS LOAD-BEARING (the mutation
/// check, as `every_land_family_is_load_bearing` is Land's): with any one
/// word taken out of any one list of [`NONE_WORDS`], the needs table
/// (`fixtures/needs-table.txt`) reads some row otherwise — a none it held
/// becomes a phantom need, or a need it kept is lost — so no word is dead
/// weight, and none can be dropped without the table naming the report it
/// changes. The reader's own lists misread nothing. Each word's search is
/// the whole table, the rows that spell the word tried first: one of them
/// is usually the row it bears. NEGATIVE CONTROL: a reader that takes words
/// a person is asked for (`just`, `please`, `publish`, `key`, `tag`,
/// `approve`) as a time a none is said for loses needs on the table.
#[test]
fn every_none_word_is_load_bearing() {
    assert_eq!(misread_by(&NONE_WORDS), Vec::<String>::new());
    let table = needs_table();
    let lowered: Vec<String> = table.iter().map(|(_, r)| r.to_lowercase()).collect();
    let mut dead = Vec::new();
    for k in 0.. {
        let mut by = NONE_WORDS;
        let Some((name, list)) = none_list(&mut by, k) else {
            break;
        };
        let words: &[&str] = list;
        for (i, word) in words.iter().enumerate() {
            let kept: Vec<&str> = words
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, w)| *w)
                .collect();
            let mut by = NONE_WORDS;
            if let Some((_, list)) = none_list(&mut by, k) {
                *list = &kept;
            }
            let (first, rest): (Vec<usize>, Vec<usize>) =
                (0..table.len()).partition(|&r| lowered[r].contains(word));
            let bears = first.iter().chain(&rest).any(|&r| {
                let (row, report) = &table[r];
                misread(row, report, &by).is_some()
            });
            if !bears {
                dead.push(format!("{name}: {word:?}"));
            }
        }
    }
    assert!(dead.is_empty(), "words no row of the table needs: {dead:?}");
    // NEGATIVE CONTROL.
    let loose: Vec<&str> = NONE_TIMES
        .iter()
        .copied()
        .chain([
            "just", "please", "publish", "key", "tag", "approve", "the", "pr", "signing",
        ])
        .collect();
    let mut by = NONE_WORDS;
    by.times = &loose;
    let lost = misread_by(&by);
    assert!(
        lost.iter().any(|m| m.starts_with("Need(")),
        "a loose reader lost no need: {lost:?}"
    );
}

/// A NONE IN COMMON WORDS READS AS NONE (the review of b8/residuals,
/// 2026-10-20): `no action needed from you.`, `nothing at this point.`,
/// `None at this point.`, `nothing to do.`, `nothing else to do.`,
/// `nothing from my side.`, `zero.`, a dash alone, a none closed by its
/// sentence's stop with a statement after it, and `you're all set` after a
/// none. Each raised a phantom need on the person's screen and kept a clean
/// defense from landing. NEGATIVE CONTROLS: a none word whose clause names
/// a thing the person could be asked for (`no access to the signing
/// server`), a `nothing` whose later sentence asks, a `no` before a dash,
/// a `you` that asks, and a none word that is part of the need's words,
/// still name a need.
#[test]
fn a_none_in_common_words_reads_as_none() {
    use crate::supervise::policy::turn_end::clean_defense;
    let defense = "Nothing left: all of it is on the remote main branch.";
    for x in [
        "no action needed from you.",
        "nothing at this point.",
        "None at this point.",
        "nothing to do.",
        "nothing else to do.",
        "nothing from my side.",
        "nothing on my end",
        "zero.",
        "nope",
        "—",
        "-",
        "– ",
        "not needed",
        "nothing. Everything is on main.",
        "nothing, you're all set.",
        "none — you are good to go",
    ] {
        let tail = format!("Merged.\nNEEDS YOU: {x}\n{defense}");
        assert!(needs_you(&tail).is_empty(), "{x:?}: {:?}", needs_you(&tail));
        assert!(says_no_needs(&tail), "{x:?}");
        assert!(clean_defense(Some(&tail)), "{x:?}");
    }
    // NEGATIVE CONTROLS.
    for x in [
        "no access to the signing server",
        "nothing to do until you approve the deploy",
        "nothing. Please approve the deploy.",
        "nothing for now; once you are back, approve the deploy.",
        "none blocking — but the prod deploy waits on your approval.",
        "no — the key, please",
        "nothing — you have to run the signing step yourself",
        "not until you sign the release",
        "nothing works without the signing credential",
        "zero downtime needs your sign-off",
    ] {
        let tail = format!("Merged.\nNEEDS YOU: {x}\n{defense}");
        assert_eq!(needs_you(&tail).len(), 1, "{x:?}");
        assert!(!says_no_needs(&tail), "{x:?}");
        assert!(!clean_defense(Some(&tail)), "{x:?}");
    }
    // A head alone still heads the list below it: no none.
    assert_eq!(
        needs_you("Merged.\nNEEDS YOU:\n- approve the deploy"),
        ["approve the deploy"]
    );
}

/// A NEEDS-YOU HEAD WITH ITS WORDS HYPHENATED IS A HEAD (the review of
/// b8/residuals, 2026-10-20): `NEEDS-YOU: approve the deploy` (and the
/// underscore form) was read as an ordinary line, so the need never
/// reached the person and the turn's defense could land past it. NEGATIVE
/// CONTROLS: a hyphenated token in prose with no separator, or one that
/// runs on into a word, is no head.
#[test]
fn a_hyphenated_needs_you_head_is_a_head() {
    use crate::supervise::policy::turn_end::clean_defense;
    let defense = "Nothing left: all of it is on the remote main branch.";
    for line in [
        "NEEDS-YOU: approve the deploy",
        "NEEDS_YOU: approve the deploy",
        "- **Needs-you:** approve the deploy",
        "NEEDS-YOU approve the deploy",
    ] {
        let tail = format!("Merged.\n{line}\n{defense}");
        assert_eq!(needs_you(&tail), ["approve the deploy"], "{line:?}");
        assert!(!clean_defense(Some(&tail)), "{line:?}");
    }
    assert!(says_no_needs(&format!(
        "Merged.\nNEEDS-YOU: none\n{defense}"
    )));
    // NEGATIVE CONTROLS.
    for line in [
        "Needs-you lines are how a report asks.",
        "NEEDS-YOUR review is not the convention.",
        "needs-you approve the deploy",
    ] {
        assert!(
            needs_you(&format!("Merged.\n{line}")).is_empty(),
            "{line:?}"
        );
    }
}
