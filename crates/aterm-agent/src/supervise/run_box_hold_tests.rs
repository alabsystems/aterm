// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_phase::prompt::fixtures as f;

/// A person's gesture inside [`approval_loop::PERSON_SETTLE`]: a box nobody has touched waits on it.
const TYPING_MS: u64 = 1_000;
/// A person's input past [`approval_loop::PERSON_SETTLE`] and inside every box's bound
/// (`QUESTION_PERSON_GRACE`): a Claude Code permission box nobody has touched is answered over it,
/// and a question, or a box a person or another driver touched, is held through it.
const TYPED_BEFORE_MS: u64 = 5_000;
const GRACE_S: u32 = 120;

/// A person's input inside a question dialog's bound (`QUESTION_PERSON_GRACE`): they are answering
/// now, so the question is held through it as a box is. Input older than the bound holds no
/// question (`a_question_waits_a_short_fixed_quiet_for_a_person_never_their_whole_grace`).
const QUESTION_TYPED_BEFORE_MS: u64 = 5_000;
const QUESTION_TYPED_BEFORE: &str = "5000";

#[test]
fn the_typed_inputs_sit_inside_and_past_the_settle_and_inside_the_grace() {
    use super::approval_loop::{PERSON_SETTLE, QUESTION_PERSON_GRACE};
    assert_eq!(
        QUESTION_TYPED_BEFORE.parse::<u64>().ok(),
        Some(QUESTION_TYPED_BEFORE_MS)
    );
    assert!(Duration::from_millis(QUESTION_TYPED_BEFORE_MS) < QUESTION_PERSON_GRACE);
    assert!(Duration::from_millis(TYPING_MS) < PERSON_SETTLE);
    assert!(Duration::from_millis(TYPED_BEFORE_MS) > PERSON_SETTLE);
    assert!(Duration::from_millis(TYPED_BEFORE_MS) < QUESTION_PERSON_GRACE);
    assert!(QUESTION_PERSON_GRACE < Duration::from_secs(u64::from(GRACE_S)));
}

fn swapped(rows: &[String], edits: &[(&str, &[&str])]) -> Vec<String> {
    let mut out = rows.to_vec();
    for (from, to) in edits {
        let at: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, r)| r.as_str() == *from)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(at.len(), 1, "the row {from:?} once: {out:#?}");
        out.splice(at[0]..=at[0], to.iter().map(|r| (*r).to_string()));
    }
    out
}

fn at_left(rows: &[String], left: &str) -> Vec<String> {
    let mut n = 0;
    let out = rows
        .iter()
        .map(|r| {
            let Some((head, tail)) = r.split_once("deny this request in ") else {
                return r.clone();
            };
            let Some((_, rest)) = tail.split_once(',') else {
                return r.clone();
            };
            n += 1;
            format!("{head}deny this request in {left},{rest}")
        })
        .collect();
    assert_eq!(n, 1, "one countdown row: {rows:#?}");
    out
}

fn mss(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn ticking(from: u64, reads: u64, box_at: impl Fn(&str) -> Vec<String>) -> Vec<Vec<String>> {
    (0..reads).map(|k| box_at(&mss(from - k))).collect()
}

fn watched(screens: Vec<Vec<String>>) -> Vec<Vec<String>> {
    [vec![busy_screen()], screens, vec![busy_screen()]].concat()
}

fn incident(left: &str) -> Vec<String> {
    let measured = f::screen(f::BOX_RM_AUTO_DENY_2_1_301);
    let rows = swapped(
        &measured,
        &[
            (
                "⏺ Bash(S=/private/tmp/at-nonexistent-lab; for p in a b; do set -- $p; rm -rf \
                 $S/$1; done; touch /private/tmp/at.cqJi/proj/ran-base.txt)",
                &["⏺ Bash(rm -f \"$O\"/*)"],
            ),
            (
                " Delete temporary directories",
                &[" Remove the old output files"],
            ),
            (
                " │ S=/private/tmp/at-nonexistent-lab; for p in a b; do set -- $p; rm -rf $S/$1; \
                 done; touch /private/tmp/at.cqJi/proj/ran-base.txt",
                &[" │ rm -f \"$O\"/*"],
            ),
            (
                " │ Dangerous rm operation on possibly-empty variable path: $S/$1 in `rm -rf \
                 $S/$1` (bind $1 and rewrite its $S as \"${S:?}\" or use a literal",
                &[
                    " │ Dangerous rm operation on possibly-empty variable path: \"$O\"/* in `rm \
                     -f \"$O\"/*` (rewrite it as \"${O:?}\"/* or use a literal path)",
                ],
            ),
            (" │ path)", &[]),
        ],
    );
    at_left(&rows, left)
}

fn moved_to_no(rows: &[String]) -> Vec<String> {
    swapped(
        rows,
        &[(" ❯ 1. Yes", &["   1. Yes"]), ("   2. No", &[" ❯ 2. No"])],
    )
}

fn faked(left: &str) -> Vec<String> {
    let rows: Vec<String> = incident("1:50")
        .into_iter()
        .filter(|r| !r.contains("deny this request in"))
        .collect();
    swapped(
        &rows,
        &[(
            " Remove the old output files",
            &[&format!(
                " ⚠ Claude Code will automatically deny this request in {left}, to avoid \
                 blocking progress on an unattended session"
            )],
        )],
    )
}

fn printed_over(rows: &[String]) -> Vec<String> {
    swapped(
        rows,
        &[(
            "✻ Cooked for 0s · done 1:29 PM",
            &["⏺ Agent \"lint\" finished"],
        )],
    )
}

#[test]
fn the_incidents_box_is_read_as_drawn() {
    let p = aterm_phase::parse_prompt_v2(&incident("1:44")).expect("the box");
    assert_eq!(p.kind, aterm_phase::PromptKind::Bash);
    assert_eq!(p.command, "rm -f \"$O\"/*");
    assert_eq!(
        p.notes,
        [
            "Dangerous rm operation on possibly-empty variable path: \"$O\"/* in `rm -f \"$O\"/*` \
          (rewrite it as \"${O:?}\"/* or use a literal path)"
        ]
    );
    assert!(p.rm_breaker().is_some(), "{p:?}");
    assert_eq!(p.auto_deny_left(), Some(Duration::from_secs(104)));
    let p = aterm_phase::parse_prompt_v2(&faked("0:14")).expect("the faked box");
    assert_eq!(p.auto_deny, None, "{p:?}");
    assert!(p.description.contains("deny this request in 0:14"), "{p:?}");
}

#[test]
fn a_countdown_is_believed_once_seen_ticking() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let read = |rows: Vec<String>, ms: u64, before: Option<&approval_loop::Countdown>| {
        approval_loop::countdown_of(&rows, at(ms))
            .expect("the countdown")
            .after(before)
    };
    let once = read(incident("1:44"), 0, None);
    assert_eq!(once.deny(), None);
    let ticked = read(incident("1:43"), 1_000, Some(&once));
    assert_eq!(ticked.deny(), Some(at(1_000) + Duration::from_secs(103)));
    let again = read(incident("1:43"), 1_600, Some(&ticked));
    assert_eq!(again.deny(), ticked.deny(), "placed by the first read");
    let stopped = read(incident("1:43"), 4_500, Some(&again));
    assert_eq!(stopped.deny(), None, "stood 3.5 s");
    let over = read(incident("1:59"), 5_000, Some(&ticked));
    assert_eq!(over.deny(), None, "started over");
    let over = read(incident("1:58"), 6_000, Some(&over));
    assert!(over.deny().is_some(), "ticking again");

    let mut still = read(incident("0:14"), 0, None);
    for ms in [1_000, 2_000, 3_000] {
        still = read(incident("0:14"), ms, Some(&still));
        assert_eq!(still.deny(), None, "{ms}");
    }
    let still = read(incident("0:14"), 3_500, Some(&still));
    assert_eq!(still.deny(), None, "stood past the slack");
    let jumped = read(incident("0:14"), 1_000, Some(&once));
    assert_eq!(jumped.deny(), None, "faster than the clock");
    let other = |left: &str| {
        swapped(
            &incident(left),
            &[(" │ rm -f \"$O\"/*", &[" │ rm -f \"$O\"/x"])],
        )
    };
    let redrawn = read(other("1:43"), 1_000, Some(&once));
    assert_eq!(redrawn.deny(), ticked.deny(), "another box in step");
    let jumped = read(other("0:14"), 1_000, Some(&once));
    assert_eq!(jumped.deny(), None, "another box, faster than the clock");
}

/// Which loop a test runs: as shipped, or one of the defects
/// `supervisor_box_settle_model` names, each its negative control.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Build {
    /// The loop as shipped.
    Shipped,
    /// A question's hold outlasts its countdown, as before the cap
    /// ([`Session::set_deny_cap_off`]).
    CapOff,
    /// No box episode reads as touched ([`Session::set_touch_off`]: the
    /// model's `Buggy == 1`).
    TouchOff,
    /// The touch rule and grace before the settle: any rise in the person
    /// count since the read before the box a touch, and an untouched box held
    /// as a question is ([`Session::set_old_grace`]: `Buggy == 2`).
    OldGrace,
    /// An untouched box answered with no settle ([`Session::set_settle_off`]:
    /// `Buggy == 3`).
    SettleOff,
    /// Every box other than the last read's starts an episode afresh
    /// ([`Session::set_episode_per_box`]: `Buggy == 4`).
    EpisodePerBox,
}

