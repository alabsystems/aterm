// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for slice 9c's wants (§14.9): what aterm checked, held to the receipt the journal holds.

use super::*;
use crate::forever::{Chain, Cover, Seen, Verdict};

const T0: i64 = 1_792_867_220_000;
const HEAD: &str = "1c81bc6f0e2a3b4c5d6e7f8091a2b3c4d5e6f708";
const RECEIPT_12: &str = "RECEIPT seq=12 rule=land@v1 head=1c81bc6f0e2a on=origin/main leg=on-main \
                          repo=/private/tmp/at.x/proj/land-a branch=main Nothing left: all of it \
                          is pushed.";

fn status(t: i64, task: &str, fields: &str) -> (i64, String) {
    (
        T0 + t,
        format!("task={task} needs=- autopilot=land held=- waiting=- {fields}"),
    )
}

const NONE: &str = "landed_head=- landed_on=- landed_leg=- landed_repo=-";
const ON_MAIN: &str = "landed_head=1c81bc6f0e2a landed_on=origin/main landed_leg=on-main \
                       landed_repo=/private/tmp/at.x/proj/land-a";

fn landing(receipt: Option<&str>, reads: Vec<(i64, String)>) -> Seen {
    let mut journal = vec![(T0 + 40_000, "DONE seq=12 rule=land@v1 …".to_string())];
    journal.extend(receipt.map(|r| (T0 + 40_001, r.to_string())));
    Seen {
        journal,
        status: reads,
        end_ms: T0 + 90_000,
        head: Some(HEAD.to_string()),
        ..Seen::default()
    }
}

fn word_of(v: &Verdict) -> &'static str {
    v.word()
}

/// STATUS SAYS WHAT ATERM CHECKED, AND `-` OFF THE LANDING: the landed read names the journal's
/// receipt and every other read `-`, which passes. NEGATIVE CONTROLS: a build before 9c (no
/// `landed_head=` at all, bin-w4's status) fails; a landed read saying `-`, a read off the
/// landing naming the head, another head than the chain's, and another leg than its folder's
/// each fail; a landing chain with no landed read is unobserved. A chain landing twice has each
/// landed read held to its own landing's receipt.
#[test]
fn status_agrees_with_the_receipt_and_says_dash_off_the_landing() {
    let reads = || {
        vec![
            status(1_000, "open", NONE),
            status(41_000, "landed", ON_MAIN),
        ]
    };
    let v = status_agrees(Chain::LandA, &landing(Some(RECEIPT_12), reads()), T0);
    assert_eq!(word_of(&v), "PASS", "{v:?}");

    let before = vec![
        (T0 + 1_000, "task=open needs=- held=- waiting=-".to_string()),
        (
            T0 + 41_000,
            "task=landed needs=- held=- waiting=-".to_string(),
        ),
    ];
    let v = status_agrees(Chain::LandA, &landing(Some(RECEIPT_12), before), T0);
    assert_eq!(word_of(&v), "FAIL", "a build before 9c: {v:?}");
    let v = status_agrees(
        Chain::LandA,
        &landing(
            Some(RECEIPT_12),
            vec![status(1_000, "open", NONE), status(41_000, "landed", NONE)],
        ),
        T0,
    );
    assert_eq!(word_of(&v), "FAIL", "the landed read says nothing: {v:?}");
    let mut stale = reads();
    stale.push(status(60_000, "open", ON_MAIN));
    let v = status_agrees(Chain::LandA, &landing(Some(RECEIPT_12), stale), T0);
    assert_eq!(
        word_of(&v),
        "FAIL",
        "the receipt outlived its landing: {v:?}"
    );
    let other = Seen {
        head: Some("b0b0b0b0b0b0aaaa".to_string()),
        ..landing(Some(RECEIPT_12), reads())
    };
    assert_eq!(word_of(&status_agrees(Chain::LandA, &other, T0)), "FAIL");
    assert_eq!(
        word_of(&status_agrees(
            Chain::LandN,
            &landing(Some(RECEIPT_12), reads()),
            T0
        )),
        "FAIL",
        "land-n's folder makes no-remote"
    );
    let v = status_agrees(
        Chain::LandA,
        &landing(None, vec![status(1_000, "open", NONE)]),
        T0,
    );
    assert_eq!(word_of(&v), "UNOBSERVED", "{v:?}");

    // A chain that lands again after a person's turn: each landed read speaks for its own
    // landing's receipt; the first read naming the second's head fails.
    let again = |first: &str| {
        let mut s = landing(Some(RECEIPT_12), Vec::new());
        s.journal.push((
            T0 + 70_000,
            RECEIPT_12
                .replace("seq=12", "seq=19")
                .replace("1c81bc6f0e2a", "2d92cd7a1f3b"),
        ));
        s.head = Some("2d92cd7a1f3b4c5d".to_string());
        s.status = vec![
            status(41_000, "landed", first),
            status(55_000, "open", NONE),
            status(
                71_000,
                "landed",
                &ON_MAIN.replace("1c81bc6f0e2a", "2d92cd7a1f3b"),
            ),
        ];
        s
    };
    let v = status_agrees(Chain::LandA, &again(ON_MAIN), T0);
    assert_eq!(word_of(&v), "PASS", "two landings: {v:?}");
    let second = ON_MAIN.replace("1c81bc6f0e2a", "2d92cd7a1f3b");
    let v = status_agrees(Chain::LandA, &again(&second), T0);
    assert_eq!(
        word_of(&v),
        "FAIL",
        "the first landing read the second's head: {v:?}"
    );
}

