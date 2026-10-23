// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_phase::prompt::fixtures as f;

/// How long before the box came up the person typed, in the incident.
const TYPED_BEFORE_MS: u64 = 22_000;
/// The incident's grace: `[harness] human_grace_s`'s default.
const GRACE_S: u32 = 120;

/// `rows` with every row equal to `from` replaced by the rows `to` — each
/// replacement asserted to have happened once.
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

/// `rows` with the countdown's `M:SS` made `left` (any text: a form the
/// reader does not read, too).
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

/// `secs` as the countdown draws it (`1:44`).
fn mss(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// `reads` reads of the box `box_at` draws, its countdown TICKING from
/// `from` seconds — one second lower each read, as Claude Code redraws it
/// every second and the loop reads it a second apart.
fn ticking(from: u64, reads: u64, box_at: impl Fn(&str) -> Vec<String>) -> Vec<Vec<String>> {
    (0..reads).map(|k| box_at(&mss(from - k))).collect()
}

/// `screens` as a loop watching the worker reads them: the worker busy
/// first — the read before the box's first, which the touch rule counts a
/// person's gestures from — then the box, then the worker busy again.
fn watched(screens: Vec<Vec<String>>) -> Vec<Vec<String>> {
    [vec![busy_screen()], screens, vec![busy_screen()]].concat()
}

/// THE INCIDENT'S BOX, COMPOSED from the measured 2.1.301 rm box (the lab,
/// 2026-10-15: its dashed layout, its two blank rows under the countdown):
/// the call, the command and the breaker's note made the incident's (`rm -f
/// "$O"/*`, the note in the words 2.1.301's breaker builds for it), its
/// countdown at `left`.
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

/// The incident's box with its focus moved to `2. No`, as a Down leaves it.
fn moved_to_no(rows: &[String]) -> Vec<String> {
    swapped(
        rows,
        &[(" ❯ 1. Yes", &["   1. Yes"]), ("   2. No", &[" ❯ 2. No"])],
    )
}

/// The incident's box with NO countdown of the vendor's, its Bash call's
/// description written by the worker to read as one at `left` — `⚠` and all
/// (a prompt-injected worker, or one writing about this very feature).
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

/// `rows` with a row of the transcript above the box's echo printed over, as
/// the main thread prints over a subagent's box (`⏺ Agent "lint"
/// finished`): outside the box's span.
fn printed_over(rows: &[String]) -> Vec<String> {
    swapped(
        rows,
        &[(
            "✻ Cooked for 0s · done 1:29 PM",
            &["⏺ Agent \"lint\" finished"],
        )],
    )
}

/// The incident's box is read as the incident's: an rm breaker's Bash box,
/// its command and note, and its countdown's 1:44.
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

/// A COUNTDOWN IS BELIEVED ONCE SEEN TICKING (`approval_loop::Countdown`), on
/// the loop's clock (instants made up here, a read's `asked`): seen once it
/// is not believed, but may still tick; a second on, a second lower, it is,
/// the deny placed by that read; the same time read again within its second
/// keeps that placing; a time standing past the slack has stopped, and is
/// believed no longer.
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
}

/// One `watch` over `screens` under full power, built `build`, a person who
/// typed `typed_ms` before every read and `status` (`None`: nobody), the
/// server's person count `0` on every read unless `tweak` moves it, each
/// hold looked at again at once ([`Session::set_person_hold_wait`]) and an
/// outage probed again within milliseconds.
fn held_watch(
    screens: Vec<Vec<String>>,
    typed_ms: Option<u64>,
    build: Build,
    tweak: impl FnOnce(&mut Mock, &mut SuperviseOpts),
) -> (Mock, Vec<String>) {
    let (m, journal, _) = held_watch_ledgered(screens, typed_ms, build, tweak);
    (m, journal)
}

/// [`held_watch`], and the approval ledger's rows.
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

