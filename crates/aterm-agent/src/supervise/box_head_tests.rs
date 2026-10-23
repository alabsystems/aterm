// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::approval_loop::whole_box_decision;
use super::super::box_head::joined_box;
use crate::supervise::policy::approval::{self as policy, ApprovalCtx, Choice, Decision};
use crate::supervise::policy::row_guard;
use aterm_phase::prompt::fixtures as fx;

/// A measured box's screen and the row its top rule is on (the rows above it
/// the transcript it was drawn under).
fn the_box(fixture: &str) -> (Vec<String>, usize) {
    let rows = fx::screen(fixture);
    let rule = rows
        .iter()
        .rposition(|r| aterm_phase::phase::is_rule(r))
        .expect("the box's top rule");
    (rows, rule)
}

/// Full power, as `policy/approval_tests.rs`'s `full()`.
fn full_power() -> ApprovalCtx {
    ApprovalCtx::new(
        PathBuf::from("/private/tmp/claude-502/scratch/work1"),
        Some(PathBuf::from("/Users/_owner")),
        502,
        Some(PathBuf::from("/var/folders/ab/xyz/T")),
        std::sync::Arc::new(crate::supervise::policy::LiveProbe),
    )
}

/// The cut a pane shows of `rows` from row `k` on, read as the loop reads it.
fn cut_at(rows: &[String], k: usize) -> aterm_phase::Reading {
    aterm_phase::read(Some("claude"), &rows[k..], Some(2))
}

/// THE MEASURED BOXES WHOSE TAILS ARE THE SAME, EACH CUT AT EVERY ROW, ARE
/// ANSWERED WHOLE FROM THEIR HEADS IN THE HISTORY: the rm breaker (2.1.287),
/// which full power answers `1` once, and the owner's Read (2.1.292,
/// accept-edits), which it holds for its folder grant
/// (`policy/approval_tests.rs`'s
/// `a_box_whose_head_is_off_the_screen_is_held_because_its_tail_names_no_box`).
#[test]
fn a_box_taller_than_the_pane_is_answered_whole_from_its_head_in_the_history_measured() {
    let ctx = full_power();
    for (name, fixture) in [
        ("rm breaker", fx::BOX_RM_BREAKER_2_1_287),
        ("Read", fx::BOX_READ_FORCED_ACCEPT_EDITS_2_1_292),
    ] {
        let (rows, rule) = the_box(fixture);
        let whole = policy::decide_screen(Some("claude"), &rows, &ctx).expect("a box");
        match (name, &whole) {
            (
                "rm breaker",
                Decision::Approve {
                    rule_id: policy::RULE_ALLOW_ONCE,
                    choice: Choice::Digit(1),
                    ..
                },
            ) => {}
            ("Read", Decision::Escalate { reason }) if reason.contains("no folder grant") => {}
            _ => panic!("PRECONDITION: {name} whole: {whole:?}"),
        }
        let mut cuts = 0;
        for k in rule + 1..rows.len() {
            let screen = &rows[k..];
            let cut = cut_at(&rows, k);
            let Some(p) = cut.prompt.as_ref().filter(|p| p.head_off_screen) else {
                continue;
            };
            cuts += 1;
            let held = policy::decide(&cut, screen, &ctx);
            assert!(
                matches!(held, Decision::Escalate { .. }),
                "{name} cut at {k}: the cut alone is pressed: {held:?}"
            );
            let (joined, reading) = joined_box(&rows[rule..k], screen, p, 2)
                .unwrap_or_else(|| panic!("{name} cut at {k}: no join"));
            assert_eq!(joined, rows[rule..], "{name} cut at {k}");
            let d = whole_box_decision(
                policy::decide(&reading, &joined, &ctx),
                &joined,
                screen,
                || held.clone(),
            );
            match (&whole, &d) {
                (
                    Decision::Approve {
                        rule_id, choice, ..
                    },
                    Decision::Approve {
                        rule_id: got,
                        choice: chose,
                        guard,
                        ..
                    },
                ) => {
                    assert_eq!((rule_id, choice), (got, chose), "{name} cut at {k}");
                    assert!(
                        screen.iter().any(|r| row_guard(r) == *guard),
                        "{name} cut at {k}: the guard {guard} names no row the screen shows"
                    );
                }
                (Decision::Escalate { reason }, Decision::Escalate { reason: got }) => {
                    assert_eq!(reason, got, "{name} cut at {k}");
                }
                _ => panic!("{name} cut at {k}: {d:?}, whole {whole:?}"),
            }
        }
        assert!(cuts >= 5, "{name}: only {cuts} cuts read as cut boxes");
    }
}