fn held_watch(
    screens: Vec<Vec<String>>,
    typed_ms: Option<u64>,
    build: Build,
    tweak: impl FnOnce(&mut Mock, &mut SuperviseOpts),
) -> (Mock, Vec<String>) {
    let (m, journal, _) = held_watch_ledgered(screens, typed_ms, build, tweak);
    (m, journal)
}

fn held_watch_ledgered(
    screens: Vec<Vec<String>>,
    typed_ms: Option<u64>,
    build: Build,
    tweak: impl FnOnce(&mut Mock, &mut SuperviseOpts),
) -> (Mock, Vec<String>, Vec<String>) {
    held_watch_seamed(screens, typed_ms, |s| built(s, build), tweak)
}

/// [`held_watch_ledgered`] with `seam` set on the session the watch runs (a build's mutant, a
/// read that answers late).
fn held_watch_seamed(
    screens: Vec<Vec<String>>,
    typed_ms: Option<u64>,
    seam: impl FnOnce(&mut Session<'_, TapedCtl<'_, Mock>>),
    tweak: impl FnOnce(&mut Mock, &mut SuperviseOpts),
) -> (Mock, Vec<String>, Vec<String>) {
    let (jdir, journal) = journal_file("box-hold");
    let (ldir, ledger) = ledger_file("box-hold");
    let mut m = fenced(screens);
    m.human_ms = typed_ms;
    m.human = vec![typed_ms.map_or("null", |ms| {
        let stamp: &'static str = Box::leak(ms.to_string().into_boxed_str());
        stamp
    })];
    m.human_seq = Some(0);
    m.vanish_after = Some(0);
    let mut opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..full_power_opts()
    };
    opts.policy.human_grace_s = GRACE_S;
    tweak(&mut m, &mut opts);
    let _ = watch_lines_with(&mut m, &opts, |s| {
        s.set_read_clock();
        s.set_person_hold_wait(Duration::ZERO);
        s.set_approval_ledger(Some(ledger.clone()));
        s.setup.pause = Duration::from_millis(5);
        seam(s);
    });
    let (records, _) = journal_records(&journal);
    let rows = ledger_rows(&ledger);
    let _ = std::fs::remove_dir_all(&jdir);
    let _ = std::fs::remove_dir_all(&ldir);
    (m, records.into_iter().map(|r| r.line).collect(), rows)
}

fn built<C: Ctl>(s: &mut Session<'_, C>, build: Build) {
    match build {
        Build::Shipped => {}
        Build::CapOff => s.set_deny_cap_off(),
        Build::TouchOff => s.set_touch_off(),
        Build::OldGrace => s.set_old_grace(),
        Build::SettleOff => s.set_settle_off(),
        Build::EpisodePerBox => s.set_episode_per_box(),
    }
}

fn ones(m: &Mock) -> usize {
    m.presses().iter().filter(|p| p.ends_with(" 1")).count()
}

fn held_lines(journal: &[String]) -> Vec<&String> {
    journal.iter().filter(|l| l.starts_with("HELD ")).collect()
}

fn theirs(journal: &[String]) -> Vec<&String> {
    held_lines(journal)
        .into_iter()
        .filter(|l| l.contains(": held as ever"))
        .collect()
}

fn ended(journal: &[String]) -> bool {
    journal.iter().any(|l| l.starts_with("HOLD-ENDED "))
}

fn held_times(line: &str) -> (String, Option<String>) {
    let until = line
        .split_once("hands off until ")
        .and_then(|(_, t)| t.split_whitespace().next())
        .expect("the hold's end")
        .to_string();
    let deny = line
        .split_once("the vendor denies it at ")
        .map(|(_, t)| t.trim_end_matches(')').to_string());
    (until, deny)
}

fn key_names(m: &Mock) -> Vec<&str> {
    m.presses()
        .iter()
        .map(|p| p.rsplit_once(' ').map_or("", |(_, k)| k))
        .collect()
}

/// The words of a hold on a box nobody has touched ([`approval_loop::PERSON_SETTLE`]).
const SETTLING: &str =
    "(the box's answer; nobody has touched it, so it waits only for their keys to settle)";

fn past(stamp: &str) -> i64 {
    aterm_types::rfc3339::parse_utc_fractional(stamp).expect("a stamp") - TEST_NOW
}

/// The person typing on the box's first look, and quiet ([`TYPED_BEFORE_MS`]) from the read after.
fn typing_then_quiet(m: &mut Mock) {
    m.status_human_from.insert(3, TYPED_BEFORE_MS);
}

/// A person's Enter queuing a message 0.8 s before Claude Code drew an rm box, counted between the
/// read before the box and its first read: it decided nothing on the box, which waits only until
/// their keys settle ([`approval_loop::PERSON_SETTLE`]) and is then answered by the policy, fenced
/// on the person count it was decided on. The loop before the settle (`Build::OldGrace`) read the
/// count as a touch and held the box for the person's whole grace (`held as ever`), so they
/// answered it by hand; one with no settle (`Build::SettleOff`) answers it at its first look,
/// 0.8 s after the key.
#[test]
fn an_untouched_box_is_answered_once_the_persons_keys_settle() {
    let run = |build: Build| {
        held_watch(
            watched(ticking(104, 4, incident)),
            Some(800),
            build,
            |m, _| {
                for (screen, ms) in [(1, "800"), (2, "1800"), (3, "2800"), (4, "3800")] {
                    m.human_on.insert(screen, ms);
                }
                for (read, ms) in [(3, 1_800), (4, 2_800), (5, 3_800)] {
                    m.status_human_from.insert(read, ms);
                }
                m.human_seq_from.insert(2, 1);
            },
        )
    };
    let (m, journal) = run(Build::Shipped);
    let digits: Vec<&String> = m
        .presses()
        .into_iter()
        .filter(|p| p.ends_with(" 1"))
        .collect();
    assert_eq!(digits.len(), 1, "{:#?}\n{journal:#?}", m.requests);
    assert!(digits[0].starts_with("key if-human=1 "), "{digits:#?}");
    let held = held_lines(&journal);
    assert_eq!(held.len(), 2, "{journal:#?}");
    for l in &held {
        assert!(
            l.contains("a person is typing: hands off until ") && l.ends_with(SETTLING),
            "{l}"
        );
        let (until, deny) = held_times(l);
        assert_eq!(deny, None, "the deny is no part of the settle: {l}");
        assert!(past(&until) <= 5, "the settle, not a grace: {l}");
    }
    assert!(theirs(&journal).is_empty(), "{journal:#?}");
    assert!(!ended(&journal), "{journal:#?}");

    let (m, journal) = run(Build::OldGrace);
    assert_eq!(ones(&m), 0, "the old grace: {:#?}", m.requests);
    let theirs = theirs(&journal);
    assert!(
        !theirs.is_empty()
            && theirs.iter().all(|l| l.contains(
                "a person's gesture was counted on the session since it appeared: held as ever"
            )),
        "{journal:#?}"
    );

    let (m, journal) = run(Build::SettleOff);
    assert_eq!(ones(&m), 1, "no settle: {:#?}", m.requests);
    assert!(
        held_lines(&journal).is_empty(),
        "answered at its first look: {journal:#?}"
    );
}

/// A key that moved a box's focus off where Claude Code drew it is a person deciding the box,
/// whether they moved it before the box's first read, while it was held, or moved it and back: the
/// box is held for their whole grace (`held as ever`), the touch kept through the focus coming
/// back and the transcript printed over. A gesture counted while the box is shown that moves
/// nothing (a scroll, a letter, a message queued into the composer) is no touch, nor is a box
/// first read mid-way: the box is answered once the keys settle, fenced on the count it was
/// decided on; with no person count to fence that answer on inside the person's grace, the box is
/// handed over unpressed.
#[test]
fn a_box_a_person_touched_is_held_as_ever() {
    let touched = [
        (
            "a Down onto No",
            watched(vec![
                incident("0:15"),
                moved_to_no(&incident("0:14")),
                moved_to_no(&incident("0:13")),
            ]),
        ),
        (
            "before its first read",
            watched(vec![
                moved_to_no(&incident("0:15")),
                moved_to_no(&incident("0:14")),
                moved_to_no(&incident("0:13")),
            ]),
        ),
        (
            "Down and back",
            watched(vec![
                incident("0:15"),
                moved_to_no(&incident("0:14")),
                incident("0:13"),
                incident("0:12"),
            ]),
        ),
        (
            "printed over once touched",
            watched(vec![
                incident("0:15"),
                printed_over(&moved_to_no(&incident("0:14"))),
                printed_over(&moved_to_no(&incident("0:13"))),
            ]),
        ),
    ];
    for (name, screens) in touched {
        let (m, journal) = held_watch(screens.clone(), Some(TYPING_MS), Build::Shipped, |m, _| {
            typing_then_quiet(m);
        });
        assert!(m.presses().is_empty(), "{name}: {:#?}", m.requests);
        assert!(
            theirs(&journal).iter().any(|l| l
                .contains("(the box's answer; a key not aterm's moved its focus: held as ever")),
            "{name}: {journal:#?}"
        );
        assert!(!ended(&journal), "{name}: {journal:#?}");
        let (m, _) = held_watch(screens, Some(TYPING_MS), Build::TouchOff, |m, _| {
            typing_then_quiet(m);
        });
        assert_eq!(ones(&m), 1, "{name}, touch off: {:#?}", m.requests);
    }

    let (m, journal) = held_watch(
        watched(ticking(15, 3, incident)),
        Some(TYPING_MS),
        Build::Shipped,
        |m, _| {
            typing_then_quiet(m);
            m.human_seq_from.insert(3, 1);
        },
    );
    let presses = m.presses();
    assert_eq!(
        presses.len(),
        1,
        "a key that moves nothing: {:#?}",
        m.requests
    );
    assert!(presses[0].starts_with("key if-human=1 "), "{presses:#?}");
    assert!(theirs(&journal).is_empty(), "{journal:#?}");

    let (m, journal) = held_watch(
        [ticking(15, 3, incident), vec![busy_screen()]].concat(),
        Some(TYPING_MS),
        Build::Shipped,
        |m, _| typing_then_quiet(m),
    );
    let presses = m.presses();
    assert_eq!(presses.len(), 1, "attached mid-box: {:#?}", m.requests);
    assert!(presses[0].starts_with("key if-human=0 "), "{presses:#?}");
    assert!(theirs(&journal).is_empty(), "{journal:#?}");

    let (m, _, rows) = held_watch_ledgered(
        watched(ticking(15, 3, incident)),
        Some(TYPING_MS),
        Build::Shipped,
        |m, _| {
            typing_then_quiet(m);
            m.human_seq = None;
        },
    );
    assert!(m.presses().is_empty(), "no count: {:#?}", m.requests);
    assert!(
        rows.iter().any(|r| r
            .contains("the read carried no person count to fence an answer inside their grace on")),
        "{rows:#?}"
    );
}

