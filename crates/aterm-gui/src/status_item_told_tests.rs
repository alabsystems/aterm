// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A NEED TOLD ONCE (design §14.18): the herald tells its owner of each need of a harness's ask
//! once, by the need's own words; the same need back, re-counted or re-ordered posts nothing,
//! while one raised again after the agent took it down, or after the owner's own turn, is told
//! again. The need keeps standing on every surface meanwhile.

use super::*;
use crate::needs::{AgentFact, AttentionFact, HarnessNeeds, NeedFacts, ToldNeeds};
use aterm_types::needs::AttentionKind;

const A: &str = "grant Screen Recording to com.aterm.aterm in System Settings";
const B: &str = "the release credential to sign 0.48.0";
const C: &str = "a second reviewer for the parser's error paths";

/// The needs entry a loop writes for a set whose first need is `first`, counting `more` past it.
fn entry(first: &str, more: usize) -> String {
    let more = if more == 0 {
        String::new()
    } else {
        format!(" (+{more} more)")
    };
    format!("claude needs you: {first}{more} \u{2014} carrying on")
}

/// A need longer than the needs entry carries: its loop's writer cuts it to the entry's 64 cells
/// (aterm-agent `needs_attention_text`, `clip_to(first, SUBJECT_CELLS)`: 63 cells and `…`).
const LONG: &str = "grant Screen Recording and Accessibility to com.aterm.aterm in System Settings, \
                    then restart aterm";
/// Another need that shares [`LONG`]'s first 63 cells, so the entry carries both in the same words.
const LONG_TOO: &str = "grant Screen Recording and Accessibility to com.aterm.aterm in System \
                        Settings, then sign out and back in";

/// The needs entry the writer makes for a set whose first need is `first`: cut as
/// `needs_attention_text` cuts an ASCII need past 64 cells.
fn cut_entry(first: &str, more: usize) -> String {
    let cut = if first.chars().count() > 64 {
        format!("{}\u{2026}", first.chars().take(63).collect::<String>())
    } else {
        first.to_string()
    };
    entry(&cut, more)
}

/// Session 1's row with `entry` standing and, where `told` names one, the set its loop told the
/// window with that entry; `turn` the owner's last plain Enter.
fn ask_row(entry: &str, told: Option<&[&str]>, turn: u64) -> SessionRow {
    let mut held = ToldNeeds::default();
    if let Some(needs) = told {
        let needs: Vec<String> = needs.iter().map(|n| (*n).to_string()).collect();
        held.tell(
            HarnessNeeds::told(&needs, entry).map(std::sync::Arc::new),
            Some(entry),
        );
    }
    let facts = NeedFacts {
        attention: vec![AttentionFact {
            owner: crate::harness_host::NEEDS_ATTENTION_OWNER.to_string(),
            text: entry.to_string(),
            kind: Some(AttentionKind::Ask),
        }],
        told: held,
        ..NeedFacts::default()
    };
    SessionRow {
        id: 1,
        title: "worker".to_string(),
        need: crate::needs::of(&facts).need,
        owner_turn: turn,
        ..SessionRow::default()
    }
}

/// The row whose loop told `needs` with the entry it writes for them.
fn asks(needs: &[&str]) -> SessionRow {
    asks_at(needs, 0)
}

fn asks_at(needs: &[&str], turn: u64) -> SessionRow {
    ask_row(&entry(needs[0], needs.len() - 1), Some(needs), turn)
}

/// Session 1 with an approval box in front, which hides the ask (a need of another kind).
fn boxed() -> SessionRow {
    let facts = NeedFacts {
        agent: Some(AgentFact {
            word: "prompt",
            detail: Some("bash:not-read-only".to_string()),
            rev: 9,
            subject: Some("rm -rf build".to_string()),
        }),
        ..NeedFacts::default()
    };
    SessionRow {
        id: 1,
        title: "worker".to_string(),
        need: crate::needs::of(&facts).need,
        ..SessionRow::default()
    }
}

/// The `k`th instant a step apart, past every limit.
fn at(t0: std::time::Instant, k: u32) -> std::time::Instant {
    t0 + NOTIFY_BURST_WINDOW * k
}

fn note(
    h: &mut Herald,
    row: Option<&SessionRow>,
    alerts: bool,
    now: std::time::Instant,
) -> HeraldOutcome {
    let esc = row.and_then(escalation);
    h.note(1, esc.as_ref(), false, alerts, now)
}

fn body(out: &HeraldOutcome) -> Option<&str> {
    out.notice.as_ref().map(|n| n.body.as_str())
}

#[test]
fn a_need_that_comes_back_is_told_once() {
    let t0 = std::time::Instant::now();
    let run = |told_off: bool, turn: u64| {
        let mut h = Herald {
            told_off,
            ..Herald::default()
        };
        let first = note(&mut h, Some(&asks(&[A])), true, at(t0, 0));
        assert_eq!(body(&first), Some(format!("worker: {A}").as_str()));
        assert!(
            note(&mut h, Some(&boxed()), true, at(t0, 1))
                .notice
                .is_some()
        );
        let back = note(&mut h, Some(&asks_at(&[A], turn)), true, at(t0, 2));
        (back, h.last_quiet())
    };
    let (back, quiet) = run(false, 0);
    assert!(back.transition && back.notice.is_none(), "{back:?}");
    assert_eq!((back.owed, quiet), (0, Some(HeraldQuiet::ToldOnce)));
    // Negative control: the owner's own turn in the tab lets it be told again.
    let (back, quiet) = run(false, 1);
    assert_eq!(body(&back), Some(format!("worker: {A}").as_str()));
    assert_eq!(quiet, None);
    // Mutant: the herald with no told memory posts it again.
    let (back, _) = run(true, 0);
    assert!(back.notice.is_some(), "the memory off posts it again");
}