/// `s` built as `build` says.
fn built<C: Ctl>(s: &mut Session<'_, C>, build: Build) {
    match build {
        Build::Shipped => {}
        Build::CapOff => s.set_deny_cap_off(),
        Build::TouchOff => s.set_touch_off(),
        Build::EpisodePerBox => s.set_episode_per_box(),
    }
}

/// The digit presses a run made (`key … 1`).
fn ones(m: &Mock) -> usize {
    m.presses().iter().filter(|p| p.ends_with(" 1")).count()
}

/// The journal's `HELD` lines.
fn held_lines(journal: &[String]) -> Vec<&String> {
    journal.iter().filter(|l| l.starts_with("HELD ")).collect()
}

/// The `HELD` lines that hold a box as its toucher's: held as ever.
fn theirs(journal: &[String]) -> Vec<&String> {
    held_lines(journal)
        .into_iter()
        .filter(|l| l.contains(": held as ever"))
        .collect()
}

/// Whether the journal said a hold's end.
fn ended(journal: &[String]) -> bool {
    journal.iter().any(|l| l.starts_with("HOLD-ENDED "))
}

/// The UTC stamps a `HELD` line names: its hold's end and, on a box with a
/// countdown, the vendor's deny.
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

/// The keys a run pressed, by their last word (`down`, `enter`, `1`).
fn key_names(m: &Mock) -> Vec<&str> {
    m.presses()
        .iter()
        .map(|p| p.rsplit_once(' ').map_or("", |(_, k)| k))
        .collect()
}

/// THE INCIDENT: a person who typed 22 s before the box came up (their
/// grace, 120 s, runs 98 s past it), the loop watching the busy worker when
/// it did — so their keystroke is before the read before the box's first,
/// and the box is untouched.
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
    assert!((30..=88).contains(&until), "{until}: {}", held[1]);
    assert!((45..=103).contains(&deny), "{deny}: {}", held[1]);
    assert!((14..=16).contains(&(deny - until)), "{}", held[1]);
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

/// NO TEXT OF THE WORKER'S ENDS A HOLD: under the person's hand, a box whose
/// countdown is the worker's own description (`⚠ Claude Code will
/// automatically deny this request in 0:14, …`, the vendor's own countdown
/// absent) is read as no countdown at all, and held as ever — no
/// `HOLD-ENDED`, no press, no deny named; a countdown at the vendor's place
/// that stands still at 0:14 read after read is never believed — held, the
/// box read again to see it tick, never pressed.
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

/// A STALL BETWEEN TWO READS CHANGES NOTHING: the scripted server's clock is
/// its reads (`Session::set_read_clock`: every read a second after the one
/// before it), so the incident's box read at 0:15 and then, the run held up
/// 4.5 s on its clock ([`Mock::stall_after_read`]), at 0:14 is seen ticking
/// as on an idle machine — `HOLD-ENDED`, and `1` before the deny — and no
/// test of the hold rests on how long its thread took between two reads.
#[test]
fn a_stall_between_two_reads_changes_nothing() {
    let (m, journal) = held_watch(
        watched(ticking(15, 2, incident)),
        Some(TYPED_BEFORE_MS),
        Build::Shipped,
        |m, _| {
            // The box's first read is the second read: the busy screen's is
            // the first.
            m.stall_after_read.insert(2, Duration::from_millis(4_500));
        },
    );
    assert_eq!(ones(&m), 1, "{:#?}", m.requests);
    assert!(ended(&journal), "{journal:#?}");
}

/// A BOX A PERSON TOUCHED SINCE IT APPEARED IS THEIRS, HELD AS EVER: the
/// incident's box ticking into the margin under the person's grace, and a
/// gesture of theirs the server counted after the read before the box's
/// first (`human_seq` 0, then 1) — every look holds it to their grace's end
/// (`HELD … (the box's answer; a person's gesture was counted on the session
/// since it appeared: held as ever, and the vendor denies it at …)`): no
/// `HOLD-ENDED`, no key, and the vendor's deny stands for their unfinished
/// answer.
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