/// A HEAD THAT IS NOT THE BOX'S OWN IS NO JOIN, and the cut is held as before:
/// the history's rows from below the top rule (the rule not read); a row a
/// repaint left from column 0 (a transcript row) between the rule and the
/// screen; an older frame's rule over the composer's footer, or over a blank
/// row, or over the box's own rule, the box opening under it; and a cut whose
/// options are not the joined box's (its focus elsewhere).
#[test]
fn a_head_that_is_not_the_boxs_own_is_no_join() {
    let (rows, rule) = the_box(fx::BOX_RM_BREAKER_2_1_287);
    let k = rule + 8;
    let screen = &rows[k..];
    let cut = cut_at(&rows, k);
    let p = cut.prompt.as_ref().expect("a cut box");
    assert!(p.head_off_screen, "PRECONDITION: {p:?}");
    let head = rows[rule..k].to_vec();
    assert!(
        joined_box(&head, screen, p, 2).is_some(),
        "control: its own head joins"
    );
    let mut transcript_row = head.clone();
    transcript_row.insert(2, "⏺ Bash(rm -rf /)".to_string());
    let mut footer = vec![head[0].clone(), "  ? for shortcuts".to_string()];
    footer.extend(head[1..].iter().cloned());
    let under = |row: &str| {
        let mut h = vec![head[0].clone(), row.to_string()];
        h.extend(head[1..].iter().cloned());
        h
    };
    for (what, head) in [
        ("no top rule", head[1..].to_vec()),
        ("a transcript row", transcript_row),
        ("an older frame's rule over the footer", footer),
        ("an older frame's rule over a blank row", under("")),
        ("an older frame's rule over the box's", under(&head[0])),
        ("nothing", Vec::new()),
    ] {
        assert!(
            joined_box(&head, screen, p, 2).is_none(),
            "{what}: {head:#?}"
        );
    }
    let refocused: Vec<String> = screen
        .iter()
        .map(|r| match r.as_str() {
            " ❯ 1. Yes" => "   1. Yes".to_string(),
            "   2. No" => " ❯ 2. No".to_string(),
            _ => r.clone(),
        })
        .collect();
    assert_ne!(refocused, screen, "PRECONDITION: the focus moved");
    let other = aterm_phase::read(Some("claude"), &refocused, Some(2));
    let other = other.prompt.as_ref().expect("a cut box");
    assert!(
        joined_box(&head, screen, other, 2).is_none(),
        "a cut whose options are not the joined box's"
    );
}