#[test]
fn a_need_taken_down_and_raised_again_is_told_again() {
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    assert!(
        note(&mut h, Some(&asks(&[A])), true, at(t0, 0))
            .notice
            .is_some()
    );
    // The agent's take-down (`NEEDS YOU: nothing`): no need stands.
    assert!(note(&mut h, None, true, at(t0, 1)).notice.is_none());
    let again = note(&mut h, Some(&asks(&[A])), true, at(t0, 2));
    assert_eq!(body(&again), Some(format!("worker: {A}").as_str()));
}

#[test]
fn a_new_need_beside_a_told_one_names_the_new_one() {
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    assert!(
        note(&mut h, Some(&asks(&[A])), true, at(t0, 0))
            .notice
            .is_some()
    );
    let both = note(&mut h, Some(&asks(&[A, B])), true, at(t0, 1));
    assert_eq!(body(&both), Some(format!("worker: {B} (+1 more)").as_str()));
    // A set that loses a need is not news.
    let lost = note(&mut h, Some(&asks(&[A])), true, at(t0, 2));
    assert!(lost.transition && lost.notice.is_none(), "{lost:?}");
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::ToldOnce));
    // Mutant: one id for the whole set posts the lost set again.
    let mut h = Herald {
        told_off: true,
        ..Herald::default()
    };
    for (k, set) in [&[A][..], &[A, B], &[A]].into_iter().enumerate() {
        let out = note(&mut h, Some(&asks(set)), true, at(t0, k as u32));
        assert!(out.notice.is_some(), "the memory off posts {set:?}");
    }
}

#[test]
fn a_recount_or_reorder_is_not_news() {
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    assert!(
        note(&mut h, Some(&asks(&[A, B])), true, at(t0, 0))
            .notice
            .is_some()
    );
    let reordered = note(&mut h, Some(&asks(&[B, A])), true, at(t0, 1));
    assert!(
        reordered.transition && reordered.notice.is_none(),
        "{reordered:?}"
    );
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::ToldOnce));
    // The same entry read without its set (the told set gone): its key was told with it.
    let alone = ask_row(&entry(B, 1), None, 0);
    let recount = note(&mut h, Some(&alone), true, at(t0, 2));
    assert!(recount.notice.is_none(), "{recount:?}");
}

#[test]
fn a_look_tells_the_need() {
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    let esc = escalation(&asks(&[A]));
    let looked = h.note(1, esc.as_ref(), true, true, at(t0, 0));
    assert!(looked.notice.is_none());
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::Looking));
    assert!(
        note(&mut h, Some(&boxed()), true, at(t0, 1))
            .notice
            .is_some()
    );
    let back = note(&mut h, Some(&asks(&[A])), true, at(t0, 2));
    assert!(back.notice.is_none(), "{back:?}");
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::ToldOnce));
}

#[test]
fn told_once_is_decided_with_alerts_off() {
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    assert!(
        note(&mut h, Some(&asks(&[A])), false, at(t0, 0))
            .notice
            .is_none()
    );
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::Disabled));
    let _ = note(&mut h, Some(&asks(&[A, B])), false, at(t0, 1));
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::Disabled));
    let back = note(&mut h, Some(&asks(&[A])), false, at(t0, 2));
    assert!(back.transition);
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::ToldOnce));
}

#[test]
fn turning_desktop_alerts_on_forgets_what_alerts_off_marked() {
    let mut app = crate::App::headless_for_test();
    app.config.desktop_alerts = Some(false);
    app.apply_desktop_alerts(false);
    let t0 = std::time::Instant::now();
    let h = &mut app.presence.herald;
    assert!(
        note(h, Some(&asks(&[A])), false, at(t0, 0))
            .notice
            .is_none()
    );
    app.config.desktop_alerts = Some(true);
    app.apply_desktop_alerts(true);
    let h = &mut app.presence.herald;
    let same = note(h, Some(&asks(&[A])), true, at(t0, 1));
    assert!(
        same.notice.is_none(),
        "turning alerts on replays nothing standing: {same:?}"
    );
    assert!(note(h, Some(&boxed()), true, at(t0, 2)).notice.is_some());
    let back = note(h, Some(&asks(&[A])), true, at(t0, 3));
    assert_eq!(
        body(&back),
        Some(format!("worker: {A}").as_str()),
        "never bannered"
    );
    // Negative control: a config reload with alerts already on forgets nothing.
    app.apply_desktop_alerts(true);
    let h = &mut app.presence.herald;
    assert!(note(h, Some(&boxed()), true, at(t0, 4)).notice.is_some());
    let told = note(h, Some(&asks(&[A])), true, at(t0, 5));
    assert!(told.notice.is_none(), "{told:?}");
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::ToldOnce));
}