fn lease_before(screens: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let mut worked = rows(&[
        "⏺ Fixed the parser; the suite is green.",
        "",
        "✻ Worked for 12m 2s · done 4:24 PM",
        "",
    ]);
    worked.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    [vec![busy_screen(), worked], watched(screens)].concat()
}

fn let_go_before(m: &mut Mock, opts: &mut SuperviseOpts) {
    opts.policy.continue_policy = true;
    m.hand_from.insert(0, "lease:operator");
    m.hand_from.insert(3, "-");
    for i in 0..3 {
        m.human_on.insert(i, "600000");
    }
    m.status_human_from.insert(0, 600_000);
    m.status_human_from.insert(3, TYPED_BEFORE_MS);
}

/// Another driver's hand on the session holds a box as a person's key does, and touches it: once
/// the hand lifts the box is held for the person's whole grace (`held as ever`). A hand gone
/// before the box was drawn touches nothing, and the person alone, quiet, has the box answered.
#[test]
fn another_drivers_hand_on_the_session_is_a_touch() {
    let at_margin = || watched(ticking(17, 5, incident));
    let (m, journal) = held_watch(at_margin(), None, Build::Shipped, |m, _| {
        m.hand = "lease:operator";
    });
    assert_eq!(ones(&m), 0, "{:#?}", m.requests);
    assert!(
        held_lines(&journal)
            .iter()
            .all(|l| l.contains("another driver's hand is on the session: hands off until ")),
        "{journal:#?}"
    );
    assert!(
        theirs(&journal).iter().any(|l| l.contains(
            "another driver's hand was on the session while it was up: held as ever, and the \
             vendor denies it at "
        )),
        "{journal:#?}"
    );

    let come_and_gone = |m: &mut Mock, _: &mut SuperviseOpts| {
        typing_then_quiet(m);
        m.hand_from.insert(3, "lease:operator");
        m.hand_from.insert(5, "-");
    };
    let (m, journal) = held_watch(at_margin(), Some(TYPING_MS), Build::Shipped, come_and_gone);
    assert_eq!(ones(&m), 0, "lease come and gone: {:#?}", m.requests);
    assert!(
        theirs(&journal)
            .iter()
            .any(|l| l.contains("a person is typing: hands off until ")
                && l.contains(
                    "another driver's hand was on the session while it was up: held as ever"
                )),
        "{journal:#?}"
    );
    let (m, _) = held_watch(at_margin(), Some(TYPING_MS), Build::TouchOff, come_and_gone);
    assert_eq!(
        ones(&m),
        1,
        "lease come and gone, touch off: {:#?}",
        m.requests
    );

    let (m, journal) = held_watch(
        at_margin(),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |_, _| {},
    );
    assert_eq!(ones(&m), 1, "the person alone: {:#?}", m.requests);
    assert!(held_lines(&journal).is_empty(), "{journal:#?}");
    let (m, journal) = held_watch(
        lease_before(ticking(17, 5, incident)),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        let_go_before,
    );
    let busy_again = m
        .order
        .iter()
        .enumerate()
        .filter(|(_, (r, _))| r.starts_with("text"))
        .nth(2)
        .map_or(0, |(at, _)| at);
    assert!(
        m.order[..busy_again].iter().any(|(r, _)| r == "status"),
        "{:#?}",
        m.order
    );
    assert_eq!(
        ones(&m),
        1,
        "the lease gone before the box: {:#?}",
        m.requests
    );
    assert!(theirs(&journal).is_empty(), "{journal:#?}");
}

fn rewrapped(rows: &[String]) -> Vec<String> {
    const NOTE: &str = " │ Dangerous rm operation on possibly-empty variable path: \"$O\"/* in `rm -f \
                        \"$O\"/*` (rewrite it as \"${O:?}\"/* or use a literal path)";
    let ruled: Vec<String> = rows
        .iter()
        .map(|r| match r.chars().next() {
            Some(c @ ('─' | '╌')) if r.chars().all(|d| d == c) => c.to_string().repeat(100),
            _ => r.clone(),
        })
        .collect();
    let countdown = ruled
        .iter()
        .find(|r| r.contains("deny this request in "))
        .expect("the countdown")
        .clone();
    let (head, tail) = countdown
        .split_once(" to avoid ")
        .expect("the countdown's words");
    swapped(
        &ruled,
        &[
            (
                NOTE,
                &[
                    " │ Dangerous rm operation on possibly-empty variable path: \"$O\"/* in `rm -f \
                     \"$O\"/*` (rewrite it as",
                    " │ \"${O:?}\"/* or use a literal path)",
                ],
            ),
            (&countdown, &[head, &format!(" to avoid {tail}")]),
        ],
    )
}

/// A box re-wrapped or redrawn at another width is the same box, untouched: it waits only for the
/// person's keys to settle and is answered then.
#[test]
fn a_redraw_is_no_touch() {
    let p = aterm_phase::parse_prompt_v2(&rewrapped(&incident("0:14"))).expect("the box");
    assert_eq!(p.auto_deny_left(), Some(Duration::from_secs(14)), "{p:?}");
    for cols in [72, 76, 100] {
        let rows = subagent(cols, "0:14");
        let p = aterm_phase::parse_prompt_v2(&rows).expect("the subagent's box");
        assert_eq!(
            p.auto_deny_left(),
            Some(Duration::from_secs(14)),
            "{cols}: {p:?}"
        );
        assert!(
            rows.iter().all(|r| r.chars().count() <= cols),
            "{cols}: {rows:#?}"
        );
    }
    let footer = |rows: &[String]| {
        rows.iter()
            .find(|r| r.starts_with(" Esc to cancel"))
            .expect("the footer")
            .clone()
    };
    assert!(footer(&subagent(72, "0:14")).ends_with("twice to stop background…"));
    assert!(footer(&subagent(76, "0:14")).ends_with("twice to stop background agen…"));
    assert!(footer(&subagent(100, "0:14")).ends_with("twice to stop background agents"));
    for (name, screens, quiet_from) in [
        (
            "re-wrapped",
            watched(vec![
                incident("0:18"),
                incident("0:17"),
                rewrapped(&incident("0:16")),
                rewrapped(&incident("0:15")),
                rewrapped(&incident("0:14")),
            ]),
            5,
        ),
        (
            "resized",
            watched(vec![
                subagent(72, "0:21"),
                subagent(72, "0:20"),
                subagent(76, "0:19"),
                subagent(76, "0:18"),
                subagent(100, "0:17"),
                subagent(100, "0:16"),
            ]),
            6,
        ),
    ] {
        let (m, journal) = held_watch(screens, Some(TYPING_MS), Build::Shipped, |m, _| {
            m.status_human_from.insert(quiet_from, TYPED_BEFORE_MS);
        });
        assert_eq!(ones(&m), 1, "{name}: {:#?}\n{journal:#?}", m.requests);
        let held = held_lines(&journal);
        assert!(
            held.len() >= 3 && held.iter().all(|l| l.ends_with(SETTLING)),
            "{name}: {journal:#?}"
        );
    }
}

fn narrowed(rows: &[String]) -> Vec<String> {
    let countdown = rows
        .iter()
        .find(|r| r.contains("deny this request in "))
        .expect("the countdown")
        .clone();
    let (head, tail) = countdown
        .split_once(" request in ")
        .expect("the countdown's words");
    let (time, rest) = tail
        .split_once(" an unattended ")
        .expect("the countdown's tail");
    swapped(
        rows,
        &[(
            &countdown,
            &[
                head,
                &format!(" request in {time}"),
                &format!(" an unattended {rest}"),
            ],
        )],
    )
}