/// WHAT A JOINED READ CARRIES OUT: an escalation, and a digit, pressed under
/// its own guard where the screen shows that row and else under the box's
/// first drawn row on the screen.
#[test]
fn a_joined_read_presses_a_digit_or_holds_and_nothing_else() {
    let head = [
        "─".repeat(40),
        " Bash command".to_string(),
        " │ rm -rf x".to_string(),
    ];
    let screen = vec![
        String::new(),
        " Do you want to proceed?".to_string(),
        " ❯ 1. Yes".to_string(),
    ];
    let rows: Vec<String> = head.iter().chain(&screen).cloned().collect();
    let approve = |guard: String, choice: Choice| Decision::Approve {
        rule_id: policy::RULE_ALLOW_ONCE,
        choice,
        guard,
        subject: "rm -rf x".to_string(),
        unproven: None,
    };
    let cut = || Decision::Escalate {
        reason: "the cut".to_string(),
    };
    let first_drawn = row_guard(" Do you want to proceed?");
    assert_eq!(
        whole_box_decision(
            approve(row_guard(" │ rm -rf x"), Choice::Digit(1)),
            &rows,
            &screen,
            cut
        ),
        approve(first_drawn, Choice::Digit(1)),
        "a guard on the head: the box's first drawn row on the screen"
    );
    let own = row_guard(" ❯ 1. Yes");
    assert_eq!(
        whole_box_decision(approve(own.clone(), Choice::Digit(1)), &rows, &screen, cut),
        approve(own, Choice::Digit(1)),
        "a guard on the screen: its own"
    );
    let held = Decision::Escalate {
        reason: "whole".to_string(),
    };
    assert_eq!(whole_box_decision(held.clone(), &rows, &screen, cut), held);
    let focus = approve(
        row_guard(" ❯ 1. Yes"),
        Choice::Focus {
            steps: 1,
            label: "No".to_string(),
        },
    );
    assert_eq!(whole_box_decision(focus, &rows, &screen, cut), cut());
    let decline = Decision::Decline {
        rule_id: policy::RULE_ALLOW_ONCE,
        refusal: 1,
        text: "no".to_string(),
        subject: "rm -rf x".to_string(),
    };
    assert_eq!(whole_box_decision(decline, &rows, &screen, cut), cut());
    let blank = vec![String::new(); 3];
    let rows: Vec<String> = head.iter().chain(&blank).cloned().collect();
    assert_eq!(
        whole_box_decision(
            approve(row_guard(" │ rm -rf x"), Choice::Digit(1)),
            &rows,
            &blank,
            cut
        ),
        cut(),
        "no drawn row on the screen: no guard"
    );
}

/// The rm breaker cut `off` rows under its top rule: the main screen's history
/// (its transcript and the box's head) and the screen (the rest).
fn breaker_cut(off: usize) -> (Vec<String>, Vec<String>) {
    let (rows, rule) = the_box(fx::BOX_RM_BREAKER_2_1_287);
    (rows[..rule + off].to_vec(), rows[rule + off..].to_vec())
}

/// A fenced host showing the breaker's cut on the main screen, its head in the
/// history, the screen sitting until a key (the generation unmoved); full
/// power.
fn breaker_host(off: usize) -> (Mock, SuperviseOpts) {
    let (history, screen) = breaker_cut(off);
    let mut m = fenced(vec![screen, idle_screen()]);
    m.key_releases = Some(0);
    m.alt_screen = Some(false);
    let mut older = fx::screen(fx::BOX_READ_FORCED_ACCEPT_EDITS_2_1_292);
    older.extend(history);
    m.scrollback = Some(older);
    let mut opts = auto(30);
    opts.policy.approve = Approve::All;
    opts.policy.rm_breaker_ask = false;
    (m, opts)
}