/// OPERATOR.md's lease flow, come and gone BEFORE the box `screens` reads:
/// the worker's turn ends after work, nobody at the keyboard, and the
/// harness's turn-end look reads `status` under an operator's lease
/// (`hand=lease:operator`: the continuation is held); the operator sends work
/// and lets the lease go, and the worker runs busy, read with no `status`; a
/// person types (the box's reads carry their stamp, [`TYPED_BEFORE_MS`] old:
/// give it to [`held_watch`]); then the box, the worker busy again after it.
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

/// The world of [`lease_before`]: the continue policy on; the lease on every
/// `status` until the third read (the worker busy again), gone from it on;
/// the person's last keystroke ten minutes old on every read and `status`
/// until then.
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

/// ANOTHER DRIVER'S HAND ON THE SESSION IS A TOUCH: a drive lease on the
/// session (`status hand=lease:operator`), nobody's keystroke counted, the
/// box ticking into the margin as it opened — the driver's keys carry no
/// count, so their hand is the fact the server records.
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

/// The incident's box as Claude Code redraws it in a narrower pane (100
/// columns, a split or a resize): its rules shortened to the width, its note
/// and its countdown each wrapped onto a second row.
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

/// A REDRAW IS NO TOUCH (the box's face — its letters and focus, its footer
/// among them — read as a touch when it changed with no key of the loop's,
/// and Claude Code redraws it by itself, so an untouched box was held as
/// touched and denied).
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

/// The incident's box with its countdown wrapped as Ink wraps it in a pane
/// 48 columns wide, through its very words: `deny this` ends the `⚠` row,
/// `request in M:SS, …` and `an unattended session` each a row of their own
/// at its column.
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

/// A COUNTDOWN A NARROW PANE WRAPS THROUGH ITS WORDS ENDS THE HOLD TOO (its
/// words were once looked for whole on one row, so in a pane narrower than
/// ~55 cells no countdown was read, and the hold ran into the vendor's deny).
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

/// PAST THE HOLD A PERSON STILL WINS THE RACE, AND THE BOX IS THEIRS: the
/// press that goes although a person's hand is on the session carries the
/// person fence (`key if-human=<the episode's baseline count> …`), so a person who keys
/// between the read and the press skips it (`OK skipped reason=person`,
/// ledgered `decided again after their quiet`) — and that key came after the
/// box appeared, so the box is theirs from the next read on: held as ever,
/// never pressed again.
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

/// `rows` with the box's options drawn without their numbers, as an
/// unnumbered (arrows-and-Enter) box draws them — HAND-BUILT: no Claude Code
/// box measured today is both unnumbered and carries a countdown.
fn unnumbered(rows: &[String]) -> Vec<String> {
    swapped(
        rows,
        &[(" ❯ 1. Yes", &[" ❯ Yes"]), ("   2. No", &["   No"])],
    )
}

/// The mock fences on the generation and the person, as a current host.
fn fenced_on(m: &mut Mock) {
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
    m.human_seq = Some(0);
}

/// PAST THE HOLD A FOCUS PRESS IS FENCED ON THE PERSON TOO: the incident's
/// box unnumbered (its answer `Yes` by the focus and Enter, `Choice::Focus`),
/// ticking at the margin, a person's hand on the session.
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

/// The irreversible tool's default-No `Tool use` box (unnumbered, `No`
/// first and focused, as Claude Code opens it) with an auto-deny countdown
/// at `left` over its question — HAND-BUILT — its focus on `Yes` when
/// `on_yes`, where the loop's own `down` leaves it.
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

/// A PERSON'S KEY AFTER THE LOOP'S OWN ARROW MAKES THE BOX THEIRS (review a
/// person's move back onto the box's first option after the loop's arrow
/// moved it away was once pressed over).
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

/// A GESTURE BETWEEN A FOCUS PRESS'S ARROW AND ITS ENTER SKIPS THE ENTER (the
/// test above scripts the Enter's skip, so none would fail if the Enter lost
/// its fence).
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