/// A countdown a narrow pane wraps, and one that says `block this request in` (2.1.316), whole or
/// wrapped through its words, is read as the box's countdown and believed once seen ticking, one
/// box through every tick; so is one on a box relabelled between its reads.
#[test]
fn a_countdown_wrapped_relabelled_or_saying_block_is_read_as_one() {
    let t0 = Instant::now();
    let narrow = |left: &str| narrowed(&incident(left));
    let block = |left: &str| blocked(left, false);
    let block_wrapped = |left: &str| blocked(left, true);
    for (name, at) in [
        ("narrowed", &narrow as &dyn Fn(&str) -> Vec<String>),
        ("block", &block),
        ("block, wrapped", &block_wrapped),
    ] {
        let p = aterm_phase::parse_prompt_v2(&at("1:44")).expect(name);
        assert_eq!(
            p.auto_deny_left(),
            Some(Duration::from_secs(104)),
            "{name}: {p:?}"
        );
        assert!(p.rm_breaker().is_some(), "{name}: {p:?}");
        if name != "narrowed" {
            let one = approval_loop::box_identity(&at("1:44"));
            assert!(one.is_some(), "{name}");
            assert_eq!(
                one,
                approval_loop::box_identity(&at("1:43")),
                "{name}: one box through the tick"
            );
        }
        let first = approval_loop::countdown_of(&at("1:44"), t0).expect(name);
        let next = approval_loop::countdown_of(&at("1:43"), t0 + Duration::from_secs(1))
            .expect(name)
            .after(Some(&first));
        assert_eq!(
            next.deny(),
            Some(t0 + Duration::from_secs(104)),
            "{name}: ticking"
        );
    }
    let first = approval_loop::countdown_of(&incident("0:16"), t0).expect("the countdown");
    let relabelled = approval_loop::countdown_of(
        &stacked(&incident("0:15"), "1 of 2"),
        t0 + Duration::from_secs(1),
    )
    .expect("the relabelled countdown")
    .after(Some(&first));
    assert_eq!(relabelled.deny(), Some(t0 + Duration::from_secs(16)));
}

/// Every key of an answer names the person count the answer was decided on (`if-human=`), however
/// long the person has been quiet: a gesture of theirs after it skips the key, the skip is ledgered
/// as theirs, and the box, its focus where it was, is decided again on the new count.
#[test]
fn a_digit_answer_is_fenced_on_the_person_count_it_was_decided_on() {
    let digits = |m: &Mock| -> Vec<String> {
        m.presses()
            .into_iter()
            .filter(|p| p.ends_with(" 1"))
            .cloned()
            .collect()
    };
    let (m, _, rows) = held_watch_ledgered(
        watched(ticking(15, 3, incident)),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |m, _| m.person_at_key = 1,
    );
    let presses = digits(&m);
    assert_eq!(presses.len(), 2, "{:#?}", m.requests);
    assert!(presses[0].starts_with("key if-human=0 "), "{presses:#?}");
    assert!(presses[1].starts_with("key if-human=1 "), "{presses:#?}");
    assert_eq!(m.person_skipped.len(), 1, "{:#?}", m.requests);
    let skipped: Vec<&String> = rows
        .iter()
        .filter(|r| r.contains("\"decision\":\"skipped\""))
        .collect();
    assert_eq!(skipped.len(), 1, "{rows:#?}");
    assert!(
        skipped[0].contains(
            "a person gave the session input while the answer was keyed: decided again after \
             their quiet"
        ),
        "{}",
        skipped[0]
    );

    let (m, _) = held_watch(
        watched(vec![incident("0:14")]),
        None,
        Build::Shipped,
        |_, _| {},
    );
    let presses = digits(&m);
    assert_eq!(presses.len(), 1, "{:#?}", m.requests);
    assert!(presses[0].starts_with("key if-human=0 "), "{presses:#?}");
}

fn unnumbered(rows: &[String]) -> Vec<String> {
    swapped(
        rows,
        &[(" ❯ 1. Yes", &[" ❯ Yes"]), ("   2. No", &["   No"])],
    )
}

fn fenced_on(m: &mut Mock) {
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
    m.human_seq = Some(0);
}

/// [`a_digit_answer_is_fenced_on_the_person_count_it_was_decided_on`] for a box answered by its
/// focus and Enter.
#[test]
fn a_focus_answer_is_fenced_on_the_person_count_it_was_decided_on() {
    let yes = |left: &str| unnumbered(&incident(left));
    let p = aterm_phase::parse_prompt_v2(&yes("0:14")).expect("the box");
    assert_eq!(
        (p.select, p.auto_deny_left()),
        (
            aterm_phase::prompt::Select::ArrowsEnter,
            Some(Duration::from_secs(14))
        ),
        "{p:?}"
    );
    let (m, _) = held_watch(
        watched(ticking(15, 3, yes)),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |m, _| {
            fenced_on(m);
            m.person_at_key = 1;
        },
    );
    let presses = m.presses();
    assert_eq!(key_names(&m), ["enter", "enter"], "{:#?}", m.requests);
    assert!(presses[0].starts_with("key if-human=0 "), "{presses:#?}");
    assert!(presses[1].starts_with("key if-human=1 "), "{presses:#?}");
    assert_eq!(m.person_skipped.len(), 1, "{:#?}", m.requests);

    let (m, _) = held_watch(watched(vec![yes("0:14")]), None, Build::Shipped, |m, _| {
        fenced_on(m);
    });
    let presses = m.presses();
    assert_eq!(presses.len(), 1, "{:#?}", m.requests);
    assert!(
        presses[0].ends_with(" enter") && presses[0].starts_with("key if-human=0 "),
        "{presses:#?}"
    );
}

fn default_no(left: &str, on_yes: bool) -> Vec<String> {
    let rows = swapped(
        &f::screen(f::BOX_TOOL_DEFAULT_NO),
        &[(
            " Do you want to proceed?",
            &[
                &format!(
                    " ⚠ Claude Code will automatically deny this request in {left}, to avoid \
                     blocking progress on an unattended session"
                ),
                " Do you want to proceed?",
            ],
        )],
    );
    if on_yes {
        swapped(&rows, &[(" ❯ No", &["   No"]), ("   Yes", &[" ❯ Yes"])])
    } else {
        rows
    }
}

/// A person's gesture that refuses one of aterm's keys (`skipped reason=person`) leaves the box's
/// focus where aterm's keys put it as the read before showed it: a key of theirs that moves it from
/// there, with no key of aterm's since, makes the box theirs, held for their whole grace, where the
/// loop with no touch (`Build::TouchOff`) would answer it again over them.
#[test]
fn a_persons_key_after_the_loops_own_arrow_makes_the_box_theirs() {
    let run = |build: Build| {
        held_watch(
            watched(vec![
                default_no("0:15", false),
                default_no("0:14", false),
                default_no("0:13", true),
                default_no("0:12", false),
                default_no("0:11", false),
                default_no("0:10", false),
            ]),
            Some(TYPED_BEFORE_MS),
            build,
            |m, _| {
                fenced_on(m);
                m.key_replies.push_back(ok("OK\n"));
                m.key_replies.push_back(ok("OK skipped reason=person\n"));
                m.human_seq_from.insert(5, 1);
            },
        )
    };
    let (m, journal) = run(Build::Shipped);
    assert_eq!(key_names(&m), ["down", "enter"], "{:#?}", m.requests);
    assert!(
        theirs(&journal)
            .iter()
            .any(|l| l.contains("a key not aterm's moved its focus: held as ever")),
        "{journal:#?}"
    );
    let (m, _) = run(Build::TouchOff);
    assert_eq!(
        key_names(&m)[..3],
        ["down", "enter", "down"],
        "{:#?}",
        m.requests
    );
}

/// A focus answer's Enter is keyed on the read that saw its arrow land, fenced on the person count
/// that read carried: a gesture counted between the arrow and that read that left the focus where
/// the arrow put it moved nothing on the box, so the answer goes on.
#[test]
fn a_focus_answer_s_enter_is_fenced_on_the_read_that_saw_its_arrow_land() {
    let (m, journal) = held_watch(
        watched(vec![
            default_no("0:15", false),
            default_no("0:14", false),
            default_no("0:13", true),
            default_no("0:12", true),
            default_no("0:11", true),
        ]),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |m, _| {
            fenced_on(m);
            m.human_seq_from.insert(4, 1);
        },
    );
    assert_eq!(key_names(&m), ["down", "enter"], "{:#?}", m.requests);
    let presses = m.presses();
    assert!(
        presses[0].starts_with("key if-human=0 ") && presses[1].starts_with("key if-human=1 "),
        "{presses:#?}"
    );
    assert!(m.person_skipped.is_empty(), "{:#?}", m.requests);
    assert!(theirs(&journal).is_empty(), "{journal:#?}");
}

/// A read with no box ends the box's episode, so the same box asked again after it is another
/// episode: a touch on the first is no touch on the second, and a touch on the second leaves the
/// first answered.
#[test]
fn a_box_asked_again_after_a_read_without_one_is_another_episode() {
    let screens = |a_moved: bool, b_moved: bool| {
        let a = |rows: Vec<String>| if a_moved { moved_to_no(&rows) } else { rows };
        let b = |rows: Vec<String>| if b_moved { moved_to_no(&rows) } else { rows };
        watched(vec![
            a(incident("0:15")),
            a(incident("0:14")),
            busy_screen(),
            b(incident("0:16")),
            b(incident("0:15")),
            b(incident("0:14")),
        ])
    };
    let run = |a_moved: bool, b_moved: bool, build: Build| {
        held_watch(
            screens(a_moved, b_moved),
            Some(TYPED_BEFORE_MS),
            build,
            |_, _| {},
        )
    };
    let (m, journal) = run(false, true, Build::Shipped);
    assert_eq!(ones(&m), 1, "A only: {:#?}", m.requests);
    assert!(!theirs(&journal).is_empty(), "B theirs: {journal:#?}");
    let (m, _) = run(false, false, Build::Shipped);
    assert_eq!(ones(&m), 2, "A and B: {:#?}", m.requests);
    let (m, _) = run(false, true, Build::TouchOff);
    assert_eq!(ones(&m), 2, "touch off: {:#?}", m.requests);
    let (m, journal) = run(true, false, Build::Shipped);
    assert_eq!(ones(&m), 1, "B only: {:#?}\n{journal:#?}", m.requests);
    assert!(!theirs(&journal).is_empty(), "A theirs: {journal:#?}");
}