#[test]
fn a_refused_told_once_notice_is_retried() {
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    assert!(
        note(&mut h, Some(&asks(&[A])), true, at(t0, 0))
            .notice
            .is_some()
    );
    let both = note(&mut h, Some(&asks(&[A, B])), true, at(t0, 1));
    assert_eq!(body(&both), Some(format!("worker: {B} (+1 more)").as_str()));
    h.refused(1, both.owed, at(t0, 1));
    let retry = at(t0, 1) + NOTIFY_RETRY;
    assert_eq!(h.due(retry).0, vec![1]);
    let again = note(&mut h, Some(&asks(&[A, B])), true, retry);
    assert!(!again.transition);
    assert_eq!(
        body(&again),
        Some(format!("worker: {B} (+1 more)").as_str()),
        "the retry names the need the refused notice named"
    );
    assert_eq!(h.due(retry), (vec![], None));
}

#[test]
fn a_need_swapped_under_the_same_entry_is_told() {
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    let first = entry(A, 1);
    let ab = ask_row(&first, Some(&[A, B]), 0);
    assert!(note(&mut h, Some(&ab), true, at(t0, 0)).notice.is_some());
    let ac = ask_row(&first, Some(&[A, C]), 0);
    assert_eq!(
        escalation(&ab).map(|e| e.key),
        escalation(&ac).map(|e| e.key)
    );
    let swapped = note(&mut h, Some(&ac), true, at(t0, 1));
    assert!(swapped.transition, "{swapped:?}");
    assert_eq!(
        body(&swapped),
        Some(format!("worker: {C} (+1 more)").as_str())
    );
    assert!(note(&mut h, Some(&ac), true, at(t0, 2)).notice.is_none());
}

#[test]
fn a_told_set_read_after_its_entry_tells_the_new_need() {
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    assert!(
        note(&mut h, Some(&asks(&[A])), true, at(t0, 0))
            .notice
            .is_some()
    );
    let first = entry(A, 1);
    let alone = note(&mut h, Some(&ask_row(&first, None, 0)), true, at(t0, 1));
    assert_eq!(
        body(&alone),
        Some(format!("worker: {A} (+1 more)").as_str()),
        "the entry counts a need the window has no words for"
    );
    let told = note(
        &mut h,
        Some(&ask_row(&first, Some(&[A, B]), 0)),
        true,
        at(t0, 2),
    );
    assert_eq!(body(&told), Some(format!("worker: {B} (+1 more)").as_str()));
    assert!(
        note(
            &mut h,
            Some(&ask_row(&first, Some(&[A, B]), 0)),
            true,
            at(t0, 3)
        )
        .notice
        .is_none(),
        "told once"
    );
}

#[test]
fn escalation_names_each_need_by_its_words() {
    use crate::needs::need_id;
    let ids = |row: &SessionRow| -> Vec<u64> {
        escalation(row)
            .map(|e| e.needs.iter().map(|n| n.id).collect())
            .unwrap_or_default()
    };
    assert_eq!(ids(&asks(&[A, B])), [need_id(A), need_id(B)]);
    let counted = ask_row(&entry(A, 2), None, 0);
    let key = counted.need.as_ref().map(|n| n.key).unwrap_or_default();
    assert_eq!(ids(&counted), [need_id(A), key]);
    let bodies: Vec<String> = escalation(&asks(&[A, B]))
        .map(|e| e.needs.into_iter().map(|n| n.body).collect())
        .unwrap_or_default();
    assert_eq!(
        bodies,
        [
            format!("worker: {A} (+1 more)"),
            format!("worker: {B} (+1 more)")
        ]
    );
    // A box, a wall and a stall keep one notice per transition: they stand for no need id.
    let wall = SessionRow {
        id: 1,
        title: "worker".to_string(),
        need: crate::needs::of(&NeedFacts {
            agent: Some(AgentFact {
                word: "wall:login",
                detail: None,
                rev: 3,
                subject: None,
            }),
            ..NeedFacts::default()
        })
        .need,
        ..SessionRow::default()
    };
    let stall = SessionRow {
        id: 1,
        title: "worker".to_string(),
        need: crate::needs::of(&NeedFacts {
            stall: Some(crate::needs::StallFact {
                text: "claude is frozen: not reading input".to_string(),
                key: 0xbeef,
                stopped: false,
                act: true,
                hand_back_frees: false,
            }),
            ..NeedFacts::default()
        })
        .need,
        ..SessionRow::default()
    };
    for row in [boxed(), wall, stall] {
        assert!(row.need.is_some());
        assert_eq!(ids(&row), Vec::<u64>::new(), "{:?}", row.need);
    }
    // An ask that is not a harness's stands for its key.
    let plain = SessionRow {
        id: 1,
        title: "zsh".to_string(),
        need: crate::needs::of(&NeedFacts {
            attention: vec![AttentionFact {
                owner: "-".to_string(),
                text: "deploy needs a human".to_string(),
                kind: Some(AttentionKind::Ask),
            }],
            ..NeedFacts::default()
        })
        .need,
        ..SessionRow::default()
    };
    let key = plain.need.as_ref().map(|n| n.key).unwrap_or_default();
    assert_eq!(ids(&plain), [key]);
}