/// A BOX ASKED AGAIN AFTER A READ WITHOUT ONE IS ANOTHER EPISODE: box A, the
/// incident's, ticking into the margin under a person's grace and untouched,
/// gets its `1` at `HOLD-ENDED`; the worker works (a busy read: no box, the
/// episode over), and asks the very same again — box B, its countdown seen
/// ticking afresh, its episode's baseline the busy read's count — and the
/// person presses Down on it, their gesture counted: B is theirs, held as
/// ever, no second `1`; B left as it opened gets its own `1`.
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

/// The incident's box with another command in it, its countdown at `left`:
/// the next request of parallel tool calls.
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

/// A TOUCHED BOX RELABELLED, OR REPLACED, STAYS THEIRS (a box was once
/// re-judged afresh when it asked otherwise, and Claude Code's stacked-asks
/// label on its title row — `1 of 2` once a second ask stacks behind it, `2
/// of 2` once the other settles — made it ask otherwise, so the touched box
/// was judged from a count that already held the gesture, and `1` was pressed
/// over it).
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

/// AN EPISODE RUNS FROM THE FIRST READ THAT SHOWS A BOX TO THE FIRST THAT
/// SHOWS NONE, ITS TOUCH LATCHED (`Session::keep_episode`), read by read over
/// reads made up here.
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

/// A PERSON COUNT THAT STARTED OVER IS A TOUCH (a gesture was once read as a
/// touch only on a count higher than the one before the box, and a session
/// adopted across a seamless update counts from 0 again).
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

/// A QUESTION DIALOG'S TABS ARE ONE EPISODE: a four-question dialog on its
/// last question tab under a person's grace, their gesture counted on the
/// tab's second read; they answer it, and Claude Code draws the review tab,
/// with a countdown, in the tab's place — no read between without a box, so
/// the same episode, touched: held as ever, never submitted.
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
            m.human = vec!["22000"];
            m.human_ms = Some(TYPED_BEFORE_MS);
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

/// One world of the Tier-1 bind ([`box_hold_violations`]): the reads of the
/// box (the worker busy before them when `watched`; else the loop's first
/// read is the box), the person's last keystroke before every read and
/// `status` (`None`: nobody), another driver's lease on the session, a lease
/// come and gone before the box ([`lease_before`], which the model sees as
/// no hand on the box), whether the policy answers the box (`false`:
/// `approve = "none"`, it escalates), the person counts the server gives
/// from a read on (`human_seq`; none at all when `no_count`), the read from
/// which their grace is over (`status human_ms=` past it), and the world as
/// the model sees it at the deciding look: the vendor's countdown there (its
/// seconds; `None`: none of the vendor's ever ticks there — the worker's own
/// text, or one standing still), whether the loop knows the deny there (read
/// and seen ticking), and the facts of the episode — a gesture counted past
/// its baseline, and whether its touch cannot be told.
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

/// The worlds: the incident's box ticking at 1:44, 0:29 (two margins: held,
/// 13 s short of the margin) and 0:15 under a person's grace, untouched; at
/// 1:44 with nobody about (pressed on its first read); escalated at 0:15
/// under the person; its countdown in the screen-reader form (`in about 2
/// minutes`, which the reader does not read); the worker's own text for a
/// countdown; a countdown standing still at 0:14; touched at 0:15 by a
/// person's gesture counted on its second read, and in the gap before its
/// first; touched, their grace then over; the box the loop's first read
/// showed; reads with no person count, the box unnumbered (no digit goes on
/// such a read, whatever the hold says: its focus and Enter do); a count that
/// started over; under a lease, its focus moved by the driver, and as it
/// opened; touched, then relabelled `1 of 2`; a subagent's box resized, its
/// footer re-cut, untouched; a lease let go before the box came up.
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
    ]
}

/// Whether the loop built `build` pressed the world's box at its look: any
/// key on it.
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