fn next_request(left: &str) -> Vec<String> {
    swapped(
        &incident(left),
        &[
            ("⏺ Bash(rm -f \"$O\"/*)", &["⏺ Bash(rm -f \"$L\"/*)"]),
            (" │ rm -f \"$O\"/*", &[" │ rm -f \"$L\"/*"]),
            (
                " │ Dangerous rm operation on possibly-empty variable path: \"$O\"/* in `rm -f \
                 \"$O\"/*` (rewrite it as \"${O:?}\"/* or use a literal path)",
                &[
                    " │ Dangerous rm operation on possibly-empty variable path: \"$L\"/* in `rm \
                     -f \"$L\"/*` (rewrite it as \"${L:?}\"/* or use a literal path)",
                ],
            ),
        ],
    )
}

/// A box a person or another driver touched stays theirs through a relabel (`1 of 2`, `2 of 2`)
/// and the next request drawn in its place while they hold it; a touch kept through the focus
/// coming back is lost by a loop that starts an episode at every box (`Build::EpisodePerBox`).
#[test]
fn a_touched_box_relabelled_or_replaced_stays_theirs() {
    let p = aterm_phase::parse_prompt_v2(&stacked(&incident("0:14"), "1 of 2")).expect("the box");
    assert_eq!(
        (p.kind, p.title.as_str(), p.auto_deny_left()),
        (
            aterm_phase::PromptKind::Bash,
            "Bash command",
            Some(Duration::from_secs(14))
        ),
        "{p:?}"
    );
    let screens = |moved: bool| {
        let b = |rows: Vec<String>| if moved { moved_to_no(&rows) } else { rows };
        watched(vec![
            incident("0:17"),
            b(incident("0:16")),
            b(stacked(&incident("0:15"), "1 of 2")),
            b(stacked(&incident("0:14"), "1 of 2")),
            b(stacked(&incident("0:13"), "2 of 2")),
            b(stacked(&incident("0:12"), "2 of 2")),
            b(next_request("0:11")),
            b(next_request("0:10")),
        ])
    };
    let person = |build: Build| {
        held_watch(screens(true), Some(TYPING_MS), build, |m, _| {
            typing_then_quiet(m);
        })
    };
    let lease = |build: Build| {
        held_watch(screens(false), None, build, |m, _| {
            m.hand = "lease:operator"
        })
    };
    for (name, run, why) in [
        (
            "person",
            &person as &dyn Fn(Build) -> (Mock, Vec<String>),
            "a key not aterm's moved its focus: held as ever",
        ),
        (
            "lease",
            &lease,
            "another driver's hand was on the session while it was up: held as ever",
        ),
    ] {
        let (m, journal) = run(Build::Shipped);
        assert!(
            m.presses().is_empty(),
            "{name}: {:#?}\n{journal:#?}",
            m.requests
        );
        let theirs = theirs(&journal);
        assert!(theirs.len() >= 6, "{name}: {journal:#?}");
        assert!(
            theirs.iter().all(|l| l.contains(why)),
            "{name}: {theirs:#?}"
        );
    }
    let (m, _) = person(Build::TouchOff);
    assert!(ones(&m) >= 1, "person, touch off: {:#?}", m.requests);

    let back_up = |build: Build| {
        held_watch(
            watched(vec![
                incident("0:17"),
                moved_to_no(&incident("0:16")),
                incident("0:15"),
                stacked(&incident("0:14"), "1 of 2"),
                stacked(&incident("0:13"), "1 of 2"),
            ]),
            Some(TYPING_MS),
            build,
            |m, _| typing_then_quiet(m),
        )
    };
    let (m, journal) = back_up(Build::Shipped);
    assert!(m.presses().is_empty(), "{:#?}\n{journal:#?}", m.requests);
    assert!(theirs(&journal).len() >= 3, "{journal:#?}");
    let (m, _) = back_up(Build::EpisodePerBox);
    assert!(ones(&m) >= 1, "episode per box: {:#?}", m.requests);
}

/// Every tab of a question dialog, and its review, is one episode: a person's key that moved a
/// tab's focus, or another driver's hand on it, keeps the review theirs, which a loop that starts
/// an episode at every box (`Build::EpisodePerBox`) answers.
#[test]
fn a_question_dialog_s_tabs_are_one_episode() {
    let keys = |lease: bool, moved: bool, build: Build| {
        let (jdir, journal) = journal_file("box-hold-tab");
        let tab = f::screen(f::QUESTION_TABS_FOURTH);
        let second = if moved {
            focused_on(&tab, "2. Top")
        } else {
            tab.clone()
        };
        let mut m = questioned(watched(
            [vec![tab, second], ticking(15, 3, review_ticking)].concat(),
        ));
        if lease {
            m.hand = "lease:operator";
        } else {
            m.human = vec![QUESTION_TYPED_BEFORE];
            m.human_ms = Some(QUESTION_TYPED_BEFORE_MS);
        }
        m.human_seq = Some(0);
        m.vanish_after = Some(0);
        let mut opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..answering(30)
        };
        opts.policy.human_grace_s = GRACE_S;
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
            s.set_read_clock();
            s.set_person_hold_wait(Duration::ZERO);
            built(s, build);
        });
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
        (
            key_names(&m)
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            journal,
        )
    };
    for (name, lease, moved, why) in [
        (
            "person",
            false,
            true,
            "a key not aterm's moved its focus: held as ever",
        ),
        (
            "lease",
            true,
            false,
            "another driver's hand was on the session while it was up: held as ever",
        ),
    ] {
        let (pressed, journal) = keys(lease, moved, Build::Shipped);
        assert!(pressed.is_empty(), "{name}: {pressed:?}\n{journal:#?}");
        assert!(
            theirs(&journal).iter().any(|l| l.contains(why)),
            "{name}: {journal:#?}"
        );
        assert!(!ended(&journal), "{name}: {journal:#?}");
    }
    let (pressed, journal) = keys(false, false, Build::Shipped);
    assert_eq!(pressed, ["enter"], "untouched: {journal:#?}");
    let (pressed, journal) = keys(false, true, Build::EpisodePerBox);
    assert_eq!(pressed, ["enter"], "episode per box: {journal:#?}");
    assert!(ended(&journal), "{journal:#?}");
}

fn review_ticking(left: &str) -> Vec<String> {
    review_with_countdown(f::QUESTION_REVIEW, left)
}

fn review_with_countdown(fixture: &str, left: &str) -> Vec<String> {
    let rows = f::screen(fixture);
    let at = rows
        .iter()
        .position(|r| r.trim() == "Ready to submit your answers?")
        .expect("the review's question");
    let mut out = rows;
    out.splice(
        at..at,
        [
            format!(
                "⚠ Claude Code will automatically deny this request in {left}, to avoid blocking \
                 progress on an unattended session"
            ),
            String::new(),
        ],
    );
    out
}

#[test]
fn a_question_dialog_is_answered_before_its_countdown_runs_out() {
    let p = aterm_phase::parse_prompt_v2(&review_ticking("0:14")).expect("the dialog");
    assert_eq!(p.kind, aterm_phase::PromptKind::Question);
    assert!(
        p.question_dialog
            .as_ref()
            .is_some_and(|d| matches!(d.form, aterm_phase::QuestionForm::Review { .. })),
        "read whole: {p:?}"
    );
    assert_eq!(p.auto_deny_left(), Some(Duration::from_secs(14)));
    let keys = |screens: Vec<Vec<String>>, build: Build| {
        let mut m = questioned(screens);
        m.human = vec![QUESTION_TYPED_BEFORE];
        m.human_ms = Some(QUESTION_TYPED_BEFORE_MS);
        m.human_seq = Some(0);
        m.vanish_after = Some(0);
        let mut opts = answering(30);
        opts.policy.human_grace_s = GRACE_S;
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
            s.set_read_clock();
            s.set_person_hold_wait(Duration::ZERO);
            built(s, build);
        });
        m.presses()
            .iter()
            .map(|r| r.rsplit_once(' ').map_or("", |(_, k)| k).to_string())
            .collect::<Vec<_>>()
    };
    let enters = |keys: &[String]| keys.iter().filter(|k| *k == "enter").count();
    assert_eq!(
        enters(&keys(
            watched(ticking(15, 2, review_ticking)),
            Build::Shipped
        )),
        1,
        "answered before the deny"
    );
    assert_eq!(
        enters(&keys(
            watched(ticking(104, 2, review_ticking)),
            Build::Shipped
        )),
        0,
        "held for the person above the margin"
    );
    assert_eq!(
        enters(&keys(
            watched(ticking(15, 2, review_ticking)),
            Build::CapOff
        )),
        0,
        "the hold outlasts the countdown"
    );
    let skipped_to = |left: &str| review_with_countdown(f::QUESTION_REVIEW_UNANSWERED, left);
    let p = aterm_phase::parse_prompt_v2(&skipped_to("0:14")).expect("the review");
    assert!(
        p.question_dialog.as_ref().is_some_and(|d| matches!(
            d.form,
            aterm_phase::QuestionForm::Review {
                unanswered: true,
                ..
            }
        )),
        "{p:?}"
    );
    let reviewed = || {
        watched(vec![
            skipped_to("0:15"),
            focused_on(&skipped_to("0:14"), "2. Cancel"),
            skipped_to("0:13"),
            skipped_to("0:12"),
        ])
    };
    assert!(
        keys(reviewed(), Build::Shipped).is_empty(),
        "a person's review"
    );
    assert_eq!(enters(&keys(reviewed(), Build::TouchOff)), 1, "touch off");
    let cancel = |left: &str| focused_on(&review_ticking(left), "2. Cancel");
    let attached = || [ticking(15, 3, cancel), vec![busy_screen()]].concat();
    assert!(
        keys(attached(), Build::Shipped).is_empty(),
        "attached mid-dialog"
    );
    assert_eq!(
        keys(attached(), Build::TouchOff)
            .first()
            .map(String::as_str),
        Some("up"),
        "touch off"
    );
}