#[test]
fn told_once_is_one_log_line_without_its_text() {
    use aterm_log::Level::{Debug, Info};
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    let line = |h: &Herald, row: &SessionRow, out: &HeraldOutcome| {
        herald_log_line(
            1,
            escalation(row).as_ref(),
            out,
            h.last_quiet(),
            HeraldPost::Queued,
        )
    };
    let _ = note(&mut h, Some(&asks(&[A, B])), true, at(t0, 0));
    let back = note(&mut h, Some(&asks(&[B, A])), true, at(t0, 1));
    let (level, said) = line(&h, &asks(&[B, A]), &back).expect("a line");
    assert_eq!(level, Info, "the first quiet decision since the post");
    assert!(said.ends_with(": not queued: told once"), "{said}");
    for secret in [A, B, "worker"] {
        assert!(!said.contains(secret), "{said}");
    }
    let again = note(&mut h, Some(&asks(&[A, B])), true, at(t0, 1));
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::ToldOnce));
    assert_eq!(
        line(&h, &asks(&[A, B]), &again).map(|(l, _)| l),
        Some(Debug)
    );
    // A slot's quiet decisions speak at Info again once a floor has passed since the last one
    // that did, with no post between (`HeraldSlot::first_quiet`).
    let later = at(t0, 1) + NOTIFY_SESSION_FLOOR;
    let floor_on = note(&mut h, Some(&asks(&[B, A])), true, later);
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::ToldOnce));
    assert_eq!(
        line(&h, &asks(&[B, A]), &floor_on).map(|(l, _)| l),
        Some(Info)
    );
}

/// THE LIVE JOURNAL'S NEEDS, TOLD ONCE (§14.18's holdout): this Mac's lead journal's `NEEDS`
/// and needs `CLEARED` rows from 10-14 to 10-29, each row's first need reduced to a label and
/// its count kept, no words (`fixtures/needs-told-holdout.tsv`). Read as the entry alone (the
/// row carries no told set), the herald before told once posts each raise, and told once posts
/// each distinct need once until the take-down.
#[test]
fn the_live_journal_holdout_tells_each_need_once() {
    let fixture = include_str!("fixtures/needs-told-holdout.tsv");
    let t0 = std::time::Instant::now();
    let run = |told_off: bool| {
        let mut h = Herald {
            told_off,
            ..Herald::default()
        };
        let mut posted: Vec<String> = Vec::new();
        for (k, row) in fixture.lines().filter(|l| !l.starts_with('#')).enumerate() {
            let cols: Vec<&str> = row.split('\t').collect();
            let now = at(t0, u32::try_from(k).unwrap_or(u32::MAX));
            let out = match cols[..] {
                ["NEEDS", label, count] => {
                    let more = count.parse::<usize>().unwrap_or(1).saturating_sub(1);
                    note(
                        &mut h,
                        Some(&ask_row(&entry(label, more), None, 0)),
                        true,
                        now,
                    )
                }
                ["CLEARED"] => note(&mut h, None, true, now),
                _ => panic!("a row: {row:?}"),
            };
            posted.extend(out.notice.map(|n| n.body));
        }
        posted
    };
    let before = run(true);
    let told = run(false);
    let raises = fixture.lines().filter(|l| l.starts_with("NEEDS")).count();
    assert_eq!(before.len(), raises, "{before:?}");
    let distinct = |posts: &[String]| -> std::collections::BTreeSet<String> {
        posts.iter().cloned().collect()
    };
    assert_eq!(told.len(), 9, "{told:?}");
    assert_eq!(
        distinct(&told).len(),
        9,
        "each need once until the take-down: {told:?}"
    );
    assert_eq!(
        told.last().map(String::as_str),
        Some("worker: X (+3 more)"),
        "a need the entry counts past the one told is news: {told:?}"
    );
    let a_prime = before
        .iter()
        .filter(|b| b.starts_with("worker: A' "))
        .count();
    assert_eq!(a_prime, 6, "{before:?}");
    assert_eq!(
        told.iter().filter(|b| b.starts_with("worker: A' ")).count(),
        1,
        "{told:?}"
    );
}

/// A seamless update adopted a told ask (`Herald::adopt`, the carried `told`): its needs are told
/// in the new process too, so neither the standing set nor a need of it coming back is bannered.
/// Control: the same carry untold posts, as main's adoption does.
#[test]
fn an_adopted_told_ask_is_not_told_again() {
    let t0 = std::time::Instant::now();
    let row = asks(&[A, B]);
    let word = row.need.as_ref().map(|n| n.word);
    let carry = |told: bool| StatusCarry {
        need: word.map(|w| (w, 100)),
        told,
        ..StatusCarry::default()
    };
    let successor = |carry: StatusCarry| {
        let mut h = Herald::default();
        h.adopt(1, carry, 1_000, t0);
        let _ = h.observe(1, Some(&row), 1_010);
        let out = note(&mut h, Some(&row), true, t0);
        (h, out)
    };
    let (mut h, adopted) = successor(carry(true));
    assert!(
        adopted.notice.is_none() && !adopted.transition,
        "{adopted:?}"
    );
    let _ = note(&mut h, Some(&asks(&[A])), true, at(t0, 1));
    let back = note(&mut h, Some(&row), true, at(t0, 2));
    assert!(
        back.notice.is_none(),
        "an adopted need back is told: {back:?}"
    );
    assert!(successor(carry(false)).1.notice.is_some(), "untold: owed");
}