/// THE LOOP ANSWERS A BOX TALLER THAN THE PANE WHOLE, FROM ITS HEAD IN THE
/// MAIN SCREEN'S HISTORY: the rm breaker cut eight rows under its rule (its
/// command row off the screen), read with `search` (the top rule nearest
/// above the screen) and `line` (each row from it down), the screen re-read
/// unmoved, is pressed `1` at full power, fenced on the screen's generation
/// and guarded on its first drawn row.
#[test]
fn a_box_taller_than_the_pane_is_pressed_whole_only_where_its_head_reads() {
    let (_, screen) = breaker_cut(8);
    let guard = row_guard(
        screen
            .iter()
            .find(|r| !r.trim().is_empty())
            .expect("a drawn row"),
    );
    let (mut m, opts) = breaker_host(8);
    let mut s = session(&mut m, None);
    let r = s.supervise(&opts);
    drop(s);
    let presses = m.presses();
    assert_eq!(presses.len(), 1, "{r:?}: {:#?}", m.requests);
    assert!(
        presses[0].starts_with("key if-human=7 if-gen=1.")
            && presses[0].ends_with(&format!(" if={guard} 1")),
        "{presses:?}"
    );
    let older = fx::screen(fx::BOX_READ_FORCED_ACCEPT_EDITS_2_1_292).len();
    let (_, rule) = the_box(fx::BOX_RM_BREAKER_2_1_287);
    let rule = older + rule;
    for row in rule..rule + 8 {
        assert!(
            m.requests.contains(&format!("line {row}")),
            "the head's row {row} read: {:#?}",
            m.requests
        );
    }
    type Spoil = fn(&mut Mock);
    let controls: [(&str, Spoil); 7] = [
        ("the alternate screen", |m| m.alt_screen = Some(true)),
        ("a search that may have left out newer rows", |m| {
            m.search_qualifier = " incomplete rewrapping";
        }),
        ("the head's rule evicted", |m| {
            m.evicted_below = 1 + m.scrollback.as_ref().map_or(0, |h| {
                h.iter()
                    .rposition(|r| aterm_phase::phase::is_rule(r))
                    .map_or(0, |k| k as u64)
            });
        }),
        ("a head row unreadable", |m| {
            m.unreadable_row = m.scrollback.as_ref().and_then(|h| {
                h.iter()
                    .rposition(|r| r == " Delete temporary directories")
                    .map(|k| k as u64)
            });
        }),
        ("the generation moving", |m| {
            m.key_releases = None;
            m.screens.truncate(1);
        }),
        ("a transcript row under the rule", |m| {
            if let Some(h) = m.scrollback.as_mut() {
                let at = h.len() - 2;
                h.insert(at, "⏺ Bash(rm -rf /)".to_string());
            }
        }),
        ("no history served", |m| m.scrollback = None),
    ];
    for (what, spoil) in controls {
        let (mut m, opts) = breaker_host(8);
        spoil(&mut m);
        let mut s = session(&mut m, None);
        let r = s.supervise(&opts);
        drop(s);
        let written = m.presses().len() - m.gen_skipped;
        assert_eq!(written, 0, "{what}: {r:?}: {:#?}", m.requests);
        assert!(
            matches!(&r, Ok((out, _)) if out.starts_with("prompt\n")),
            "{what}: the owner's: {r:?}: {:#?}",
            m.requests
        );
    }
}

/// A CUT THE JOIN DOES NOT TAKE READS NO HISTORY and is held as the cut is: a
/// question (its answer's keys re-read the dialog off the screen, and the
/// policy's memory of it is the screen's).
#[test]
fn a_cut_question_reads_no_history() {
    let rows = fx::screen(fx::QUESTION_YES_NO);
    let k = 6;
    let cut = aterm_phase::read(Some("claude"), &rows[k..], Some(2));
    assert!(
        cut.prompt.as_ref().is_some_and(|p| p.head_off_screen),
        "PRECONDITION: a question cut at {k}: {:?}",
        cut.prompt
    );
    let mut m = fenced(vec![rows[k..].to_vec(), idle_screen()]);
    m.program = "claude";
    m.key_releases = Some(0);
    m.alt_screen = Some(false);
    m.scrollback = Some(rows[..k].to_vec());
    let mut opts = auto(30);
    opts.policy.approve = Approve::All;
    let mut s = session(&mut m, None);
    let r = s.supervise(&opts);
    drop(s);
    assert!(m.presses().is_empty(), "{r:?}: {:#?}", m.requests);
    assert!(
        matches!(&r, Ok((out, _)) if out.starts_with("prompt\n")),
        "{r:?}: {:#?}",
        m.requests
    );
    let read: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| ["modes", "search", "line"].contains(&verb_of(r)))
        .collect();
    assert!(read.is_empty(), "{read:?}");
}