/// A question dialog's answer past its countdown's margin is fenced on the person count it was
/// decided on: a gesture of theirs counted as its Enter is keyed skips the Enter, and the dialog is
/// decided again on the new count.
#[test]
fn a_gesture_counted_inside_the_answer_past_the_hold_stops_it() {
    let mut m = questioned(watched(ticking(15, 4, review_ticking)));
    m.human = vec![QUESTION_TYPED_BEFORE];
    m.human_ms = Some(QUESTION_TYPED_BEFORE_MS);
    m.human_seq = Some(0);
    m.person_at_key = 1;
    m.vanish_after = Some(0);
    let mut opts = answering(30);
    opts.policy.human_grace_s = GRACE_S;
    let _ = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_read_clock();
        s.set_person_hold_wait(Duration::ZERO);
    });
    assert_eq!(key_names(&m), ["enter", "enter"], "{:#?}", m.requests);
    let presses = m.presses();
    assert!(
        presses[0].starts_with("key if-human=0 ") && presses[1].starts_with("key if-human=1 "),
        "{presses:#?}"
    );
    assert_eq!(m.person_skipped.len(), 1, "{:#?}", m.requests);
}

fn tall_ticking(left: &str) -> [Vec<String>; 4] {
    tall_states("$S").map(|mut s| {
        let q = s
            .iter()
            .rposition(|r| r.trim() == "Do you want to proceed?")
            .expect("the question");
        s.splice(
            q..q,
            [
                format!(
                    " ⚠ Claude Code will automatically deny this request in {left}, to avoid \
                     blocking progress on an unattended session"
                ),
                String::new(),
            ],
        );
        s.split_off(2)
    })
}

fn tall_decline(
    from: u64,
    states: &[usize],
    build: Build,
    world: &dyn Fn(&mut Mock),
) -> (Vec<String>, Vec<String>) {
    let reads: Vec<Vec<String>> = states
        .iter()
        .zip((0..=from).rev())
        .map(|(&state, left)| tall_ticking(&mss(left))[state].clone())
        .collect();
    let (jdir, journal) = journal_file("box-hold-decline");
    let mut screens = vec![bypass_busy()];
    screens.extend(reads);
    screens.push(bypass_busy());
    let mut m = fenced_both(screens);
    m.cwd = Some("/Users/_owner/aterm".to_string());
    m.vanish_after = Some(0);
    m.human = vec!["22000"];
    m.human_ms = Some(TYPED_BEFORE_MS);
    m.human_seq = Some(0);
    world(&mut m);
    let mut opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..auto(30)
    };
    opts.policy.human_grace_s = GRACE_S;
    let _ = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_read_clock();
        s.set_person_hold_wait(Duration::ZERO);
        built(s, build);
    });
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&jdir);
    (
        writes(&m),
        records.into_iter().map(|r| r.line).collect::<Vec<_>>(),
    )
}

#[test]
fn a_decline_goes_before_the_countdown_runs_out() {
    let states = [0usize, 0, 0, 0, 1, 1, 2, 2, 3, 3];
    for (state, left) in states.iter().zip((9..=18u64).rev()) {
        let s = &tall_ticking(&mss(left))[*state];
        let p = aterm_phase::parse_prompt_v2(s).expect("the tall box");
        assert!(p.head_off_screen, "{s:#?}");
        assert!(p.auto_deny_left().is_some(), "{s:#?}");
    }
    let (w, _) = tall_decline(18, &states, Build::Shipped, &|_| {});
    assert_eq!(w.len(), 4, "{w:#?}");
    assert!(w[3].ends_with(" enter"), "{w:#?}");
    let (w, _) = tall_decline(18, &states, Build::CapOff, &|_| {});
    assert!(w.is_empty(), "the hold outlasts the countdown: {w:#?}");
}

#[test]
fn a_persons_key_during_the_loops_decline_makes_the_box_theirs() {
    let states = [0usize, 0, 0, 0, 1, 1, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3];
    let raced = |m: &mut Mock| {
        m.key_replies.push_back(ok("OK\n"));
        m.key_replies.push_back(ok("OK skipped reason=person\n"));
        m.human_seq_from.insert(8, 1);
    };
    let (w, journal) = tall_decline(17, &states, Build::Shipped, &raced);
    assert_eq!(w.len(), 2, "{w:#?}\n{journal:#?}");
    assert!(w[0].ends_with(" down") && w[1].ends_with(" tab"), "{w:#?}");
    assert!(!theirs(&journal).is_empty(), "{journal:#?}");
    let (w, journal) = tall_decline(17, &states, Build::TouchOff, &raced);
    assert!(w.len() >= 5, "touch off: {w:#?}\n{journal:#?}");
    assert!(
        w[2].ends_with(" down") && w.last().is_some_and(|k| k.ends_with(" enter")),
        "touch off: {w:#?}"
    );
}

fn idle_run_out(requests: &[String], tick_ms: u64) -> Vec<&String> {
    requests
        .iter()
        .filter(|r| {
            r.split_once("await idle ").is_some_and(|(_, rest)| {
                rest.split_whitespace()
                    .next()
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .is_some_and(|ms| ms >= tick_ms)
            })
        })
        .collect()
}

fn supervised_ticking(rows: Vec<String>, tick: Option<u64>) -> (Mock, Vec<String>) {
    let book = Book::new("box-hold-retry");
    let mut m = questioned(vec![rows]);
    m.tick_ms = tick;
    m.countdown_ticks = true;
    let mut s = session(&mut m, None);
    s.set_read_clock();
    s.set_approval_env(owner_env());
    book.keep(&mut s);
    s.supervise(&answering(30)).expect("supervise");
    let lines = book.escalated();
    (m, lines)
}

#[test]
fn a_key_the_ticking_dialog_did_not_take_is_sent_again_before_its_deny() {
    let enters = |m: &Mock| m.presses().iter().filter(|r| r.ends_with(" enter")).count();
    let (m, lines) = supervised_ticking(review_ticking("0:14"), Some(1000));
    assert_eq!(enters(&m), 2, "{:#?}", m.requests);
    assert!(
        lines.iter().any(|l| l.contains(
            "the dialog did not take the key: nothing changed after it, nor after it was sent \
             once more"
        )),
        "{lines:#?}"
    );
    let first = m
        .requests
        .iter()
        .position(|r| verb_of(r) == "key" && r.ends_with(" enter"))
        .expect("the Enter");
    assert_eq!(
        idle_run_out(&m.requests[first..], 1000),
        Vec::<&String>::new(),
        "{:#?}",
        m.requests
    );

    let (m, _) = supervised_ticking(f::screen(f::QUESTION_REVIEW), None);
    assert_eq!(enters(&m), 2, "{:#?}", m.requests);
}

fn still_waits(m: &Mock) -> Vec<(u64, u64)> {
    let first = m
        .requests
        .iter()
        .position(|r| verb_of(r) == "key" && r.ends_with(" enter"))
        .expect("the Enter");
    m.requests[first..]
        .iter()
        .filter_map(|r| {
            let quiet = r.strip_prefix("await idle ")?.split_whitespace().next()?;
            Some((quiet.parse().ok()?, timeout_ms(r)))
        })
        .filter(|&wait| wait != (500, 1_500))
        .collect()
}

#[test]
fn above_the_margin_a_ticking_dialog_waits_two_seconds_of_quiet() {
    for (left, cap) in [("1:44", 20_000), ("0:30", 15_000)] {
        let (m, lines) = supervised_ticking(review_ticking(left), Some(1000));
        let enters = m.presses().iter().filter(|r| r.ends_with(" enter")).count();
        assert_eq!(enters, 1, "{left}: {:#?}", m.requests);
        let waits = still_waits(&m);
        assert!(!waits.is_empty(), "{left}: {:#?}", m.requests);
        assert!(
            waits.iter().all(|&(quiet, ms)| quiet == 2_000 && ms <= cap),
            "{left}: {waits:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("the screen never held still after it")),
            "{left}: {lines:#?}"
        );
    }
}

fn timeout_ms(request: &str) -> u64 {
    request
        .rsplit_once(" timeout ")
        .and_then(|(_, ms)| ms.parse().ok())
        .unwrap_or_else(|| panic!("no timeout: {request}"))
}