/// THE OWNER'S TURN IS THEIR LAST PLAIN ENTER IN THE TAB (§14.18): `App::status_session_row` reads
/// it from the tab's person stamp (`HumanInputStamp::last_plain_enter`), so typing keeps the turn
/// and only a plain Enter starts a new one, which lets a need they were told of be told again.
#[test]
fn the_rows_owner_turn_is_the_last_plain_enter() {
    use crate::human_input::Gesture;
    let app = crate::App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let handle = app
        .store
        .read()
        .unwrap()
        .by_local(0)
        .cloned()
        .expect("session 0");
    let turn = || app.status_session_row(&handle).owner_turn;
    assert_eq!(turn(), 0, "no Enter yet");
    let _ = ctx.human_input.note(1_000, Gesture::Typed);
    assert_eq!(turn(), 0, "typing is not a turn");
    let _ = ctx.human_input.note(2_000, Gesture::PlainEnter);
    let first = turn();
    assert_ne!(first, 0, "a plain Enter is a turn");
    let _ = ctx.human_input.note(3_000, Gesture::Typed);
    assert_eq!(turn(), first, "typing after it keeps the turn");
    let _ = ctx.human_input.note(4_000, Gesture::PlainEnter);
    assert!(turn() > first, "the next plain Enter is the next turn");
}

/// THE MEMORY IS BOUNDED (§14.18): a slot keeps exactly [`TOLD_MAX`] told ids, the oldest dropped
/// first, so the first id goes with the one past the bound, and the second stays.
#[test]
fn the_told_memory_keeps_exactly_its_bound() {
    let bound = u64::try_from(TOLD_MAX).unwrap_or(u64::MAX);
    let mut told = Told::default();
    assert_eq!(told.mark(1..=bound).len(), TOLD_MAX, "each id is new");
    assert!(
        (1..=bound).all(|id| told.has(id)),
        "the bound's worth stays"
    );
    assert_eq!(told.mark([bound + 1]), [bound + 1]);
    assert!(!told.has(1), "the oldest went with the one past the bound");
    assert!(told.has(2) && told.has(bound + 1));
    assert_eq!(told.ids.len(), TOLD_MAX);
}

/// A NEED THE ENTRY CUTS IS ONE NEED, READ CUT OR WHOLE (§14.18.1): the entry carries a first need
/// past its cells cut ([`cut_entry`]), its told set whole, and a row reads whichever it has, so
/// the entry read alone and then with its set, or the other way round, is one banner, as `main`'s
/// one notice per transition gives under the entry's one key. Two needs that share only the cut
/// stay two when read whole.
#[test]
fn a_need_the_entry_cuts_is_one_need_read_cut_or_whole() {
    let t0 = std::time::Instant::now();
    let cut = cut_entry(LONG, 0);
    assert_ne!(cut, entry(LONG, 0), "the entry cuts it");
    let alone = ask_row(&cut, None, 0);
    let whole = ask_row(&cut, Some(&[LONG]), 0);
    let reads = |row: &SessionRow| -> Vec<NeedRead> {
        escalation(row)
            .map(|e| e.needs.iter().map(|n| n.read).collect())
            .unwrap_or_default()
    };
    assert_eq!(reads(&alone), [NeedRead::Entry]);
    let entry_words = format!("{}\u{2026}", LONG.chars().take(63).collect::<String>());
    assert_eq!(
        reads(&whole),
        [NeedRead::Cut(crate::needs::need_id(&entry_words))]
    );
    assert_eq!(
        reads(&asks(&[A])),
        [NeedRead::Whole],
        "an entry that cuts nothing"
    );
    for (first, then) in [(&alone, &whole), (&whole, &alone)] {
        let mut h = Herald::default();
        assert!(note(&mut h, Some(first), true, at(t0, 0)).notice.is_some());
        let again = note(&mut h, Some(then), true, at(t0, 1));
        assert!(
            again.notice.is_none() && !again.transition,
            "{:?} then {:?}: {again:?}",
            reads(first),
            reads(then)
        );
    }
    // Two needs the entry carries in the same words, each read whole: the second is news.
    let mut h = Herald::default();
    assert!(note(&mut h, Some(&whole), true, at(t0, 0)).notice.is_some());
    let other = ask_row(&cut_entry(LONG_TOO, 0), Some(&[LONG_TOO]), 0);
    assert_eq!(
        escalation(&other).map(|e| e.key),
        escalation(&whole).map(|e| e.key)
    );
    let news = note(&mut h, Some(&other), true, at(t0, 1));
    assert_eq!(body(&news), Some(format!("worker: {LONG_TOO}").as_str()));
}