/// screens-1, DECIDED: A HEAD IS READ FROM AT MOST
/// [`HEAD_ROWS`] ROWS ABOVE THE SCREEN, one `line` read a row, and a box
/// whose top rule lies further up is held as the cut is, its history never
/// read.
#[test]
fn a_head_more_than_head_rows_up_is_never_read() {
    use super::super::box_head::HEAD_ROWS;
    let (rows, rule) = the_box(fx::BOX_RM_BREAKER_2_1_287);
    let run = |up: u64| {
        let pad = usize::try_from(up).expect("rows") - 8;
        let mut grown = rows[..=rule + 1].to_vec();
        grown.extend(std::iter::repeat_n(
            " Delete temporary directories".to_string(),
            pad,
        ));
        grown.extend(rows[rule + 2..].iter().cloned());
        let at = rule + 8 + pad;
        let mut m = fenced(vec![grown[at..].to_vec(), idle_screen()]);
        m.key_releases = Some(0);
        m.alt_screen = Some(false);
        m.scrollback = Some(grown[..at].to_vec());
        let mut opts = auto(30);
        opts.policy.approve = Approve::All;
        opts.policy.rm_breaker_ask = false;
        let mut s = session(&mut m, None);
        let r = s.supervise(&opts);
        drop(s);
        let lines = m.requests.iter().filter(|r| verb_of(r) == "line").count();
        (r, m, lines)
    };
    let (_, m, lines) = run(HEAD_ROWS);
    assert_eq!(
        lines,
        usize::try_from(HEAD_ROWS).expect("rows"),
        "CONTROL: a head {HEAD_ROWS} rows up is read row by row: {:#?}",
        m.requests
            .iter()
            .filter(|r| verb_of(r) != "line")
            .collect::<Vec<_>>()
    );
    let (r, m, lines) = run(HEAD_ROWS + 1);
    assert_eq!(lines, 0, "a head {} rows up is not read", HEAD_ROWS + 1);
    assert!(
        m.requests.iter().any(|r| verb_of(r) == "search"),
        "PRECONDITION: its rule searched for: {:#?}",
        m.requests
    );
    assert!(m.presses().is_empty(), "{r:?}: {:#?}", m.requests);
    assert!(
        matches!(&r, Ok((out, _)) if out.starts_with("prompt\n")),
        "the owner's: {r:?}"
    );
}

/// A TICKING BOX TALLER THAN THE PANE IS JOINED THROUGH ITS COUNTDOWN'S TICK:
/// the 2.1.301 rm box with the vendor's auto-deny countdown, cut three rows
/// under its rule (its command row in the history), the screen sitting until
/// a key and redrawn a second lower on every read, as Claude Code redraws the
/// countdown every second.
#[test]
fn a_ticking_box_taller_than_the_pane_is_joined_through_its_countdowns_tick() {
    let (rows, rule) = the_box(fx::BOX_RM_AUTO_DENY_2_1_301);
    let at = rule + 3;
    assert!(
        cut_at(&rows, at)
            .prompt
            .is_some_and(|p| p.head_off_screen && p.auto_deny.is_some()),
        "PRECONDITION: the cut is a box whose head is off the screen, its countdown on it"
    );
    let mut m = fenced(vec![rows[at..].to_vec(), idle_screen()]);
    m.key_releases = Some(0);
    m.alt_screen = Some(false);
    m.scrollback = Some(rows[..at].to_vec());
    m.countdown_ticks = true;
    let mut opts = auto(30);
    opts.policy.approve = Approve::All;
    opts.policy.rm_breaker_ask = false;
    let mut s = session(&mut m, None);
    let r = s.supervise(&opts);
    drop(s);
    assert!(
        m.requests.iter().any(|r| verb_of(r) == "line"),
        "PRECONDITION: the head read: {:#?}",
        m.requests
    );
    let presses = m.presses();
    assert_eq!(presses.len(), 1, "{r:?}: {:#?}", m.requests);
    assert!(presses[0].ends_with(" 1"), "{presses:?}");
}