/// EACH FOLDER'S LEG: land-n's receipt is `no-remote` (its head, no ref) and land-o's `no-repo`
/// (nothing named), each passing on its own chain and failing on the other's.
#[test]
fn each_folder_makes_its_own_leg() {
    let no_remote = "RECEIPT seq=12 rule=land@v1 head=1c81bc6f0e2a on=- leg=no-remote \
                     repo=/private/tmp/at.x/proj/land-n Nothing left: committed here.";
    let no_repo = "RECEIPT seq=12 rule=land@v1 head=- on=- leg=no-repo repo=- Nothing left: \
                   the files are written.";
    let n = landing(
        Some(no_remote),
        vec![status(
            41_000,
            "landed",
            "landed_head=1c81bc6f0e2a landed_on=- landed_leg=no-remote \
             landed_repo=/private/tmp/at.x/proj/land-n",
        )],
    );
    let o = Seen {
        head: None,
        ..landing(
            Some(no_repo),
            vec![status(
                41_000,
                "landed",
                "landed_head=- landed_on=- landed_leg=no-repo landed_repo=-",
            )],
        )
    };
    assert_eq!(leg_of(Chain::LandN), ReceiptLeg::NoRemote);
    assert_eq!(leg_of(Chain::LandO), ReceiptLeg::NoRepo);
    assert_eq!(leg_of(Chain::LandA), ReceiptLeg::OnMain);
    for (chain, seen, want) in [
        (Chain::LandN, &n, "PASS"),
        (Chain::LandO, &o, "PASS"),
        (Chain::LandN, &o, "FAIL"),
        (Chain::LandO, &n, "FAIL"),
    ] {
        let v = status_agrees(chain, seen, T0);
        assert_eq!(word_of(&v), want, "{}: {v:?}", chain.name());
    }
}

/// THE DONE FOR YOU ROW'S TIP: the row's tip is the receipt's `landed_tip`, naming the head on
/// `origin/main`. NEGATIVE CONTROLS: 9b's bare row (no tip) and a tip saying aterm checked
/// nothing fail.
#[test]
fn the_done_rows_tip_names_the_head() {
    let seen = |tip: &str| {
        let mut s = landing(Some(RECEIPT_12), Vec::new());
        s.window.read = true;
        s.window.done_tip = vec![(T0 + 1_000, "-".into()), (T0 + 42_000, tip.to_string())];
        s
    };
    let r = receipt_of(&seen("-"), T0).expect("the receipt");
    let want = mode_words::landed_tip(Some(&r));
    assert!(want.contains("1c81bc6f0e2a on origin/main"), "{want}");
    assert_eq!(word_of(&done_tip(&seen(&want), T0)), "PASS");
    assert_eq!(word_of(&done_tip(&seen("-"), T0)), "FAIL");
    assert_eq!(word_of(&done_tip(&seen(mode_words::NO_RECORD), T0)), "FAIL");
    let unread = Seen {
        window: Default::default(),
        ..seen("-")
    };
    let v = done_tip(&unread, T0);
    assert!(v.why().contains("SKIPPED"), "a run with no window: {v:?}");
}