/// A seamless successor that adopts a told ask reads its entry alone until its loop tells the set
/// again (the timeline's set is not carried): the set the outgoing process told names the needs
/// that entry only counts, so the first read that names them under the adopted key is told. Past
/// that read, a need swapped in under the same key is news again.
#[test]
fn an_adopted_ask_read_alone_is_told_when_its_set_names_it() {
    let t0 = std::time::Instant::now();
    let first = entry(A, 1);
    let alone = ask_row(&first, None, 0);
    let named = ask_row(&first, Some(&[A, B]), 0);
    let mut h = Herald::default();
    h.adopt(
        1,
        StatusCarry {
            need: alone.need.as_ref().map(|n| (n.word, 100)),
            told: true,
            ..StatusCarry::default()
        },
        1_000,
        t0,
    );
    let _ = h.observe(1, Some(&alone), 1_010);
    let adopted = note(&mut h, Some(&alone), true, t0);
    assert!(
        adopted.notice.is_none() && !adopted.transition,
        "{adopted:?}"
    );
    let told = note(&mut h, Some(&named), true, at(t0, 1));
    assert!(
        told.notice.is_none() && !told.transition,
        "the outgoing process told the set: {told:?}"
    );
    let swapped = note(
        &mut h,
        Some(&ask_row(&first, Some(&[A, C]), 0)),
        true,
        at(t0, 2),
    );
    assert_eq!(
        body(&swapped),
        Some(format!("worker: {C} (+1 more)").as_str())
    );
}

/// A told-once decision owes nothing (§14.18): a set held by the rate limit, then a set whose
/// every need was told, leaves no notice for the timer to post.
#[test]
fn told_once_owes_nothing() {
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    assert!(note(&mut h, Some(&asks(&[A])), true, t0).notice.is_some());
    let held = note(
        &mut h,
        Some(&asks(&[A, B])),
        true,
        t0 + std::time::Duration::from_secs(1),
    );
    assert_eq!((held.owed, h.last_quiet()), (1, Some(HeraldQuiet::Limited)));
    let lost = note(
        &mut h,
        Some(&asks(&[A])),
        true,
        t0 + std::time::Duration::from_secs(2),
    );
    assert_eq!(
        (lost.owed, h.last_quiet()),
        (0, Some(HeraldQuiet::ToldOnce))
    );
    assert_eq!(h.due(at(t0, 1)).0, Vec::<u64>::new(), "nothing owed");
}

/// A told-once decision cancels a refused notice's retry (§14.18): a new need after it posts at
/// once, not when the retry falls due.
#[test]
fn told_once_cancels_a_pending_retry() {
    let t0 = std::time::Instant::now();
    let mut h = Herald::default();
    assert!(
        note(&mut h, Some(&asks(&[A])), true, at(t0, 0))
            .notice
            .is_some()
    );
    let both = note(&mut h, Some(&asks(&[A, B])), true, at(t0, 1));
    assert!(both.notice.is_some());
    h.refused(1, both.owed, at(t0, 1));
    let step = std::time::Duration::from_secs(1);
    let _ = note(&mut h, Some(&asks(&[A])), true, at(t0, 1) + step);
    assert_eq!(h.last_quiet(), Some(HeraldQuiet::ToldOnce));
    let c = note(&mut h, Some(&asks(&[A, C])), true, at(t0, 1) + step * 2);
    assert!(at(t0, 1) + step * 2 < at(t0, 1) + NOTIFY_RETRY);
    assert_eq!(body(&c), Some(format!("worker: {C} (+1 more)").as_str()));
}