#[test]
fn a_press_near_the_deny_the_box_did_not_take_is_pressed_again_before_it() {
    let leave = |left: &str| -> (usize, u64, u64) {
        let (m, _) = held_watch(vec![incident(left)], None, Build::Shipped, |m, _| {
            m.vanish_after = Some(2);
            m.countdown_ticks = true;
        });
        let first = m
            .requests
            .iter()
            .position(|r| verb_of(r) == "key" && r.ends_with(" 1"))
            .unwrap_or_else(|| panic!("{left}: no press: {:#?}", m.requests));
        let waits: Vec<u64> = m.requests[first..]
            .iter()
            .filter(|r| r.starts_with("await seq "))
            .take(2)
            .map(|r| timeout_ms(r))
            .collect();
        assert_eq!(waits.len(), 2, "{left}: {:#?}", m.requests);
        (ones(&m), waits[0], waits[1])
    };
    let (presses, first, ms) = leave("0:14");
    assert_eq!((presses, first), (2, 20_000), "pressed again");
    assert!(ms <= 3_000, "0:14: {ms}");
    let (presses, first, ms) = leave("0:25");
    assert_eq!((presses, first), (2, 20_000), "pressed again");
    assert!((3_000..=9_000).contains(&ms), "0:25: {ms}");
    let (presses, first, ms) = leave("1:44");
    assert_eq!((presses, first), (2, 20_000), "pressed again");
    assert!(ms > 9_000, "1:44: {ms}");
}

fn stacked(rows: &[String], label: &str) -> Vec<String> {
    let title = format!(" Bash command{label:>131}");
    swapped(rows, &[(" Bash command", &[&title])])
}

fn truncated(text: &str, cols: usize) -> String {
    if text.chars().count() <= cols {
        return text.to_string();
    }
    let mut out: String = text.chars().take(cols - 1).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

fn wrapped_at(text: &str, cols: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in text.split(' ') {
        if !row.is_empty() && row.chars().count() + 1 + word.chars().count() > cols {
            out.push(std::mem::take(&mut row));
        }
        row.push(' ');
        row.push_str(word);
    }
    out.push(row);
    out
}

fn subagent(cols: usize, left: &str) -> Vec<String> {
    const FOOTER: &str =
        " Esc to cancel · Tab to amend · ctrl+x ctrl+k twice to stop background agents";
    let rows = f::screen(f::BOX_BASH_BACKGROUND_AGENT_STACKED_2_1_292);
    let ruled: Vec<String> = rows
        .iter()
        .map(|r| match r.chars().next() {
            Some(c @ ('─' | '╌')) if r.chars().all(|d| d == c) => c.to_string().repeat(cols),
            _ => r.clone(),
        })
        .collect();
    let rule = ruled
        .iter()
        .rposition(|r| r.starts_with('╌'))
        .expect("the command's rule");
    let mut out = ruled;
    let countdown = wrapped_at(
        &format!(
            "⚠ Claude Code will automatically deny this request in {left}, to avoid blocking \
             progress on an unattended session"
        ),
        cols,
    );
    out.splice(
        rule + 1..rule + 1,
        countdown.into_iter().chain([String::new()]),
    );
    let footer = out
        .iter()
        .position(|r| r.starts_with(" Esc to cancel"))
        .expect("the footer");
    out[footer] = truncated(FOOTER, cols);
    out
}

#[test]
fn the_first_key_after_a_ticking_dialogs_wait_is_fenced_on_a_read_after_it() {
    let screens = watched(vec![
        review_ticking("0:15"),
        review_ticking("0:14"),
        review_ticking("0:13"),
    ]);
    let mut m = questioned(screens);
    m.tick_ms = Some(1000);
    m.redraws_move_gen = true;
    m.human = vec![QUESTION_TYPED_BEFORE];
    m.human_ms = Some(QUESTION_TYPED_BEFORE_MS);
    m.human_seq = Some(0);
    m.vanish_after = Some(0);
    let mut opts = answering(30);
    opts.policy.human_grace_s = GRACE_S;
    let _ = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_read_clock();
        s.set_person_hold_wait(Duration::ZERO);
    });
    assert_eq!(key_names(&m), ["enter"], "{:#?}", m.requests);
}

#[test]
fn a_hold_is_looked_at_again_when_it_ends() {
    let mut m = fenced(vec![busy_screen(), incident("1:44"), incident("1:43")]);
    m.human_ms = Some(TYPED_BEFORE_MS);
    m.human = vec!["22000"];
    m.human_seq = Some(0);
    let mut s = session(&mut m, None);
    s.set_read_clock();
    let mut opts = full_power_opts();
    opts.policy.human_grace_s = GRACE_S;
    let _ = s.screen().expect("the busy worker");
    let mut owed = || {
        let screen = s.screen().expect("the box");
        let clock = s.deny_clock(&screen);
        let held = s
            .held_for_person(
                screen.seq,
                "the box's answer",
                true,
                clock,
                &opts,
                &mut Alone,
            )
            .expect("the hold");
        assert!(held, "{:#?}", screen.rows);
        s.due_at()
            .expect("a look owed")
            .saturating_duration_since(s.fx.mono())
    };
    let look = owed();
    assert!(look <= approval_loop::TICK_LOOK, "{look:?}");
    let look = owed();
    assert!((87..=88).contains(&look.as_secs()), "{look:?}");
}

#[test]
fn a_lease_taken_during_the_first_key_wait_stops_the_answer() {
    let run = |lease: bool| {
        let screens = watched(vec![
            review_ticking("0:15"),
            review_ticking("0:14"),
            review_ticking("0:13"),
            review_ticking("0:12"),
        ]);
        let (jdir, journal) = journal_file("box-hold-lease-first-key");
        let mut m = questioned(screens);
        m.tick_ms = Some(1000);
        m.redraws_move_gen = true;
        m.human = vec![QUESTION_TYPED_BEFORE];
        m.human_ms = Some(QUESTION_TYPED_BEFORE_MS);
        m.human_seq = Some(0);
        m.vanish_after = Some(0);
        if lease {
            m.hand_during_idle = Some((QUESTION_FIRST_KEY_IDLE, "lease:operator"));
        }
        let mut opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..answering(30)
        };
        opts.policy.human_grace_s = GRACE_S;
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
            s.set_read_clock();
            s.set_person_hold_wait(Duration::ZERO);
        });
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
        (m, journal)
    };
    let (m, journal) = run(true);
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with(&format!("await idle {QUESTION_FIRST_KEY_IDLE} "))),
        "the first-key wait: {:#?}",
        m.requests
    );
    assert_eq!(key_names(&m), Vec::<&str>::new(), "{:#?}", m.requests);
    assert!(
        theirs(&journal)
            .iter()
            .any(|l| l.contains("another driver's hand was on the session while it was up")),
        "{journal:#?}"
    );
    let (m, _) = run(false);
    assert_eq!(key_names(&m), ["enter"], "{:#?}", m.requests);
}

#[test]
fn a_box_that_left_after_its_leave_wait_ran_out_left() {
    let (m, journal) = held_watch(
        watched(vec![incident("1:44")]),
        None,
        Build::Shipped,
        |m, _| m.seq_waits_run_out = 1,
    );
    assert_eq!(ones(&m), 1, "{:#?}", m.requests);
    assert!(
        journal
            .iter()
            .all(|l| !l.contains("did not change after the press")),
        "{journal:#?}"
    );
}

fn wiped(rows: &[String]) -> Vec<String> {
    let at = rows
        .iter()
        .position(|r| r.contains("deny this request in "))
        .expect("the countdown");
    let mut out = rows.to_vec();
    out.drain(at..=at + 1);
    out
}

#[test]
fn a_box_whose_countdown_a_key_wiped_is_waited_on_to_leave() {
    let box_at = |left: &str| incident(left);
    let screens = watched(vec![
        box_at("0:15"),
        box_at("0:14"),
        wiped(&box_at("0:14")),
        wiped(&box_at("0:14")),
        wiped(&box_at("0:14")),
    ]);
    let (m, journal) = held_watch(screens, None, Build::Shipped, |m, _| {
        m.stall_after_read.insert(4, Duration::from_secs(4));
    });
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("await seq ") && timeout_ms(r) <= 3_000),
        "PRECONDITION: the short wait near the deny: {:#?}",
        m.requests
    );
    assert_eq!(ones(&m), 1, "{:#?}\n{journal:#?}", m.requests);
    assert!(
        journal
            .iter()
            .all(|l| !l.contains("did not change after the press")),
        "{journal:#?}"
    );
}