/// THE WORKER'S `checked=`: `await task`'s row and `result`'s header carry the receipt's word,
/// and its row's tip says what aterm checked. NEGATIVE CONTROL: main's (no `checked=`, a tip of
/// the hold's alone) fails.
#[test]
fn the_workers_await_result_and_row_say_checked() {
    let seen = |checked: &str, tip: &str| {
        let mut s = landing(Some(RECEIPT_12), Vec::new());
        s.window.read = true;
        s.cover = Cover {
            awaited: Some((
                T0 + 50_000,
                0,
                format!(
                    "OK 1\n@s-w rev=3 state=done agent=idle program_state=- attention=- \
                     result_bytes=40{checked} task_news=1"
                ),
            )),
            result_head: Some(format!(
                "OK 2 source=transcript session=x uuid=y bytes=40{checked}"
            )),
            ..Cover::default()
        };
        s.window.worker = vec![(
            T0 + 45_000,
            format!("mgr|mgr — 1 worker: 1 landed\nrow|✓ worker — landed #\ntip|{tip}"),
        )];
        s
    };
    let r = receipt_of(&seen("", ""), T0).expect("the receipt");
    let tip = mode_words::checked_words(Some(&r));
    let v = worker_checked(&seen(" checked=1c81bc6f0e2a@origin/main", &tip), T0);
    assert_eq!(word_of(&v), "PASS", "{v:?}");
    let v = worker_checked(&seen("", "not landed yet: 1 unpushed commit"), T0);
    assert_eq!(word_of(&v), "FAIL", "{v:?}");
}

/// THE LANDED TITLE SAYS ITS OWN RECEIPT (the review round of slice 9c): a landed chip's title,
/// read in front, holds `landed_brief` of the journal's receipt, which passes. NEGATIVE CONTROLS:
/// a title saying aterm has no record of checking (a build whose chip never reads the receipt,
/// which `title_says` alone accepts) and a title naming another head each fail; a run that read
/// no window skips it by name, and one that read no landed title is unobserved.
#[test]
fn the_landed_title_says_its_own_receipt() {
    let chip_line = "autopilot sid=s-1 window=0 program=claude face=landed drawn=\"✓ landed\"";
    let seen = |explained: &str| {
        let mut s = landing(Some(RECEIPT_12), Vec::new());
        s.window.read = true;
        s.window.chip = vec![(T0 + 41_000, chip_line.to_string())];
        s.window.title = vec![(
            T0 + 41_000,
            format!(
                "autopilot: land, landed 14:05 \u{2014} {explained} \u{b7} click: off \u{b7} \
                 right-click: more"
            ),
        )];
        s
    };
    let r = receipt_of(&seen(""), T0).expect("the receipt");
    let ours = mode_words::landed_brief(Some(&r));
    assert!(
        ours.starts_with("aterm checked 1c81bc6f0e2a on origin/main in "),
        "{ours}"
    );
    let v = title_checked(&seen(&ours), T0);
    assert_eq!(word_of(&v), "PASS", "{v:?}");
    let none = mode_words::landed_brief(None);
    let v = title_checked(&seen(&none), T0);
    assert_eq!(
        word_of(&v),
        "FAIL",
        "a title that never read the receipt: {v:?}"
    );
    assert_eq!(
        crate::forever::surfaces::title_says(
            &format!("autopilot: land, landed 14:05 \u{2014} {none}"),
            "land"
        ),
        crate::forever::surfaces::TitleSays::Past("landed"),
        "the shape alone passes it"
    );
    let other = ours.replace("1c81bc6f0e2a", "aaaaaaaaaaaa");
    assert_eq!(word_of(&title_checked(&seen(&other), T0)), "FAIL");
    let unread = Seen {
        window: Default::default(),
        ..seen(&ours)
    };
    let v = title_checked(&unread, T0);
    assert!(v.why().contains("SKIPPED"), "{v:?}");
    let mut open = seen(&ours);
    open.window.chip = vec![(T0 + 41_000, chip_line.replace("landed drawn", "land drawn"))];
    assert_eq!(word_of(&title_checked(&open, T0)), "UNOBSERVED");
}