/// A NEED TOLD BEFORE A RESTART IS TOLD ONCE ACROSS IT (§14.18, ATERM-KI-1105-5): the told ids
/// ride a seamless update's status row (`told_ids=`) and a quit's restore leaf, and the next
/// process keeps them as its memory. So a need told before the update and not standing then
/// stays quiet when it comes back, a quit's gap (nothing standing yet) forgets nothing, and the
/// first need noted after it is judged by them. CONTROLS: the owner's plain Enter in the new
/// process before its first need, and a take-down after it, each forget them, as within one
/// process. Mutants: an adoption that seeds no ids, or a gap note that forgets them, posts a
/// told need again; one that keeps them past the owner's turn posts nothing for them.
#[test]
fn a_need_told_before_a_restart_is_told_once_across_it() {
    let t0 = std::time::Instant::now();
    let mut before = Herald::default();
    assert!(
        note(&mut before, Some(&asks(&[A])), true, at(t0, 0))
            .notice
            .is_some()
    );
    // A is told, then a set of B alone stands at the update: A is told but no longer standing.
    assert!(
        note(&mut before, Some(&asks(&[B])), true, at(t0, 1))
            .notice
            .is_some()
    );
    let row = before.carry_of(1, 0).row().expect("a row");
    assert!(row.contains("told_ids="), "{row}");
    let carried = StatusCarry::parse(&row);
    assert_eq!(carried.told_ids, before.told_ids(1, 0));
    let left = told_ids_row(&before.told_ids(1, 0)).expect("a leaf's row");
    assert_eq!(parse_told_ids(&left), before.told_ids(1, 0));

    // A seamless update: B stands, then A comes back.
    let mut update = Herald::default();
    update.adopt(1, carried.clone(), 1_000, at(t0, 2));
    let standing = note(&mut update, Some(&asks(&[B])), true, at(t0, 2));
    assert!(standing.notice.is_none(), "{standing:?}");
    let back = note(&mut update, Some(&asks(&[A])), true, at(t0, 3));
    assert!(back.notice.is_none(), "told before the update: {back:?}");
    assert_eq!(update.last_quiet(), Some(HeraldQuiet::ToldOnce));
    let c = note(&mut update, Some(&asks(&[A, C])), true, at(t0, 4));
    assert_eq!(body(&c), Some(format!("worker: {C} (+1 more)").as_str()));
    // CONTROL: the owner's plain Enter in the successor before its first need forgets what was
    // carried, though the standing need the update told is still adopted as told.
    let mut turned = Herald::default();
    turned.adopt(
        1,
        StatusCarry {
            need: Some((NeedWord::Ask, 100)),
            told: true,
            ..carried.clone()
        },
        1_000,
        at(t0, 5),
    );
    let adopted = note(&mut turned, Some(&asks_at(&[B], 1)), true, at(t0, 5));
    assert!(adopted.notice.is_none(), "the update told B: {adopted:?}");
    let a = note(&mut turned, Some(&asks_at(&[A], 1)), true, at(t0, 6));
    assert_eq!(
        body(&a),
        Some(format!("worker: {A}").as_str()),
        "their turn forgot A"
    );

    // A quit and a cold restore: the leaf's ids alone, a gap with nothing standing, then A.
    let cold = |turn: u64, gap: bool| {
        let mut h = Herald::default();
        h.adopt(
            1,
            StatusCarry {
                told_ids: parse_told_ids(&left),
                ..StatusCarry::default()
            },
            1_000,
            at(t0, 2),
        );
        if gap {
            let _ = note(&mut h, None, true, at(t0, 2));
        }
        let first = note(&mut h, Some(&asks_at(&[A, B], turn)), true, at(t0, 3));
        (h, first)
    };
    let (mut h, first) = cold(0, true);
    assert!(first.notice.is_none(), "told before the quit: {first:?}");
    // Once a need was noted here, a take-down forgets as within one process.
    let _ = note(&mut h, None, true, at(t0, 4));
    let again = note(&mut h, Some(&asks(&[A])), true, at(t0, 5));
    assert_eq!(body(&again), Some(format!("worker: {A}").as_str()));
    // CONTROL: the owner's plain Enter in the new process before its first need.
    let (_, turned) = cold(1, true);
    assert!(turned.notice.is_some(), "their turn forgets: {turned:?}");
    // CONTROL: no carry, no memory.
    let mut fresh = Herald::default();
    assert!(
        note(&mut fresh, Some(&asks(&[A, B])), true, at(t0, 3))
            .notice
            .is_some()
    );
}

/// AN OWNER'S TURN THE HERALD HAS NOT NOTED YET IS NOT CARRIED AWAY (§14.18, 2026-11-05 review):
/// a keypress re-notes no row, so the memory of a need told before the owner's plain Enter stands
/// in the slot until the next note forgets it. What a restart carries (`told_ids`, `carry_of`) is
/// the memory at the tab's turn now: none once the owner has had a turn since, so a need told
/// before that Enter is news again after a restart, as it would have been in the same process.
/// CONTROL: at the turn the memory was kept under, the ids ride the restart and the need stays
/// quiet. Mutant: `told_ids` ignoring the turn carries the memory, and the need is quiet.
#[test]
fn an_owner_turn_not_yet_noted_is_not_carried_across_a_restart() {
    let t0 = std::time::Instant::now();
    let mut before = Herald::default();
    assert!(
        note(&mut before, Some(&asks(&[A])), true, at(t0, 0))
            .notice
            .is_some()
    );
    let told = before.told_ids(1, 0);
    assert!(!told.is_empty(), "PRECONDITION: A told at turn 0");
    let restart = |turn: u64| {
        let carry = StatusCarry::parse(&before.carry_of(1, turn).row().unwrap_or_default());
        let mut after = Herald::default();
        after.adopt(1, carry, 1_000, at(t0, 1));
        let _ = note(&mut after, None, true, at(t0, 1));
        note(&mut after, Some(&asks(&[A])), true, at(t0, 2))
    };
    // The owner pressed Enter (turn 1) after A was told; nothing re-noted the row before the quit.
    assert!(before.told_ids(1, 1).is_empty(), "their turn spent it");
    assert!(before.carry_of(1, 1).told_ids.is_empty());
    let news = restart(1);
    assert_eq!(
        body(&news),
        Some(format!("worker: {A}").as_str()),
        "news again after their turn: {news:?}"
    );
    // CONTROL: no turn since.
    assert_eq!(before.carry_of(1, 0).told_ids, told);
    let quiet = restart(0);
    assert!(quiet.notice.is_none(), "told once across it: {quiet:?}");
}

