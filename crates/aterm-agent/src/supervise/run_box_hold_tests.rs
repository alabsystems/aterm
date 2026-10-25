// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_phase::prompt::fixtures as f;

/// A person's input inside every box's bound (`QUESTION_PERSON_GRACE`): they are at the keys now,
/// so an untouched permission box is held through it as a question is, and the holds below run
/// against the vendor's countdown.
const TYPED_BEFORE_MS: u64 = 5_000;
/// A person's input past that bound and inside their whole grace: it holds a permission box only
/// once a person or another driver has touched the box
/// (`an_untouched_box_is_held_by_older_input_no_longer_than_a_question`).
const TYPED_LONG_BEFORE_MS: u64 = 22_000;
const GRACE_S: u32 = 120;

/// A person's input inside a question dialog's bound (`QUESTION_PERSON_GRACE`): they are answering
/// now, so the question is held through it as a box is. Input older than the bound holds no
/// question (`a_question_waits_a_short_fixed_quiet_for_a_person_never_their_whole_grace`).
const QUESTION_TYPED_BEFORE_MS: u64 = 5_000;
const QUESTION_TYPED_BEFORE: &str = "5000";

#[test]
fn the_typed_inputs_sit_inside_and_past_the_bound_and_inside_the_grace() {
    use super::approval_loop::QUESTION_PERSON_GRACE;
    assert_eq!(
        QUESTION_TYPED_BEFORE.parse::<u64>().ok(),
        Some(QUESTION_TYPED_BEFORE_MS)
    );
    assert!(Duration::from_millis(QUESTION_TYPED_BEFORE_MS) < QUESTION_PERSON_GRACE);
    assert!(Duration::from_millis(TYPED_BEFORE_MS) < QUESTION_PERSON_GRACE);
    assert!(Duration::from_millis(TYPED_LONG_BEFORE_MS) > QUESTION_PERSON_GRACE);
    assert!(Duration::from_millis(TYPED_LONG_BEFORE_MS) < Duration::from_secs(u64::from(GRACE_S)));
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

/// Which loop a test runs: as shipped, or one of the three defects the
/// model names, each its negative control.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Build {
    /// The loop as shipped.
    Shipped,
    /// The hold outlasts the countdown, as before the cap
    /// ([`Session::set_deny_cap_off`]: the model's `Outlast`).
    CapOff,
    /// No box episode reads as touched ([`Session::set_touch_off`]: the
    /// model's `Touched`).
    TouchOff,
    /// Every box other than the last read's starts an episode afresh
    /// ([`Session::set_episode_per_box`]: the model's `Lost`).
    EpisodePerBox,
    /// An untouched permission box keeps the person's whole grace, as before
    /// its bound ([`Session::set_bound_off`]).
    BoundOff,
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
        built(s, build);
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
        Build::EpisodePerBox => s.set_episode_per_box(),
        Build::BoundOff => s.set_bound_off(),
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

#[test]
fn the_incident_box_is_pressed_before_the_vendor_denies_it() {
    let (m, journal) = held_watch(
        watched(ticking(104, 2, incident)),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |_, _| {},
    );
    assert_eq!(ones(&m), 0, "held above the margin: {:#?}", m.requests);
    let held = held_lines(&journal);
    assert_eq!(held.len(), 2, "{journal:#?}");
    assert!(
        held[0].ends_with("(the box's answer; a countdown on it is read again to see it tick)"),
        "{}",
        held[0]
    );
    let past = |stamp: &str| {
        aterm_types::rfc3339::parse_utc_fractional(stamp).expect("a stamp") - TEST_NOW
    };
    let (look, _) = held_times(held[0]);
    assert!(past(&look) <= 2, "{}", held[0]);
    assert!(
        held[1].contains("a person is typing: hands off until ")
            && held[1].contains("(the box's answer; the vendor denies it at "),
        "{}",
        held[1]
    );
    let (until, deny) = held_times(held[1]);
    let (until, deny) = (past(&until), past(deny.as_deref().expect("the deny named")));
    assert!((45..=103).contains(&deny), "{deny}: {}", held[1]);
    assert!(
        until <= 10 && deny - until > 16,
        "the untouched box's bound ends the hold, not the deny: {until}: {}",
        held[1]
    );
    assert!(!ended(&journal), "{journal:#?}");

    let (m, journal) = held_watch(
        watched(ticking(20, 2, incident)),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |_, _| {},
    );
    assert_eq!(ones(&m), 0, "held above the margin: {:#?}", m.requests);
    let held = held_lines(&journal);
    assert_eq!(held.len(), 2, "{journal:#?}");
    let (until, deny) = held_times(held[1]);
    let (until, deny) = (past(&until), past(deny.as_deref().expect("the deny named")));
    assert!((16..=20).contains(&deny), "{deny}: {}", held[1]);
    assert!(
        until < 9 && (14..=16).contains(&(deny - until)),
        "the hold ends at the deny's margin, before the bound: {until}: {}",
        held[1]
    );
    assert!(!ended(&journal), "{journal:#?}");

    let at_the_margin = || watched(ticking(15, 2, incident));
    let (m, journal) = held_watch(
        at_the_margin(),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |_, _| {},
    );
    assert_eq!(ones(&m), 1, "pressed once: {:#?}", m.requests);
    let end = journal
        .iter()
        .position(|l| l.starts_with("HOLD-ENDED "))
        .expect("the hold's end said");
    assert!(
        journal[end].contains(
            "a person is typing, but nobody has touched the box and the vendor denies it at "
        ) && journal[end].ends_with("the policy's answer goes now (the box's answer)"),
        "{}",
        journal[end]
    );
    let approved = journal
        .iter()
        .position(|l| l.starts_with("APPROVED "))
        .expect("approved");
    assert!(end < approved, "{journal:#?}");

    let (m, journal) = held_watch(
        at_the_margin(),
        Some(TYPED_BEFORE_MS),
        Build::CapOff,
        |_, _| {},
    );
    assert_eq!(
        ones(&m),
        0,
        "the hold outlasts the countdown: {:#?}",
        m.requests
    );
    let held = held_lines(&journal);
    assert_eq!(held.len(), 2, "{journal:#?}");
    assert!(
        held.iter()
            .all(|l| !l.contains("the vendor denies") && !l.contains("tick")),
        "{held:#?}"
    );
    assert!(!ended(&journal), "{journal:#?}");
}

/// The owner's incident (2026-10-25): a general-purpose subagent's `bash -c` rm box
/// (`f::shell_c_rm_box`) came up 6 s after the owner's last keystroke into the tab, and full
/// power's Yes was held for the rest of their 120 s grace (`HELD … a person is typing: hands off
/// until …`). Input older than a permission box nobody has touched holds it no longer than a
/// question (`QUESTION_PERSON_GRACE`), and input older than that not at all. A box a person
/// touched, or whose touch cannot be told, keeps the whole grace, as ever.
#[test]
fn an_untouched_box_is_held_by_older_input_no_longer_than_a_question() {
    use super::approval_loop::QUESTION_PERSON_GRACE;
    let shell_c = || f::shell_c_rm_box(" │ This shell -c script runs rm and could not be checked");
    let past = |stamp: &str| {
        aterm_types::rfc3339::parse_utc_fractional(stamp).expect("a stamp") - TEST_NOW
    };
    let bound = i64::try_from(QUESTION_PERSON_GRACE.as_secs()).expect("the bound");
    let six_s = 6_000;
    let quiet_past_the_bound = |m: &mut Mock, _: &mut SuperviseOpts| {
        m.status_human_from.insert(5, 16_000);
    };
    let (m, journal) = held_watch(
        watched(vec![shell_c(); 6]),
        Some(six_s),
        Build::Shipped,
        quiet_past_the_bound,
    );
    assert_eq!(
        ones(&m),
        1,
        "pressed once the bound ran out: {:#?}",
        m.requests
    );
    let held = held_lines(&journal);
    assert!(!held.is_empty(), "held inside the bound: {journal:#?}");
    let (until, deny) = held_times(held[0]);
    assert_eq!(deny, None, "{}", held[0]);
    assert!(
        (bound - 7..=bound - 6).contains(&past(&until)),
        "held to the bound, not the 120 s grace: {}",
        held[0]
    );
    assert!(theirs(&journal).is_empty(), "{journal:#?}");

    let (m, journal) = held_watch(
        watched(vec![shell_c(); 6]),
        Some(six_s),
        Build::BoundOff,
        quiet_past_the_bound,
    );
    assert_eq!(ones(&m), 0, "the whole grace: {:#?}", m.requests);
    let held = held_lines(&journal);
    let (until, _) = held_times(held[0]);
    assert!(past(&until) > 100, "{}", held[0]);

    let (m, journal) = held_watch(
        watched(vec![shell_c(); 2]),
        Some(TYPED_LONG_BEFORE_MS),
        Build::Shipped,
        |_, _| {},
    );
    assert_eq!(ones(&m), 1, "{:#?}", m.requests);
    assert!(held_lines(&journal).is_empty(), "{journal:#?}");

    let (m, journal) = held_watch(
        watched(vec![shell_c(); 3]),
        Some(TYPED_LONG_BEFORE_MS),
        Build::Shipped,
        |m, _| {
            m.human_seq_from.insert(2, 1);
        },
    );
    assert!(m.presses().is_empty(), "touched: {:#?}", m.requests);
    let held = held_lines(&journal);
    let (until, _) = held_times(held[0]);
    assert!(
        past(&until) > 90,
        "a touched box keeps the whole grace: {}",
        held[0]
    );

    let (m, _) = held_watch(
        [vec![shell_c(); 2], vec![busy_screen()]].concat(),
        Some(TYPED_LONG_BEFORE_MS),
        Build::Shipped,
        |_, _| {},
    );
    assert!(
        m.presses().is_empty(),
        "first read the box: {:#?}",
        m.requests
    );
}

#[test]
fn no_countdown_the_worker_drew_or_one_standing_still_ends_a_hold() {
    let run = |screens: Vec<Vec<String>>| {
        held_watch(
            watched(screens),
            Some(TYPED_BEFORE_MS),
            Build::Shipped,
            |_, _| {},
        )
    };
    let (m, journal) = run(vec![faked("0:14"); 3]);
    assert_eq!(ones(&m), 0, "{:#?}", m.requests);
    let held = held_lines(&journal);
    assert_eq!(held.len(), 3, "{journal:#?}");
    assert!(
        held.iter().all(|l| l.ends_with("(the box's answer)")),
        "{held:#?}"
    );
    assert!(!ended(&journal), "{journal:#?}");

    let (m, journal) = run(vec![incident("0:14"); 3]);
    assert_eq!(ones(&m), 0, "{:#?}", m.requests);
    let held = held_lines(&journal);
    assert_eq!(held.len(), 3, "{journal:#?}");
    assert!(
        held.iter()
            .all(|l| l.ends_with("read again to see it tick)")),
        "{held:#?}"
    );
    assert!(!ended(&journal), "{journal:#?}");

    let (m, _) = run(ticking(15, 2, incident));
    assert_eq!(ones(&m), 1, "{:#?}", m.requests);
}

#[test]
fn a_stall_between_two_reads_changes_nothing() {
    let (m, journal) = held_watch(
        watched(ticking(15, 2, incident)),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |m, _| {
            m.stall_after_read.insert(2, Duration::from_millis(4_500));
        },
    );
    assert_eq!(ones(&m), 1, "{:#?}", m.requests);
    assert!(ended(&journal), "{journal:#?}");
}

#[test]
fn a_box_a_person_touched_is_held_as_ever() {
    let down: Vec<Vec<String>> = watched(vec![
        incident("0:15"),
        moved_to_no(&incident("0:14")),
        moved_to_no(&incident("0:13")),
    ]);
    let worlds = [
        ("a Down onto No", down.clone(), Some(3), false),
        (
            "a scroll",
            watched(ticking(15, 3, incident)),
            Some(3),
            false,
        ),
        (
            "in the gap before the first read",
            watched(ticking(15, 3, incident)),
            Some(2),
            false,
        ),
        (
            "printed over once touched",
            watched(vec![
                incident("0:15"),
                printed_over(&moved_to_no(&incident("0:14"))),
                printed_over(&moved_to_no(&incident("0:13"))),
            ]),
            Some(3),
            false,
        ),
        (
            "attached mid-box",
            [ticking(15, 3, incident), vec![busy_screen()]].concat(),
            None,
            false,
        ),
        (
            "no person count",
            watched(ticking(15, 3, incident)),
            None,
            true,
        ),
    ];
    let tweak = |m: &mut Mock, counted: Option<usize>, no_count: bool| {
        if let Some(read) = counted {
            m.human_seq_from.insert(read, 1);
        }
        if no_count {
            m.human_seq = None;
        }
    };
    for (name, screens, counted, no_count) in &worlds {
        let (m, journal) = held_watch(
            screens.clone(),
            Some(TYPED_BEFORE_MS),
            Build::Shipped,
            |m, _| tweak(m, *counted, *no_count),
        );
        assert!(m.presses().is_empty(), "{name}: {:#?}", m.requests);
        assert!(!ended(&journal), "{name}: {journal:#?}");
        let theirs = theirs(&journal);
        assert!(!theirs.is_empty(), "{name}: {journal:#?}");
        assert!(
            theirs
                .iter()
                .all(|l| l.contains("a person is typing: hands off until ")),
            "{name}: {theirs:#?}"
        );
        assert!(
            theirs
                .iter()
                .any(|l| l.contains(": held as ever, and the vendor denies it at ")),
            "{name}: {theirs:#?}"
        );
        let (m, journal) = held_watch(
            screens.clone(),
            Some(TYPED_BEFORE_MS),
            Build::TouchOff,
            |m, _| tweak(m, *counted, *no_count),
        );
        assert!(ended(&journal), "{name}, touch off: {journal:#?}");
        let withheld = m
            .requests
            .iter()
            .any(|r| r.contains("the digit's read carried no person count to fence on"));
        assert_eq!(
            (ones(&m), withheld),
            (usize::from(!*no_count), *no_count),
            "{name}, touch off: {:#?}",
            m.requests
        );
    }
    let (_, journal) = held_watch(
        down.clone(),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |m, _| tweak(m, Some(3), false),
    );
    assert!(
        theirs(&journal).iter().any(|l| l.contains(
            "(the box's answer; a person's gesture was counted on the session since it appeared: \
             held as ever, and the vendor denies it at "
        )),
        "{journal:#?}"
    );
    let (_, journal) = held_watch(
        [ticking(15, 3, incident), vec![busy_screen()]].concat(),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |_, _| {},
    );
    assert!(
        theirs(&journal).iter().any(|l| l.contains(
            "whether a person touched it since it appeared cannot be told: held as ever"
        )),
        "{journal:#?}"
    );

    let (m, journal) = held_watch(
        watched(ticking(15, 3, incident)),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |m, _| m.human_seq = Some(4),
    );
    assert_eq!(ones(&m), 1, "untouched: {:#?}", m.requests);
    assert!(ended(&journal), "{journal:#?}");
    let (m, _) = held_watch(down, Some(TYPED_BEFORE_MS), Build::Shipped, |m, _| {
        m.human_seq_from.insert(3, 1);
        m.status_human_from
            .insert(3, u64::from(GRACE_S) * 1000 + 10_000);
    });
    assert_eq!(ones(&m), 1, "their grace over: {:#?}", m.requests);
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

#[test]
fn another_drivers_hand_on_the_session_is_a_touch() {
    let at_margin = || watched(ticking(17, 5, incident));
    let worlds = [
        ("lease", at_margin(), None, false),
        (
            "lease come and gone",
            at_margin(),
            Some(TYPED_BEFORE_MS),
            true,
        ),
        (
            "printed over",
            watched(vec![
                incident("0:16"),
                printed_over(&incident("0:15")),
                printed_over(&incident("0:14")),
            ]),
            None,
            false,
        ),
    ];
    for (name, screens, typed_ms, passing) in worlds {
        let world = |m: &mut Mock, _: &mut SuperviseOpts| {
            if passing {
                m.hand_from.insert(3, "lease:operator");
                m.hand_from.insert(5, "-");
            } else {
                m.hand = "lease:operator";
            }
        };
        let (m, journal) = held_watch(screens.clone(), typed_ms, Build::Shipped, world);
        assert!(
            m.presses().is_empty(),
            "{name}: {:#?}\n{journal:#?}",
            m.requests
        );
        assert!(!ended(&journal), "{name}: {journal:#?}");
        assert!(
            theirs(&journal).iter().any(|l| l.contains(
                "another driver's hand was on the session while it was up: held as ever, and \
                 the vendor denies it at "
            )),
            "{name}: {journal:#?}"
        );
        let (m, journal) = held_watch(screens, typed_ms, Build::TouchOff, world);
        assert_eq!(ones(&m), 1, "{name}, touch off: {:#?}", m.requests);
        assert!(ended(&journal), "{name}, touch off: {journal:#?}");
    }
    let (m, journal) = held_watch(at_margin(), None, Build::Shipped, |m, _| {
        m.hand = "lease:operator"
    });
    assert!(
        held_lines(&journal)
            .iter()
            .all(|l| l.contains("another driver's hand is on the session: hands off until ")),
        "{journal:#?}"
    );
    assert_eq!(ones(&m), 0, "{:#?}", m.requests);
    let (m, journal) = held_watch(
        at_margin(),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |_, _| {},
    );
    assert_eq!(ones(&m), 1, "the person alone: {:#?}", m.requests);
    assert!(ended(&journal), "{journal:#?}");
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
    assert!(ended(&journal), "{journal:#?}");
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
    for (name, screens) in [
        (
            "re-wrapped",
            watched(vec![
                incident("0:18"),
                incident("0:17"),
                rewrapped(&incident("0:16")),
                rewrapped(&incident("0:15")),
                rewrapped(&incident("0:14")),
            ]),
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
                subagent(100, "0:15"),
                subagent(100, "0:14"),
            ]),
        ),
    ] {
        let (m, journal) = held_watch(
            screens.clone(),
            Some(TYPED_BEFORE_MS),
            Build::Shipped,
            |_, _| {},
        );
        assert_eq!(ones(&m), 1, "{name}: {:#?}\n{journal:#?}", m.requests);
        assert!(ended(&journal), "{name}: {journal:#?}");
        assert!(theirs(&journal).is_empty(), "{name}: {journal:#?}");
        let (m, journal) = held_watch(screens, Some(TYPED_BEFORE_MS), Build::CapOff, |_, _| {});
        assert_eq!(ones(&m), 0, "{name}, cap off: {:#?}", m.requests);
        assert!(!ended(&journal), "{name}, cap off: {journal:#?}");
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

#[test]
fn a_countdown_a_narrow_pane_wraps_ends_the_hold_too() {
    let narrow = |left: &str| narrowed(&incident(left));
    let p = aterm_phase::parse_prompt_v2(&narrow("1:44")).expect("the box");
    assert_eq!(p.auto_deny_left(), Some(Duration::from_secs(104)), "{p:?}");
    assert!(p.rm_breaker().is_some(), "{p:?}");
    let run = |build: Build| {
        held_watch(
            watched(ticking(15, 2, narrow)),
            Some(TYPED_BEFORE_MS),
            build,
            |_, _| {},
        )
    };
    let (m, journal) = run(Build::Shipped);
    assert_eq!(ones(&m), 1, "{:#?}\n{journal:#?}", m.requests);
    assert!(ended(&journal), "{journal:#?}");
    let (m, journal) = run(Build::CapOff);
    assert_eq!(ones(&m), 0, "cap off: {:#?}", m.requests);
    assert!(!ended(&journal), "{journal:#?}");
}

#[test]
fn a_press_past_the_hold_is_fenced_on_the_person() {
    let digits = |m: &Mock| -> Vec<String> {
        m.presses()
            .into_iter()
            .filter(|p| p.ends_with(" 1"))
            .cloned()
            .collect()
    };
    let raced = |build: Build| {
        held_watch_ledgered(
            watched(ticking(15, 3, incident)),
            Some(TYPED_BEFORE_MS),
            build,
            |m, _| m.person_at_key = 1,
        )
    };
    let (m, journal, rows) = raced(Build::Shipped);
    let presses = digits(&m);
    assert_eq!(presses.len(), 1, "{:#?}", m.requests);
    assert!(presses[0].starts_with("key if-human=0 "), "{presses:#?}");
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
    assert!(
        theirs(&journal)
            .iter()
            .any(|l| l.contains("a person's gesture was counted")),
        "{journal:#?}"
    );
    let (m, _, _) = raced(Build::TouchOff);
    let presses = digits(&m);
    assert_eq!(presses.len(), 2, "touch off: {:#?}", m.requests);
    assert!(presses[1].starts_with("key if-human=1 "), "{presses:#?}");

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

#[test]
fn a_focus_press_past_the_hold_is_fenced_on_the_person() {
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
    let raced = |build: Build| {
        held_watch(
            watched(ticking(15, 3, yes)),
            Some(TYPED_BEFORE_MS),
            build,
            |m, _| {
                fenced_on(m);
                m.person_at_key = 1;
            },
        )
    };
    let (m, journal) = raced(Build::Shipped);
    assert_eq!(key_names(&m), ["enter"], "{:#?}", m.requests);
    assert!(
        m.presses()[0].starts_with("key if-human=0 "),
        "{:#?}",
        m.requests
    );
    assert_eq!(m.person_skipped.len(), 1, "{:#?}", m.requests);
    assert!(!theirs(&journal).is_empty(), "{journal:#?}");
    let (m, _) = raced(Build::TouchOff);
    assert_eq!(key_names(&m), ["enter", "enter"], "{:#?}", m.requests);

    let (m, _) = held_watch(watched(vec![yes("0:14")]), None, Build::Shipped, |m, _| {
        fenced_on(m)
    });
    let presses = m.presses();
    assert_eq!(presses.len(), 1, "{:#?}", m.requests);
    assert!(
        presses[0].ends_with(" enter") && !presses[0].contains("if-human="),
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
            .any(|l| l.contains("a person's gesture was counted")),
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

#[test]
fn a_gesture_between_a_focus_press_arrow_and_its_enter_skips_the_enter() {
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
        presses.iter().all(|p| p.starts_with("key if-human=0 ")),
        "{presses:#?}"
    );
    assert_eq!(m.person_skipped.len(), 1, "{:#?}", m.requests);
    assert!(ended(&journal), "{journal:#?}");
    assert!(
        theirs(&journal)
            .iter()
            .any(|l| l.contains("a person's gesture was counted")),
        "{journal:#?}"
    );
}

#[test]
fn a_box_asked_again_after_a_read_without_one_is_another_episode() {
    let screens = |moved: bool| {
        let b = |rows: Vec<String>| if moved { moved_to_no(&rows) } else { rows };
        watched(vec![
            incident("0:15"),
            incident("0:14"),
            busy_screen(),
            incident("0:16"),
            b(incident("0:15")),
            b(incident("0:14")),
            b(incident("0:13")),
        ])
    };
    let run = |moved: bool, build: Build| {
        held_watch(screens(moved), Some(TYPED_BEFORE_MS), build, |m, _| {
            if moved {
                m.human_seq_from.insert(6, 1);
            }
        })
    };
    let (m, journal) = run(true, Build::Shipped);
    assert_eq!(ones(&m), 1, "A only: {:#?}", m.requests);
    assert!(!theirs(&journal).is_empty(), "{journal:#?}");
    let (m, _) = run(false, Build::Shipped);
    assert_eq!(ones(&m), 2, "A and B: {:#?}", m.requests);
    let (m, _) = run(true, Build::TouchOff);
    assert_eq!(ones(&m), 2, "touch off: {:#?}", m.requests);
    let (m, journal) = held_watch(
        screens(false),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |m, _| {
            m.human_seq_from.insert(3, 1);
        },
    );
    assert_eq!(ones(&m), 1, "B only: {:#?}\n{journal:#?}", m.requests);
    assert!(!theirs(&journal).is_empty(), "A theirs: {journal:#?}");
    assert!(ended(&journal), "B's hold ended: {journal:#?}");
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
        held_watch(screens(false), Some(TYPED_BEFORE_MS), build, |m, _| {
            m.human_seq_from.insert(3, 1);
        })
    };
    let lease =
        |build: Build| held_watch(screens(true), None, build, |m, _| m.hand = "lease:operator");
    for (name, run, why) in [
        (
            "person",
            &person as &dyn Fn(Build) -> (Mock, Vec<String>),
            "a person's gesture was counted on the session since it appeared: held as ever",
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
        assert!(!ended(&journal), "{name}: {journal:#?}");
        let theirs = theirs(&journal);
        assert!(theirs.len() >= 6, "{name}: {journal:#?}");
        assert!(
            theirs.iter().all(|l| l.contains(why)),
            "{name}: {theirs:#?}"
        );
        let (m, journal) = run(Build::TouchOff);
        assert!(ones(&m) >= 1, "{name}, touch off: {:#?}", m.requests);
        assert!(ended(&journal), "{name}, touch off: {journal:#?}");
    }
    let (m, journal) = person(Build::EpisodePerBox);
    let digits: Vec<&String> = m
        .presses()
        .into_iter()
        .filter(|p| p.ends_with(" 1"))
        .collect();
    assert!(
        !digits.is_empty(),
        "episode per box: {:#?}\n{journal:#?}",
        m.requests
    );
    assert!(digits[0].starts_with("key if-human=1 "), "{digits:#?}");
    assert!(
        m.person_skipped.is_empty(),
        "pressed over the gesture: {:#?}",
        m.requests
    );
    assert!(ended(&journal), "{journal:#?}");
}

#[test]
fn an_episode_runs_from_the_first_box_to_the_first_read_without_one() {
    let read = |rows: Vec<String>, count: Option<u64>, human: HumanInput| Screen {
        rows,
        human,
        human_seq: count,
        cols: Some(144),
        ..Screen::default()
    };
    let seen = |rows: Vec<String>, count: u64| read(rows, Some(count), HumanInput::Ago(22_000));
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let mut last: Option<Option<u64>> = None;
    let mut step = |s: &mut Session<'_, Mock>, r: Screen| {
        s.keep_episode(&r, last);
        last = Some(r.human_seq);
        s.touched()
    };
    assert_eq!(
        step(&mut s, seen(incident("0:20"), 0)),
        Some("whether a person touched it since it appeared cannot be told"),
        "the session's first read"
    );
    assert_eq!(
        step(&mut s, seen(busy_screen(), 0)),
        None,
        "no box: the episode over"
    );
    for (why, rows) in [
        ("as opened", incident("0:19")),
        ("relabelled", stacked(&incident("0:18"), "1 of 2")),
        ("re-wrapped", rewrapped(&incident("0:17"))),
        ("its focus moved", moved_to_no(&incident("0:16"))),
        ("its footer re-cut", subagent(76, "0:15")),
        ("at another width", subagent(100, "0:14")),
    ] {
        assert_eq!(step(&mut s, seen(rows, 0)), None, "{why}");
    }
    const COUNTED: &str = "a person's gesture was counted on the session since it appeared";
    assert_eq!(step(&mut s, seen(incident("0:13"), 1)), Some(COUNTED));
    for (why, rows) in [
        ("relabelled", stacked(&incident("0:12"), "2 of 2")),
        ("re-wrapped", rewrapped(&incident("0:11"))),
        ("another drawn in its place", next_request("0:10")),
        ("at another width", subagent(100, "0:09")),
    ] {
        assert_eq!(step(&mut s, seen(rows, 1)), Some(COUNTED), "{why}");
    }
    assert_eq!(
        step(&mut s, seen(busy_screen(), 1)),
        None,
        "no box: the episode over"
    );
    assert_eq!(
        step(&mut s, seen(incident("0:30"), 1)),
        None,
        "another episode"
    );
    assert_eq!(
        s.person_fence(&seen(incident("0:30"), 1)),
        Some(1),
        "its baseline"
    );
    for (why, r, by) in [
        (
            "no count",
            read(incident("0:29"), None, HumanInput::Ago(22_000)),
            "whether a person touched it since it appeared cannot be told",
        ),
        (
            "no stamp",
            read(incident("0:29"), Some(1), HumanInput::Unknown),
            "whether a person touched it since it appeared cannot be told",
        ),
        (
            "started over",
            seen(incident("0:29"), 0),
            "the session's person count started over since it appeared, so whether a person \
             touched it cannot be told",
        ),
    ] {
        assert_eq!(step(&mut s, seen(busy_screen(), 1)), None, "{why}");
        assert_eq!(step(&mut s, seen(incident("0:30"), 1)), None, "{why}");
        assert_eq!(step(&mut s, r), Some(by), "{why}");
        assert_eq!(
            s.person_fence(&seen(incident("0:28"), 7)),
            Some(7),
            "{why}: a touched episode is fenced on the read decided on"
        );
    }
    assert_eq!(step(&mut s, seen(busy_screen(), 1)), None);
    s.driver = true;
    assert_eq!(
        step(&mut s, seen(incident("0:30"), 1)),
        None,
        "a hand the status read before the box found"
    );
    s.episode_driven();
    assert_eq!(
        s.touched(),
        Some("another driver's hand was on the session while it was up"),
        "a status read under way"
    );

    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_episode_per_box();
    let mut last: Option<Option<u64>> = Some(Some(0));
    let mut step = |s: &mut Session<'_, Mock>, r: Screen| {
        s.keep_episode(&r, last);
        last = Some(r.human_seq);
        s.touched()
    };
    assert_eq!(step(&mut s, seen(incident("0:20"), 0)), None);
    assert_eq!(step(&mut s, seen(incident("0:19"), 1)), Some(COUNTED));
    assert_eq!(
        step(&mut s, seen(stacked(&incident("0:18"), "1 of 2"), 1)),
        None,
        "episode per box: the touch lost"
    );
}

#[test]
fn a_person_count_that_started_over_is_a_touch() {
    let (m, journal) = held_watch(
        watched(ticking(17, 5, incident)),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |m, _| {
            m.human_seq_from.insert(1, 57);
            m.human_seq_from.insert(3, 0);
            m.human_seq_from.insert(4, 1);
        },
    );
    assert_eq!(ones(&m), 0, "{:#?}\n{journal:#?}", m.requests);
    assert!(
        theirs(&journal).iter().any(|l| l.contains(
            "the session's person count started over since it appeared, so whether a person \
             touched it cannot be told: held as ever"
        )),
        "{journal:#?}"
    );
    assert!(!ended(&journal), "{journal:#?}");
}

#[test]
fn a_question_dialog_s_tabs_are_one_episode() {
    let keys = |lease: bool, counted: bool, build: Build| {
        let (jdir, journal) = journal_file("box-hold-tab");
        let tab = f::screen(f::QUESTION_TABS_FOURTH);
        let mut m = questioned(watched(
            [vec![tab.clone(), tab], ticking(15, 3, review_ticking)].concat(),
        ));
        if lease {
            m.hand = "lease:operator";
        } else {
            m.human = vec![QUESTION_TYPED_BEFORE];
            m.human_ms = Some(QUESTION_TYPED_BEFORE_MS);
        }
        m.human_seq = Some(0);
        if counted {
            m.human_seq_from.insert(3, 1);
        }
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
    for (name, lease, counted, why) in [
        (
            "person",
            false,
            true,
            "a person's gesture was counted on the session since it appeared: held as ever",
        ),
        (
            "lease",
            true,
            false,
            "another driver's hand was on the session while it was up: held as ever",
        ),
    ] {
        let (pressed, journal) = keys(lease, counted, Build::Shipped);
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

struct World {
    name: &'static str,
    screens: Vec<Vec<String>>,
    watched: bool,
    typed_ms: Option<u64>,
    lease: bool,
    lease_before: bool,
    answers: bool,
    counts: &'static [(usize, u64)],
    no_count: bool,
    grace_over_from: Option<usize>,
    vendor_s: Option<u64>,
    known: bool,
    counted: bool,
    blind: bool,
}

fn worlds() -> Vec<World> {
    let person = Some(TYPED_BEFORE_MS);
    let ticks = |name, from: u64, typed_ms, answers| World {
        name,
        screens: ticking(from, 2, incident),
        watched: true,
        typed_ms,
        lease: false,
        lease_before: false,
        answers,
        counts: &[],
        no_count: false,
        grace_over_from: None,
        vendor_s: Some(from - 1),
        known: true,
        counted: false,
        blind: false,
    };
    vec![
        ticks("1:44 person", 104, person, true),
        ticks("0:29 person", 29, person, true),
        ticks("0:15 person", 15, person, true),
        World {
            screens: vec![incident("1:44")],
            vendor_s: Some(104),
            known: false,
            ..ticks("1:44 nobody", 104, None, true)
        },
        ticks("0:15 escalated", 15, person, false),
        World {
            screens: vec![incident("about 2 minutes"); 2],
            vendor_s: Some(120),
            known: false,
            ..ticks("unread person", 120, person, true)
        },
        World {
            screens: vec![faked("0:14"); 2],
            vendor_s: None,
            known: false,
            ..ticks("faked person", 14, person, true)
        },
        World {
            screens: vec![incident("0:14"); 2],
            vendor_s: None,
            known: false,
            ..ticks("standing person", 14, person, true)
        },
        World {
            counts: &[(3, 1)],
            counted: true,
            ..ticks("touched person", 15, person, true)
        },
        World {
            counts: &[(2, 1)],
            counted: true,
            ..ticks("touched in the gap", 15, person, true)
        },
        World {
            screens: ticking(30, 3, incident),
            vendor_s: Some(28),
            counts: &[(3, 1)],
            grace_over_from: Some(4),
            counted: true,
            ..ticks("touched, grace over", 30, person, true)
        },
        World {
            watched: false,
            blind: true,
            ..ticks("first read the box", 15, person, true)
        },
        World {
            screens: ticking(15, 2, |left| unnumbered(&incident(left))),
            no_count: true,
            blind: true,
            ..ticks("no person count", 15, person, true)
        },
        World {
            counts: &[(1, 57), (3, 0)],
            blind: true,
            ..ticks("count started over", 15, person, true)
        },
        World {
            screens: vec![
                incident("0:15"),
                moved_to_no(&incident("0:14")),
                moved_to_no(&incident("0:13")),
            ],
            lease: true,
            ..ticks("lease, focus moved", 15, None, true)
        },
        World {
            lease: true,
            ..ticks("lease", 15, None, true)
        },
        World {
            screens: vec![
                incident("0:17"),
                incident("0:16"),
                stacked(&incident("0:15"), "1 of 2"),
                stacked(&incident("0:14"), "1 of 2"),
                stacked(&incident("0:13"), "1 of 2"),
            ],
            vendor_s: Some(14),
            counts: &[(3, 1)],
            counted: true,
            ..ticks("relabelled once touched", 17, person, true)
        },
        World {
            screens: vec![
                subagent(72, "0:17"),
                subagent(72, "0:16"),
                subagent(76, "0:15"),
                subagent(76, "0:14"),
                subagent(76, "0:13"),
            ],
            vendor_s: Some(14),
            ..ticks("footer re-cut", 17, person, true)
        },
        World {
            lease_before: true,
            ..ticks("lease gone before the box", 15, person, true)
        },
        ticks("1:44 older person", 104, Some(TYPED_LONG_BEFORE_MS), true),
        World {
            counts: &[(2, 1)],
            counted: true,
            ..ticks(
                "touched, older person",
                104,
                Some(TYPED_LONG_BEFORE_MS),
                true,
            )
        },
    ]
}

fn pressed_in(w: &World, build: Build) -> bool {
    let screens = if w.lease_before {
        lease_before(w.screens.clone())
    } else if w.watched {
        watched(w.screens.clone())
    } else {
        [w.screens.clone(), vec![busy_screen()]].concat()
    };
    let (answers, lease, lease_before, counts, no_count, grace_over_from) = (
        w.answers,
        w.lease,
        w.lease_before,
        w.counts,
        w.no_count,
        w.grace_over_from,
    );
    let (m, _) = held_watch(screens, w.typed_ms, build, |m, opts| {
        if !answers {
            opts.policy.approve = Approve::None;
        }
        if lease {
            m.hand = "lease:operator";
        }
        if lease_before {
            let_go_before(m, opts);
        }
        if no_count {
            m.human_seq = None;
        }
        for &(from, n) in counts {
            m.human_seq_from.insert(from, n);
        }
        if let Some(from) = grace_over_from {
            m.status_human_from
                .insert(from, u64::from(GRACE_S) * 1000 + 10_000);
        }
    });
    !m.presses().is_empty()
}

fn box_hold_violations(build: Build) -> (Vec<String>, Vec<bool>) {
    let model = aterm_models_harness::supervisor_box_hold_model();
    let window = model
        .consts
        .iter()
        .find(|(k, _)| *k == "Window")
        .map(|(_, v)| *v)
        .expect("the window");
    let margin = approval_loop::DENY_MARGIN.as_secs();
    let mut bad = Vec::new();
    let mut presses = Vec::new();
    for w in worlds() {
        let pressed = pressed_in(&w, build);
        presses.push(pressed);
        let mut st = model.init_state();
        let latched = w.counted || w.lease || w.blind;
        let bound_ms = if latched {
            u64::from(GRACE_S) * 1000
        } else {
            u64::try_from(approval_loop::QUESTION_PERSON_GRACE.as_millis()).unwrap_or(u64::MAX)
        };
        let grace = w.grace_over_from.is_none() && w.typed_ms.is_some_and(|ms| ms < bound_ms);
        for (k, v) in [
            ("up", 1),
            (
                "left",
                w.vendor_s
                    .map_or(window, |s| i64::try_from(s.div_ceil(margin)).unwrap_or(0)),
            ),
            ("known", i64::from(w.known)),
            ("policy", i64::from(w.answers)),
            ("grace", i64::from(grace)),
            ("driver", i64::from(w.lease)),
            ("counted", i64::from(w.counted)),
            ("driven", i64::from(w.lease)),
            ("blind", i64::from(w.blind)),
            ("latched", i64::from(latched)),
        ] {
            st.insert(k, v);
        }
        let action = if pressed { "Press" } else { "Tick" };
        if !model.action_enabled(action, &st) {
            bad.push(format!("{}: {action} at {st:?}", w.name));
        }
    }
    (bad, presses)
}

#[test]
fn tier1_a_hold_goes_only_where_the_model_allows() {
    let names = |bad: &[String]| -> Vec<String> {
        bad.iter()
            .map(|b| b.split_once(": ").map_or("", |(n, _)| n).to_string())
            .collect()
    };
    let shipped = [
        false, false, true, true, false, false, false, false, false, false, true, false, false,
        false, false, false, false, true, true, true, false,
    ];
    let (bad, presses) = box_hold_violations(Build::Shipped);
    assert_eq!(bad, Vec::<String>::new());
    assert_eq!(presses, shipped);

    let (bad, presses) = box_hold_violations(Build::BoundOff);
    let mut want = shipped;
    want[19] = false;
    assert_eq!(presses, want);
    assert_eq!(names(&bad), ["1:44 older person"], "{bad:#?}");
    assert!(bad.iter().all(|b| b.contains(": Tick at ")), "{bad:#?}");

    let (bad, presses) = box_hold_violations(Build::CapOff);
    let mut want = shipped;
    want[2] = false;
    want[17] = false;
    want[18] = false;
    assert_eq!(presses, want);
    assert_eq!(
        names(&bad),
        ["0:15 person", "footer re-cut", "lease gone before the box"],
        "{bad:#?}"
    );
    assert!(bad.iter().all(|b| b.contains(": Tick at ")), "{bad:#?}");

    let (bad, presses) = box_hold_violations(Build::TouchOff);
    let mut want = shipped;
    for i in [8, 9, 11, 12, 13, 14, 15, 16, 20] {
        want[i] = true;
    }
    assert_eq!(presses, want);
    assert_eq!(
        names(&bad),
        [
            "touched person",
            "touched in the gap",
            "first read the box",
            "no person count",
            "count started over",
            "lease, focus moved",
            "lease",
            "relabelled once touched",
            "touched, older person"
        ],
        "{bad:#?}"
    );
    assert!(bad.iter().all(|b| b.contains(": Press at ")), "{bad:#?}");

    let (bad, presses) = box_hold_violations(Build::EpisodePerBox);
    let mut want = shipped;
    want[16] = true;
    assert_eq!(presses, want);
    assert_eq!(names(&bad), ["relabelled once touched"], "{bad:#?}");
    assert!(bad.iter().all(|b| b.contains(": Press at ")), "{bad:#?}");
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
    let keys = |screens: Vec<Vec<String>>, build: Build, counted: bool| {
        let mut m = questioned(screens);
        m.human = vec![QUESTION_TYPED_BEFORE];
        m.human_ms = Some(QUESTION_TYPED_BEFORE_MS);
        m.human_seq = Some(0);
        m.vanish_after = Some(0);
        if counted {
            m.human_seq_from.insert(3, 1);
        }
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
            Build::Shipped,
            false
        )),
        1,
        "answered before the deny"
    );
    assert_eq!(
        enters(&keys(
            watched(ticking(104, 2, review_ticking)),
            Build::Shipped,
            false
        )),
        0,
        "held for the person above the margin"
    );
    assert_eq!(
        enters(&keys(
            watched(ticking(15, 2, review_ticking)),
            Build::CapOff,
            false
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
    assert!(
        keys(watched(ticking(15, 3, skipped_to)), Build::Shipped, true).is_empty(),
        "a person's review"
    );
    assert_eq!(
        enters(&keys(
            watched(ticking(15, 3, skipped_to)),
            Build::TouchOff,
            true
        )),
        1,
        "touch off"
    );
    let cancel = |left: &str| focused_on(&review_ticking(left), "2. Cancel");
    let attached = || [ticking(15, 3, cancel), vec![busy_screen()]].concat();
    assert!(
        keys(attached(), Build::Shipped, false).is_empty(),
        "attached mid-dialog"
    );
    assert_eq!(
        keys(attached(), Build::TouchOff, false)
            .first()
            .map(String::as_str),
        Some("up"),
        "touch off"
    );
}

#[test]
fn a_gesture_counted_inside_the_answer_past_the_hold_stops_it() {
    let cancel = |left: &str| focused_on(&review_ticking(left), "2. Cancel");
    let screens = watched(vec![
        cancel("0:15"),
        cancel("0:14"),
        review_ticking("0:13"),
        review_ticking("0:12"),
        review_ticking("0:11"),
    ]);
    let (jdir, journal) = journal_file("box-hold-answer");
    let mut m = questioned(screens);
    m.human = vec![QUESTION_TYPED_BEFORE];
    m.human_ms = Some(QUESTION_TYPED_BEFORE_MS);
    m.human_seq = Some(0);
    m.human_seq_from.insert(4, 1);
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
    });
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&jdir);
    let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
    assert_eq!(key_names(&m), ["up", "enter"], "{:#?}", m.requests);
    assert!(
        m.presses()[1].starts_with("key if-human=0 "),
        "{:#?}",
        m.requests
    );
    assert_eq!(m.person_skipped.len(), 1, "{:#?}", m.requests);
    assert!(!theirs(&journal).is_empty(), "{journal:#?}");
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

#[test]
fn a_gesture_counted_inside_the_decline_past_the_hold_stops_it() {
    let states = [0usize, 0, 0, 0, 2, 2, 3, 3, 3, 3];
    let (w, journal) = tall_decline(17, &states, Build::Shipped, &|m| {
        m.human_seq_from.insert(6, 1);
    });
    assert_eq!(w.len(), 2, "{w:#?}\n{journal:#?}");
    assert!(w[0].ends_with(" down"), "{w:#?}");
    assert!(w[1].starts_with("send if-human=0 "), "{w:#?}");
    assert!(!w.iter().any(|k| k.ends_with(" enter")), "{w:#?}");
    assert!(!theirs(&journal).is_empty(), "{journal:#?}");
    assert!(
        !journal.iter().any(|l| l.starts_with("DECLINED ")),
        "{journal:#?}"
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
fn a_countdown_ticks_on_through_its_box_s_redraws() {
    let at_margin = watched(vec![
        incident("0:17"),
        incident("0:16"),
        stacked(&incident("0:15"), "1 of 2"),
        stacked(&incident("0:14"), "1 of 2"),
        stacked(&incident("0:13"), "1 of 2"),
    ]);
    let (m, journal) = held_watch(at_margin, Some(TYPED_BEFORE_MS), Build::Shipped, |_, _| {});
    let looks = |journal: &[String]| {
        held_lines(journal)
            .into_iter()
            .filter(|l| l.contains("read again to see it tick"))
            .count()
    };
    let ends = |journal: &[String]| {
        journal
            .iter()
            .filter(|l| l.starts_with("HOLD-ENDED "))
            .count()
    };
    assert_eq!(ones(&m), 1, "{:#?}", m.requests);
    assert_eq!(looks(&journal), 1, "{journal:#?}");
    assert_eq!(ends(&journal), 1, "{journal:#?}");

    let skipped = watched(vec![
        incident("0:16"),
        incident("0:15"),
        stacked(&incident("0:14"), "1 of 2"),
        stacked(&incident("0:13"), "1 of 2"),
        stacked(&incident("0:12"), "1 of 2"),
    ]);
    let (m, journal) = held_watch(skipped, Some(TYPED_BEFORE_MS), Build::Shipped, |m, _| {
        m.tick_at_key = 1;
    });
    assert_eq!(ones(&m), 2, "{:#?}", m.requests);
    assert_eq!(looks(&journal), 1, "{journal:#?}");
    assert_eq!(ends(&journal), 1, "{journal:#?}");
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