/// TIER-1 for `SupervisorBoxHold` (aterm-spec `supervisor_box_hold_model`):
/// each world's deciding look projected onto the model — the episode up; the
/// vendor's countdown in margins (`ceil(left / DENY_MARGIN)`; a box with
/// none that ticks is projected with the whole window); whether the loop
/// knows the deny; whether the policy answers it; a person's grace (their
/// keystroke within [`GRACE_S`] and not over) and another driver's hand (a
/// lease); the episode's facts — a gesture counted, the driver's hand on it,
/// a touch that cannot be told — and the latch they set — and checked
/// against it: a look that PRESSED must find `Press` enabled, a look that
/// held must find time free to pass (`Tick`).
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
        let grace = w.grace_over_from.is_none()
            && w.typed_ms.is_some_and(|ms| ms < u64::from(GRACE_S) * 1000);
        let latched = w.counted || w.lease || w.blind;
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

/// The shipped loop goes only where the model allows: it holds the untouched
/// box above the margin under the person's grace (1:44, 0:29), presses it at
/// the margin with their grace still running (0:15, the subagent's box
/// resized, its footer re-cut, and the box a lease let go before it came
/// up), presses at once with nobody about, presses no
/// box it escalates, keeps the hold on a countdown it does not read, on the
/// worker's own text and on a countdown standing still, holds every touched
/// episode as ever — touched on its second read, in the gap before its first,
/// the box its first read showed, reads with no count, a count started over,
/// a driver's lease, the box relabelled `1 of 2` once touched — and presses
/// the touched box once their grace is over.
#[test]
fn tier1_a_hold_goes_only_where_the_model_allows() {
    let names = |bad: &[String]| -> Vec<String> {
        bad.iter()
            .map(|b| b.split_once(": ").map_or("", |(n, _)| n).to_string())
            .collect()
    };
    let shipped = [
        false, false, true, true, false, false, false, false, false, false, true, false, false,
        false, false, false, false, true, true,
    ];
    let (bad, presses) = box_hold_violations(Build::Shipped);
    assert_eq!(bad, Vec::<String>::new());
    assert_eq!(presses, shipped);

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
    for i in [8, 9, 11, 12, 13, 14, 15, 16] {
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
            "relabelled once touched"
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

/// The measured review tab of a four-question dialog (2.1.282) with an
/// auto-deny countdown at `left` over its `Ready to submit your answers?` —
/// HAND-BUILT where 2.1.301 draws it: the review tab renders the permission
/// reason block the countdown is drawn in (between its answers and that
/// row; read in its code), and auto mode can give a question the timed
/// fallback (`classifyAskUserQuestion`).
fn review_ticking(left: &str) -> Vec<String> {
    review_with_countdown(f::QUESTION_REVIEW, left)
}

/// The review tab `fixture` with the countdown at `left` (as
/// [`review_ticking`]).
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

/// A QUESTION DIALOG WITH A COUNTDOWN (its review tab), a person 22 s past
/// their keystroke, the dialog untouched: at 1:44 its answer is held for
/// them, and at the margin — its countdown seen ticking, past the hold's end
/// — it is answered (Enter on `Submit answers`, the person's quiet no longer
/// waited for: neither the hold's `status` nor the dialog's own wait, R1, nor
/// the press's stamps).
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
        m.human = vec!["22000"];
        m.human_ms = Some(TYPED_BEFORE_MS);
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

/// A PERSON'S GESTURE COUNTED INSIDE THE ANSWER STOPS IT PAST THE HOLD: the
/// review tab ticking into the margin on `Cancel` under a person's grace,
/// untouched, so its answer goes — `up` onto `Submit answers`, then Enter.
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
    m.human = vec!["22000"];
    m.human_ms = Some(TYPED_BEFORE_MS);
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

/// [`tall_states`] with an auto-deny countdown at `left` over the question
/// of each — HAND-BUILT (the measured decline states draw it there, above
/// the tall box's foot), the screen kept at its 48 rows.
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

/// The decline of the tall rm box under the safe rules, in its bypass
/// session, its countdown a second lower each screen served from `from`:
/// `states` the decline state each serves — two a look, as the loop asks for
/// the box's last rows and then, its head cut by them, the whole screen,
/// the one read of the two it keeps — the busy screen before and after.
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

/// A DECLINE THE POLICY OWES A BOX WITH A COUNTDOWN (the tall rm box, under
/// the safe rules in its bypass session), a person 22 s past their
/// keystroke, the box untouched: each of its states served twice a look (its
/// last rows, then the whole screen, which alone the loop keeps), the
/// countdown a second lower each from 0:18 — the one countdown kept, the
/// whole read's, seen once on the first look, which holds to read it again,
/// then ticking on every look, past the hold's end — so the decline goes,
/// all four keystrokes, Enter last: the person's quiet is not waited for.
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

/// A PERSON'S KEY DURING THE LOOP'S DECLINE MAKES THE BOX THEIRS (review the
/// decline once re-arrowed over a person's move back onto `Yes`).
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

/// A PERSON'S GESTURE COUNTED INSIDE THE DECLINE STOPS IT PAST THE HOLD: the
/// tall rm box declined at the hold's end under a person's grace, nothing
/// scripted at the server.
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

/// The `await idle` waits among `requests` that a screen redrawn every
/// `tick_ms` runs out ([`Mock::tick_ms`]): each a whole wait (its `timeout`)
/// in a real run, spent while the countdown runs.
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

/// A dialog `supervise` answers in one look under a manager: `rows`, its
/// countdown ticking a second a read ([`Mock::countdown_ticks`]) and the
/// screen redrawn every `tick` ms.
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

/// A KEY THE TICKING DIALOG DID NOT TAKE IS SENT AGAIN BEFORE ITS DENY: the
/// review tab at 0:14 under a manager, the Enter not taken (the dialog drawn
/// as keyed), its countdown ticking, the screen redrawn every second (the
/// mock's `await idle` latches only under 1000 ms of quiet).
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

/// The waits for a still screen `m` made after its first Enter, the retry's
/// premise, as `(quiet ms, timeout ms)` — the press's own settle after a
/// read (`await idle 500 timeout 1500`) is no such wait.
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

/// ABOVE THE MARGIN THE RETRY'S PREMISE IS TWO SECONDS OF QUIET, AS EVER,
/// WAITED FOR NO LATER THAN THE MARGIN'S START: the same review tab ticking,
/// the Enter not taken.
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

/// The timeout `await … timeout <ms>` asks for.
fn timeout_ms(request: &str) -> u64 {
    request
        .rsplit_once(" timeout ")
        .and_then(|(_, ms)| ms.parse().ok())
        .unwrap_or_else(|| panic!("no timeout: {request}"))
}

/// A PRESS NEAR THE DENY THAT THE BOX DID NOT TAKE IS PRESSED AGAIN BEFORE
/// IT: the incident's box, nobody about, pressed on its first read (its
/// countdown seen once, nothing believed yet), drawn after its `1` as it was
/// before, its countdown ticking.
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

/// `rows` with the vendor's position label `label` (`1 of 2`) closing the
/// incident box's title row at the pane's edge (144 columns), as Claude Code
/// draws it once another ask stacks behind the box (2.1.286 on;
/// `claude-2.1.292-box-bash-background-agent-stacked-72col.txt`).
fn stacked(rows: &[String], label: &str) -> Vec<String> {
    let title = format!(" Bash command{label:>131}");
    swapped(rows, &[(" Bash command", &[&title])])
}

/// `text` cut to `cols` cells as Ink's `wrap="truncate"` cuts a row that
/// does not fit: its first `cols - 1` cells and `…`.
fn truncated(text: &str, cols: usize) -> String {
    if text.chars().count() <= cols {
        return text.to_string();
    }
    let mut out: String = text.chars().take(cols - 1).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// `text` word-wrapped to `cols` cells, its continuation rows at column 1.
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

/// A BACKGROUND SUBAGENT'S BOX with the vendor's countdown at `left`
/// (HAND-BUILT on the measured 72-column box,
/// `claude-2.1.292-box-bash-background-agent-stacked-72col.txt`: the rm
/// safety check's countdown is drawn on a subagent's box too) as Claude Code
/// redraws it in a pane `cols` wide: its rules the pane's width, its
/// countdown wrapped to it, and its footer's note cut by it (`wrap="truncate"`:
/// `… twice to stop background…` at 72 columns, whole from 78).
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

/// THE FIRST KEY AFTER A TAB'S FIRST-KEY WAIT GOES ON A READ MADE AFTER IT.
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
    m.human = vec!["22000"];
    m.human_ms = Some(TYPED_BEFORE_MS);
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

/// A COUNTDOWN IS THE COUNTDOWN WHATEVER CLAUDE CODE REDRAWS OF ITS BOX: a
/// read placing the deny in step with the last read's is the same countdown
/// ticking, its box relabelled (`1 of 2`, another ask stacked behind it),
/// re-wrapped, or edited by the loop's own decline — so no hold looks again
/// to see it tick, and its hold's end is said once.
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

/// A HOLD LOOKS AGAIN WHEN IT ENDS, NOT AT THE GRACE'S END (every other test
/// of the hold looks again at once, `Session::set_person_hold_wait`, so when
/// its look was owed went unchecked).
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

/// A LEASE TAKEN DURING A TAB'S FIRST-KEY WAIT IS A TOUCH, AND NO KEY GOES
/// UNDER IT: the review tab ticking into the margin under a person's grace,
/// untouched, so its answer goes past the hold — but another driver takes a
/// cooperative lease while the loop waits for the tab to stand still before
/// its first key (`await idle 1000`, O4), which the server lets the loop's
/// keys through.
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
        m.human = vec!["22000"];
        m.human_ms = Some(TYPED_BEFORE_MS);
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

/// A BOX THAT LEFT BETWEEN THE LEAVE-WAIT'S END AND THE READ AFTER IT LEFT:
/// the incident's box, nobody about, its `1` pressed and taken, but Claude
/// Code takes the box down just after the wait for it to leave ran out
/// (`await seq … OK timeout`), so the read after that wait shows the worker
/// busy again.
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

/// `rows` with the countdown wiped as Claude Code wipes it when a key
/// reaches the box (2.1.283, measured: within 5 ms of a taken `1`): its row
/// and one of the two blank rows under it gone, the box still drawn.
fn wiped(rows: &[String]) -> Vec<String> {
    let at = rows
        .iter()
        .position(|r| r.contains("deny this request in "))
        .expect("the countdown");
    let mut out = rows.to_vec();
    out.drain(at..=at + 1);
    out
}

/// A BOX WHOSE COUNTDOWN A KEY WIPED IS WAITED ON TO LEAVE: the incident's
/// box, nobody about, its countdown seen ticking 14 s from the deny, so the
/// wait for it to leave after its `1` is the short one near the deny
/// (`LEAVE_NEAR_DENY`).
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

/// A COUNTDOWN THAT STOOD STILL IS LOOKED AT AGAIN UNTIL IT TICKS: the
/// incident's box under a person's grace, untouched, its countdown standing
/// at 1:50 for five reads a second apart (Claude Code's redraw stalled on a
/// loaded machine) — past `TICK_SLACK`, so it is no deny the loop believes —
/// then drawn at 1:45, the time the vendor's own deadline gives once it
/// redraws.
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

/// NO WAIT FOR A STILL SCREEN RUNS PAST THE DENY IT RACES: the review tab at
/// 0:14 under a manager, its Enter not taken, on a screen that never holds
/// still half a second (Claude Code's tool row blinking over the dialog as
/// well as its countdown ticking, [`Mock::idle_never`]).
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