/// TOLD ONCE ACROSS THE FLEET (design §14.18): a content survey row names its ask's needs as this
/// Mac's herald reads them (`need_ids=`, [`need_ids_row`]) and a fleet peer's herald reads them
/// back (`fleet_glance::escalation`), so there each need is told once too: the peer's set
/// re-ordered or cut posts nothing, a need it adds posts, and a first need read from the entry
/// alone is the one read whole beside the entry's cut later. CONTROL: a facts row, and an older
/// peer's row with no `need_ids=`, keep one notice per appearance of the word, so the added need
/// posts nothing there.
#[test]
fn a_fleet_peers_ask_is_told_once_by_the_need_ids_its_row_names() {
    let t0 = std::time::Instant::now();
    // `row`'s need on the wire, as the peer's survey writes it at `level` (`survey::session_row`)
    // and this Mac's glance reads it; `ids` false is an older peer's row, which names none.
    let fleet = |row: &SessionRow, level: &str, ids: bool| -> Escalation {
        let need = row.need.as_ref().expect("an ask");
        let attention = match level {
            "content" => need.what.clone(),
            _ => "1".to_string(),
        };
        let named = need_ids_row(need).filter(|_| ids && level == "content");
        let need_ids = named.map_or(String::new(), |ids| format!(",\"need_ids\":\"{ids}\""));
        let json = format!(
            "[{{\"label\":\"mac-1\",\"fleet\":true,\"instance\":{{\"pid\":\"77\",\"host\":\
             \"mac.local\",\"sessions\":\"1\",\"level\":\"{level}\",\"cut\":\"0\",\"node\":\
             \"n1\"}},\"sessions\":[{{\"sid\":\"s-a\",\"state\":\"alive\",\"program\":\"claude\",\
             \"program_kind\":\"agent\",\"agent\":\"idle\",\"needs\":\"ask\",\"needs_by\":\
             \"harness\",\"attention\":\"{attention}\"{need_ids}}}]}}]"
        );
        let mac = crate::fleet_glance::parse(&json, None, 1_000)
            .expect("a survey")
            .macs
            .remove(0);
        crate::fleet_glance::escalation(&mac, &mac.needs[0])
    };
    let posts = |level: &str, ids: bool| -> Vec<bool> {
        let mut h = Herald::default();
        let cut = cut_entry(LONG, 0);
        [
            asks(&[A, B]),
            asks(&[B, A]),
            asks(&[A]),
            asks(&[A, C]),
            ask_row(&cut, None, 0),
            ask_row(&cut, Some(&[LONG]), 0),
        ]
        .iter()
        .enumerate()
        .map(|(k, row)| {
            let esc = fleet(row, level, ids);
            let mut k = k as u32;
            // The agent takes its set down before the cut need: a new episode.
            if k >= 4 {
                if k == 4 {
                    let _ = h.note(esc.session, None, false, true, at(t0, k));
                }
                k += 1;
            }
            h.note(esc.session, Some(&esc), false, true, at(t0, k))
                .notice
                .is_some()
        })
        .collect()
    };
    assert_eq!(
        posts("content", true),
        [true, false, false, true, true, false],
        "each need told once"
    );
    assert_eq!(
        posts("facts", true),
        [true, false, false, false, true, false],
        "CONTROL: a facts row names no needs"
    );
    assert_eq!(
        posts("content", false),
        [true, false, false, false, true, false],
        "CONTROL: an older peer's row names none"
    );
    // The wire keeps how each need was read: the entry alone, then whole beside its cut.
    let cut = cut_entry(LONG, 0);
    let reads = |row: &SessionRow| -> Vec<NeedRead> {
        fleet(row, "content", true)
            .needs
            .iter()
            .map(|n| n.read)
            .collect()
    };
    let entry_words = format!("{}\u{2026}", LONG.chars().take(63).collect::<String>());
    assert_eq!(reads(&ask_row(&cut, None, 0)), [NeedRead::Entry]);
    assert_eq!(
        reads(&ask_row(&cut, Some(&[LONG]), 0)),
        [NeedRead::Cut(crate::needs::need_id(&entry_words))]
    );
}

/// `need_ids=` reads back as it was written, each need's read with it, and a part that does not
/// read is dropped; past [`NEED_IDS_MAX`] the rest are not read.
#[test]
fn a_need_ids_row_reads_back_and_drops_what_does_not_read() {
    assert_eq!(
        parse_need_ids("1f,00000000000000a1/b2,/c3,,0,zz,/0,d4/,1/zz,00000000000000001"),
        [
            (0x1f, NeedRead::Whole),
            (0xa1, NeedRead::Cut(0xb2)),
            (0xc3, NeedRead::Entry),
        ]
    );
    let many: Vec<String> = (1..=40).map(|id| format!("{id:x}")).collect();
    let read = parse_need_ids(&many.join(","));
    assert_eq!(read.len(), NEED_IDS_MAX);
    assert_eq!(read.last(), Some(&(NEED_IDS_MAX as u64, NeedRead::Whole)));
    let row = asks(&[A, B]);
    let written = need_ids_row(row.need.as_ref().expect("an ask")).expect("ids");
    let want: Vec<(u64, NeedRead)> = escalation(&row)
        .expect("an escalation")
        .needs
        .iter()
        .map(|n| (n.id, n.read))
        .collect();
    assert_eq!(parse_need_ids(&written), want);
    assert_eq!(need_ids_row(boxed().need.as_ref().expect("a box")), None);
}
