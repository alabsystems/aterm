// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for built-in occupancy rule template formatting, length bounds, and span rendering.

use super::*;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

#[test]
fn every_line_is_the_mark_a_closed_fact_and_its_rule() {
    let spans = [
        0,
        29 * MIN,
        30 * MIN,
        59 * MIN,
        HOUR,
        5 * HOUR + 6 * MIN,
        i64::MAX,
    ];
    let mut lines = Vec::new();
    for held in spans {
        lines.push(wait_nudge(held));
        lines.push(wait_nudge_slice1(held));
        for (usable, n) in [(1, 1), (1, 3), (2, 3), (u16::MAX, u16::MAX)] {
            for free in [20 * 60_000, u64::MAX] {
                for avail in [8 * 1024, 96 * 1024, u64::MAX] {
                    lines.push(hold_room(held, usable, n, free, avail));
                }
            }
        }
    }
    for class in Declarer::ALL {
        for kinds in [
            &[][..],
            &AtomKind::ALL[..],
            &[AtomKind::File, AtomKind::File][..],
        ] {
            for since in [None, Some((0, 0)), Some((23, 59))] {
                for slots in [None, Some((1, 3)), Some((u16::MAX, u16::MAX))] {
                    lines.push(wake(u16::MAX, class, kinds, since, slots));
                }
            }
        }
    }
    for line in &lines {
        let text = line.text();
        assert!(text.starts_with("[aterm harness] aterm: "), "{text}");
        assert!(text.contains(line.fact()), "{text}");
        assert!(
            text.ends_with(&format!(" (rule {})", line.rule().name())),
            "{text}"
        );
        assert!(line.fact().chars().count() <= FACT_CAP, "{text}");
        assert!(text.chars().count() < LINE_CAP, "{text}");
        assert!(
            text.chars().all(|c| c == ' ' || c.is_ascii_graphic()),
            "printable ASCII only: {text}"
        );
    }
}

#[test]
fn each_rule_says_its_own_numbers() {
    let nudge = wait_nudge(30 * MIN);
    assert_eq!(nudge.rule(), RuleId::WaitNudge);
    assert!(nudge.fact().contains("30 min"), "{}", nudge.text());
    assert!(nudge.text().contains("WAITING:"), "{}", nudge.text());

    let room = hold_room(5 * HOUR + 6 * MIN, 1, 3, 20 * 60_000, 96 * 1024);
    assert_eq!(room.rule(), RuleId::HoldRoom);
    for said in ["5h06m", "1 of 3", "20 min", "96 GiB"] {
        assert!(room.fact().contains(said), "{said} in {}", room.text());
    }
    assert!(!room.text().contains("WAITING:"), "{}", room.text());

    let woke = wake(
        1,
        Declarer::Agent,
        &[AtomKind::File],
        Some((23, 40)),
        Some((1, 3)),
    );
    assert_eq!(woke.rule(), RuleId::Wake);
    assert_eq!(
        woke.text(),
        "[aterm harness] aterm: wake #1 (yours): its file condition holds since 23:40; 1 of 3 \
         build slots free. Re-read the base of any cut, mint or publish before you start it. \
         (rule wake@v1)"
    );
    let plain = wake(
        2,
        Declarer::Agent,
        &[AtomKind::File, AtomKind::Pid, AtomKind::File, AtomKind::At],
        None,
        None,
    );
    assert!(
        plain
            .fact()
            .starts_with("wake #2 (yours): its file, pid and at conditions hold."),
        "each kind once, in order: {}",
        plain.text()
    );
}

/// The `aterm ctl` verbs `text` teaches: the word after `aterm ctl` and its `@<sid>` selector.
fn taught_verbs(text: &str) -> Vec<String> {
    text.match_indices("aterm ctl ")
        .filter_map(|(at, said)| {
            text[at + said.len()..]
                .split_whitespace()
                .map(|w| w.trim_matches('`'))
                .find(|w| !w.starts_with('@'))
                .map(str::to_string)
        })
        .collect()
}

/// The words after `wake` that `sentence`'s backticked `aterm ctl @self wake …` spells, each
/// placeholder filled (`<word>` a label, `<condition>` a time atom).
fn taught_declaration(sentence: &str) -> Option<String> {
    let (_, rest) = sentence.split_once("`aterm ctl @self wake ")?;
    let (words, _) = rest.split_once('`')?;
    Some(words.replace("<word>", "x").replace("<condition>", "in:5m"))
}

/// THE NUDGE TEACHES THE WAKE (the slice-2 plan's 2.10): from slice 2 its line is slice 1's with
/// one sentence more, and that sentence names one `aterm ctl` verb, which the control server
/// serves, spelled as a declaration `wake` reads once its placeholders are filled. Slice 1's line
/// names none. CONTROLS: a verb nothing serves is read as one, and the sentence without its
/// `label=` (as the design's §3.2 first spelled it) is refused for the label.
#[test]
fn the_nudge_names_one_served_verb_that_slice_ones_did_not() {
    use crate::supervise::occupancy::wakes::{WakeReq, WakeUsage};
    for held in [0, 30 * MIN, 5 * HOUR + 6 * MIN, i64::MAX] {
        let (taught, older) = (wait_nudge(held), wait_nudge_slice1(held));
        assert_eq!((taught.rule(), taught.fact()), (older.rule(), older.fact()));
        let (taught, older) = (taught.text(), older.text());
        let (t, o): (Vec<char>, Vec<char>) = (taught.chars().collect(), older.chars().collect());
        let head = o.iter().zip(&t).take_while(|(a, b)| a == b).count();
        let tail = o[head..]
            .iter()
            .rev()
            .zip(t[head..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        assert_eq!(
            head + tail,
            o.len(),
            "slice 1's line, one insertion: {taught}"
        );
        let added: String = t[head..t.len() - tail].iter().collect();
        let added = added.trim();
        assert!(
            added.ends_with('.') && !added.contains(". "),
            "one sentence inserted: {added:?}"
        );
        let verbs = taught_verbs(added);
        assert_eq!(verbs.len(), 1, "{added}");
        assert!(
            aterm_types::control_verbs::spec(&verbs[0]).is_some(),
            "`{}` is served",
            verbs[0]
        );
        assert_eq!(taught_verbs(older), Vec::<String>::new(), "{older}");
        let words = taught_declaration(added).expect("the sentence spells a wake");
        assert!(!words.contains('<'), "every placeholder filled: {words}");
        let read = WakeReq::parse(&words);
        assert!(read.is_ok(), "`wake {words}` is read: {read:?}");
    }
    let unserved = taught_verbs("`aterm ctl @self wakeup when=x` names a verb");
    assert_eq!(unserved, ["wakeup"]);
    assert!(aterm_types::control_verbs::spec(&unserved[0]).is_none());
    let unlabelled = taught_declaration(
        "`aterm ctl @self wake when=<condition>` makes aterm tell you here when it holds.",
    )
    .expect("spelled");
    assert_eq!(WakeReq::parse(&unlabelled).err(), Some(WakeUsage::Label));
}

#[test]
fn spans_read_in_minutes_then_hours() {
    for (ms, want) in [
        (0, "0 min"),
        (-5, "0 min"),
        (59_999, "0 min"),
        (29 * MIN + 59_999, "29 min"),
        (59 * MIN, "59 min"),
        (HOUR, "1h00m"),
        (5 * HOUR + 6 * MIN, "5h06m"),
        (26 * HOUR + 59 * MIN, "26h59m"),
    ] {
        assert_eq!(span(ms), want, "{ms}");
    }
}