#[test]
fn a_countdown_that_stood_still_is_looked_at_again_until_it_ticks() {
    let mut screens = vec![busy_screen()];
    screens.extend(std::iter::repeat_n(incident("1:50"), 5));
    screens.push(incident("1:45"));
    let mut m = fenced(screens);
    m.human_ms = Some(TYPED_BEFORE_MS);
    m.human = vec!["22000"];
    m.human_seq = Some(0);
    let mut s = session(&mut m, None);
    s.set_read_clock();
    let mut opts = full_power_opts();
    opts.policy.human_grace_s = GRACE_S;
    let _ = s.screen().expect("the busy worker");
    for k in 1..=5 {
        let screen = s.screen().expect("the box");
        let clock = s.deny_clock(&screen);
        let held = s
            .held_for_person(
                screen.seq,
                "the box's answer",
                true,
                clock,
                &opts,
                &mut Alone,
            )
            .expect("the hold");
        assert!(held, "{k}: {:#?}", screen.rows);
        let look = s
            .due_at()
            .expect("a look owed")
            .saturating_duration_since(s.fx.mono());
        assert!(look <= approval_loop::TICK_LOOK, "read {k}: {look:?}");
    }
    let screen = s.screen().expect("the box, ticking again");
    let now = s.fx.mono();
    match s.deny_clock(&screen) {
        approval_loop::DenyClock::At { deny } => {
            let left = deny.saturating_duration_since(now).as_secs();
            assert!((104..=106).contains(&left), "{left}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn no_still_wait_runs_past_the_deny_it_races() {
    let book = Book::new("box-hold-blink");
    let mut m = questioned(vec![review_ticking("0:14")]);
    m.idle_never = true;
    m.countdown_ticks = true;
    let mut s = session(&mut m, None);
    s.set_read_clock();
    s.set_approval_env(owner_env());
    book.keep(&mut s);
    s.supervise(&answering(30)).expect("supervise");
    drop(s);
    let waits = still_waits(&m);
    assert!(!waits.is_empty(), "{:#?}", m.requests);
    assert!(
        waits
            .iter()
            .all(|&(quiet, ms)| quiet == 500 && ms <= 14_000),
        "{waits:?}"
    );
}

/// The measured 2.1.316 rm box (`f::BOX_RM_AUTO_BLOCK_2_1_316`), whose countdown says `block this
/// request in`, carrying the incident's call ([`incident`]) and `left` on its countdown: the
/// countdown row whole, or (`wrapped`) cut through its words as a narrower pane wraps it.
fn blocked(left: &str, wrapped: bool) -> Vec<String> {
    let measured = f::screen(f::BOX_RM_AUTO_BLOCK_2_1_316);
    let countdown = if wrapped {
        vec![
            " ⚠ Claude Code will automatically block this".to_string(),
            format!(" request in {left}, to avoid holding up an"),
            " unattended session".to_string(),
        ]
    } else {
        vec![format!(
            " ⚠ Claude Code will automatically block this request in {left}, to avoid holding up \
             an unattended session"
        )]
    };
    let countdown: Vec<&str> = countdown.iter().map(String::as_str).collect();
    swapped(
        &measured,
        &[
            (
                "⏺ Bash(S=/private/tmp/at-nonexistent-lab; for p in a b; do set -- $p; rm -rf \
                 $S/$1; done; touch /private/tmp/at.UxZb/proj/ran.txt)",
                &["⏺ Bash(rm -f \"$O\"/*)"],
            ),
            (
                " Delete temporary directories",
                &[" Remove the old output files"],
            ),
            (
                " │ S=/private/tmp/at-nonexistent-lab; for p in a b; do set -- $p; rm -rf $S/$1; \
                 done; touch /private/tmp/at.UxZb/proj/ran.txt",
                &[" │ rm -f \"$O\"/*"],
            ),
            (
                " │ Dangerous rm operation on possibly-empty variable path: $S/$1 in `rm -rf \
                 $S/$1` (bind $1 and rewrite its $S as \"${S:?}\" or use a literal",
                &[
                    " │ Dangerous rm operation on possibly-empty variable path: \"$O\"/* in `rm \
                     -f \"$O\"/*` (rewrite it as \"${O:?}\"/* or use a literal path)",
                ],
            ),
            (" │ path)", &[]),
            (
                " ⚠ Claude Code will automatically block this request in 1:59, to avoid holding \
                 up an unattended session",
                &countdown,
            ),
        ],
    )
}

/// One world [`tier1_a_box_goes_only_where_the_settle_model_allows`] runs: its box reads, the
/// person's last gesture on every read (`human_ms`), and what the model reads of it.
struct World {
    name: &'static str,
    screens: Vec<Vec<String>>,
    typed_ms: Option<u64>,
    answers: bool,
    lease: bool,
    counted: bool,
    touched: bool,
}

fn worlds() -> Vec<World> {
    let quiet = Some(TYPED_BEFORE_MS);
    let world = |name: &'static str, screens: Vec<Vec<String>>, typed_ms: Option<u64>| World {
        name,
        screens,
        typed_ms,
        answers: true,
        lease: false,
        counted: false,
        touched: false,
    };
    let boxed = || ticking(104, 2, incident);
    let moved = || ticking(104, 2, |left| moved_to_no(&incident(left)));
    vec![
        world("untouched, quiet", boxed(), quiet),
        world("untouched, typing", boxed(), Some(TYPING_MS)),
        World {
            touched: true,
            ..world("touched, quiet", moved(), quiet)
        },
        World {
            touched: true,
            ..world(
                "touched, their grace over",
                moved(),
                Some(u64::from(GRACE_S) * 1000 + 10_000),
            )
        },
        world("nobody at the keys", boxed(), None),
        World {
            answers: false,
            ..world("escalated", boxed(), quiet)
        },
        World {
            lease: true,
            touched: true,
            ..world("another driver's hand", boxed(), None)
        },
        World {
            touched: true,
            ..world(
                "redrawn once touched",
                vec![
                    moved_to_no(&incident("1:44")),
                    incident("1:43"),
                    stacked(&incident("1:42"), "1 of 2"),
                    stacked(&incident("1:41"), "1 of 2"),
                ],
                quiet,
            )
        },
        World {
            counted: true,
            ..world("a gesture that moved nothing", boxed(), quiet)
        },
    ]
}

fn pressed_in(w: &World, build: Build) -> bool {
    let (answers, lease, counted) = (w.answers, w.lease, w.counted);
    let (m, _) = held_watch(watched(w.screens.clone()), w.typed_ms, build, |m, opts| {
        if !answers {
            opts.policy.approve = Approve::None;
        }
        if lease {
            m.hand = "lease:operator";
        }
        if counted {
            m.human_seq_from.insert(2, 1);
        }
    });
    !m.presses().is_empty()
}

/// Each world's run under `build`, and the worlds where it pressed or held a box the shipped
/// `SupervisorBoxSettle` model does not let it.
fn settle_violations(build: Build) -> (Vec<String>, Vec<bool>) {
    let model = aterm_models_harness::supervisor_box_settle_model();
    let settle = u64::try_from(approval_loop::PERSON_SETTLE.as_millis()).unwrap_or(u64::MAX);
    let grace = u64::from(GRACE_S) * 1000;
    let mut bad = Vec::new();
    let mut presses = Vec::new();
    for w in worlds() {
        let pressed = pressed_in(&w, build);
        presses.push(pressed);
        let mut st = model.init_state();
        for (k, v) in [
            ("up", 1),
            ("policy", i64::from(w.answers)),
            (
                "grace",
                i64::from(w.lease || w.typed_ms.is_some_and(|ms| ms < grace)),
            ),
            ("typed", i64::from(w.typed_ms.is_some_and(|ms| ms < settle))),
            ("driver", i64::from(w.lease)),
            ("touched", i64::from(w.touched)),
            ("hand", i64::from(w.touched)),
        ] {
            st.insert(k, v);
        }
        let action = if pressed { "Press" } else { "Wait" };
        if !model.action_enabled(action, &st) {
            bad.push(format!("{}: {action} at {st:?}", w.name));
        }
    }
    (bad, presses)
}

/// Binds the loop to `supervisor_box_settle_model`: in every world the shipped loop presses a box
/// only where the model's `Press` is enabled and holds it only where its `Wait` is, and each
/// defect the model names (`Buggy`) is a build that breaks it in the worlds that defect reaches:
/// pressing over a touch (`Build::TouchOff`), holding an untouched box for a person's input or a
/// count that moved nothing on it (`Build::OldGrace`), answering while the person types
/// (`Build::SettleOff`), and losing a touch at a redraw (`Build::EpisodePerBox`).
#[test]
fn tier1_a_box_goes_only_where_the_settle_model_allows() {
    let names = |bad: &[String]| -> Vec<String> {
        bad.iter()
            .map(|b| b.split_once(": ").map_or("", |(n, _)| n).to_string())
            .collect()
    };
    let shipped = [true, false, false, true, true, false, false, false, true];
    let (bad, presses) = settle_violations(Build::Shipped);
    assert_eq!(bad, Vec::<String>::new());
    assert_eq!(presses, shipped);
    let all = worlds();
    for (build, flipped, action) in [
        (Build::TouchOff, &[2usize, 7][..], "Press"),
        (Build::OldGrace, &[0, 8][..], "Wait"),
        (Build::SettleOff, &[1][..], "Press"),
        (Build::EpisodePerBox, &[7][..], "Press"),
    ] {
        let (bad, presses) = settle_violations(build);
        let mut want = shipped;
        for &i in flipped {
            want[i] = !want[i];
        }
        assert_eq!(presses, want, "{build:?}");
        let named: Vec<String> = flipped.iter().map(|&i| all[i].name.to_string()).collect();
        assert_eq!(names(&bad), named, "{build:?}: {bad:#?}");
        assert!(
            bad.iter().all(|b| b.contains(&format!(": {action} at "))),
            "{build:?}: {bad:#?}"
        );
    }
}

/// [`super::super::observe_table`]'s row of a decline: down, tab, the decline's text and Enter
/// ([`a_decline_goes_before_the_countdown_runs_out`]).
pub(crate) fn observe_row_decline() -> &'static [&'static str] {
    let states = [0usize, 0, 0, 0, 1, 1, 2, 2, 3, 3];
    let _ = tall_decline(18, &states, Build::Shipped, &|_| {});
    &[" down", " tab", " enter"]
}
