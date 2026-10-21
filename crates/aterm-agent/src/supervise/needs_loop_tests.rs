// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The worker's NEEDS YOU on the person's screen (FOREVER CRUISE / FOREVER
// LAND, `escalate.rs`'s `sync_needs`): the `needs` owner's entry put up once
// a set of needs, taken down when none stands, at an Off park and at the
// loop's end, journaled `NEEDS`/`CLEARED … needs` and never as an
// escalation, and never touched behind another supervisor's claim. Included
// from `run.rs`'s tests (`mod needs_loop`), so the `Mock` and the helpers
// there are in scope.

use crate::supervise::journal::{CLEARED_NEEDS, NEEDS};

/// What a step of the loop SAID and what it only NOTED (the journal alone).
#[derive(Default)]
struct Notes {
    said: Vec<String>,
    noted: Vec<String>,
}

impl Review for Notes {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, _turn: &Turn) -> Result<bool, String> {
        Ok(true)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        self.said.push(line.to_string());
        Ok(())
    }
    fn note(&mut self, line: &str) {
        self.noted.push(line.to_string());
    }
    fn unattended(&self) -> bool {
        true
    }
}

/// What only a person can give, as the worker names it.
const NEED: &str = "the App Store signing credential";

/// A turn that ended after 12 minutes of work, the worker's last words
/// `lines` (the first under Claude Code's `⏺`, the rest beneath it), at an
/// empty composer in bypass mode.
fn said(lines: &[&str]) -> Vec<String> {
    let mut drawn = vec![format!("⏺ {}", lines[0])];
    drawn.extend(lines[1..].iter().map(|l| format!("  {l}")));
    drawn.extend(["", "✻ Worked for 12m 2s · done 4:24 PM", ""].map(str::to_string));
    let mut r = rows(&drawn.iter().map(String::as_str).collect::<Vec<_>>());
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// [`said`], the report `report` with [`NEED`] on its NEEDS YOU line.
fn needing(report: &str) -> Vec<String> {
    said(&[report, &format!("NEEDS YOU: {NEED}")])
}

/// The `meta … owner=needs` requests `m` was sent, verb and owner first.
fn needs_calls(m: &Mock) -> Vec<String> {
    m.needs_calls.clone()
}

/// The journal's lines at `path` that start with `kind`.
fn journaled(path: &Path, kind: &str) -> Vec<String> {
    journal_records(path)
        .0
        .into_iter()
        .map(|r| r.line)
        .filter(|l| l.starts_with(kind))
        .collect()
}

/// `screens` watched under Cruise, every screen of `gates` sitting until
/// the loop's next act, journaled at a scratch journal of `tag`: the
/// scripted server and that journal's path.
fn watched(tag: &str, screens: Vec<Vec<String>>, gates: Vec<usize>) -> (Mock, PathBuf, PathBuf) {
    watched_with(tag, screens, gates, |_| {})
}

/// [`watched`], the scripted server first set up by `setup`.
fn watched_with(
    tag: &str,
    screens: Vec<Vec<String>>,
    gates: Vec<usize>,
    setup: impl FnOnce(&mut Mock),
) -> (Mock, PathBuf, PathBuf) {
    let (dir, journal) = journal_file(tag);
    let last = u32::try_from(screens.len()).expect("a short script");
    let mut m = Mock::new(screens);
    setup(&mut m);
    m.autopilot = Some("cruise");
    m.turn_gates = gates;
    m.vanish_after = Some(last - 1);
    m.stall_sleep = Some(Duration::from_secs(20));
    let opts = SuperviseOpts {
        policy: owners_policy(),
        journal: Some(journal.clone()),
        ..auto(30)
    };
    let _ = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    });
    (m, dir, journal)
}

/// THE WORKER'S NEEDS GO UP ONCE AND COME DOWN WHEN NONE STANDS: a report
/// with a NEEDS YOU line puts the `needs` owner's entry up at its point —
/// `meta set attention owner=needs kind=ask <program> needs you: <need> —
/// carrying on` — journaled `NEEDS seq=<n> count=1 attention=OK <need>`; the
/// busy reads after it and the same need said again at the next point put
/// nothing more up and take nothing down; the worker's `NEEDS YOU: nothing`
/// takes it down (`meta unset attention owner=needs`, journaled `CLEARED
/// seq=<n> needs attention=OK …`). The supervisor's own entry never holds
/// the need, and no line of it is an escalation. NEGATIVE CONTROL: the same
/// run with no NEEDS YOU line sends no `owner=needs` request and journals no
/// `NEEDS`.
#[test]
fn the_workers_needs_go_up_once_and_come_down_when_none_stands() {
    let (m, dir, journal) = watched(
        "needs-once",
        vec![
            busy_screen(),
            needing("Pushed the parser fix; the release is next."),
            busy_screen(),
            needing("Built the release notes; still carrying on."),
            busy_screen(),
            said(&["Shipped the docs.", "NEEDS YOU: nothing"]),
        ],
        vec![1, 3],
    );
    let calls = needs_calls(&m);
    assert_eq!(calls.len(), 2, "one set, one unset: {calls:#?}");
    assert_eq!(
        calls[0],
        format!("set attention owner=needs kind=ask claude needs you: {NEED} — carrying on"),
        "{calls:#?}"
    );
    assert_eq!(calls[1], "unset attention owner=needs", "{calls:#?}");
    assert_eq!(m.needs_attention, None, "down at the end");
    let up = journaled(&journal, &format!("{NEEDS} "));
    assert_eq!(up.len(), 1, "{up:#?}");
    assert!(
        up[0].contains(" count=1 attention=OK ") && up[0].ends_with(NEED),
        "{up:#?}"
    );
    let down: Vec<String> = journaled(&journal, "CLEARED ")
        .into_iter()
        .filter(|l| l.contains(&format!(" {CLEARED_NEEDS} attention=OK ")))
        .collect();
    assert_eq!(down.len(), 1, "{down:#?}");
    assert!(
        down[0].ends_with("none stands now"),
        "the worker's `NEEDS YOU: nothing`, not the loop's end: {down:#?}"
    );
    let escalated = journaled(&journal, "ESCALATED ");
    assert!(
        !escalated.iter().any(|l| l.contains(NEED)),
        "a need is no escalation: {escalated:#?}"
    );
    assert!(
        !m.attention.as_deref().is_some_and(|a| a.contains(NEED)),
        "the supervisor's own entry: {:?}",
        m.attention
    );
    let _ = std::fs::remove_dir_all(&dir);
    // NEGATIVE CONTROL: no NEEDS YOU line.
    let (m, dir, journal) = watched(
        "needs-none",
        vec![
            busy_screen(),
            said(&["Pushed the parser fix; the release is next."]),
            busy_screen(),
            said(&["Built the release notes; still carrying on."]),
        ],
        vec![1],
    );
    assert!(needs_calls(&m).is_empty(), "{:#?}", needs_calls(&m));
    assert!(journaled(&journal, &format!("{NEEDS} ")).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A NEW SET OF NEEDS IS PUT UP AGAIN: a second need beside the first is a
/// new set, and the entry is set once more, naming the first need and how
/// many more stand (`(+1 more)`), journaled `count=2`. NEGATIVE CONTROL: the
/// first set said again sets nothing (the test above).
#[test]
fn a_new_set_of_needs_is_put_up_again() {
    let (m, dir, journal) = watched(
        "needs-new-set",
        vec![
            busy_screen(),
            needing("Pushed the parser fix; the release is next."),
            busy_screen(),
            said(&[
                "Built the release notes.",
                &format!("NEEDS YOU: {NEED}"),
                "NEEDS YOU: a decision on the public launch date",
            ]),
        ],
        vec![1],
    );
    let sets: Vec<String> = needs_calls(&m)
        .into_iter()
        .filter(|c| c.starts_with("set "))
        .collect();
    assert_eq!(sets.len(), 2, "{sets:#?}");
    assert!(sets[1].contains(&format!("{NEED} (+1 more)")), "{sets:#?}");
    let up = journaled(&journal, &format!("{NEEDS} "));
    assert_eq!(up.len(), 2, "{up:#?}");
    assert!(up[1].contains(" count=2 "), "{up:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN OFF PARK AND THE LOOP'S END TAKE THE NEEDS ENTRY DOWN
/// (`Session::lower_badges`, `Session::release_attention`): `meta unset
/// attention owner=needs`, journaled `CLEARED seq=<n> needs attention=OK
/// <why>` — the session's autopilot off, the loop ended — and never as an
/// escalation's badge. Behind another supervisor's claim nothing is unset
/// (that owner's entry may be the claimant's own): the loop only forgets
/// its set. NEGATIVE CONTROL: a loop that put no needs up sends and says
/// nothing.
#[test]
fn an_off_park_and_the_loops_end_take_the_needs_entry_down() {
    let entry = format!("claude needs you: {NEED} — carrying on");
    let lowered = |told: bool, behind: bool, end: bool| {
        let mut m = Mock::new(vec![idle_screen()]);
        m.needs_attention = Some(entry.clone());
        let mut notes = Notes::default();
        let forgot;
        {
            let mut s = session(&mut m, None);
            s.needs_told = told.then_some(7);
            if behind {
                s.claim = claim::Claim::Behind {
                    holder: "another".to_string(),
                    tried: Instant::now(),
                };
            }
            if end {
                s.release_attention(&mut notes);
            } else {
                assert_eq!(s.lower_badges(101, &mut notes), Ok(()));
            }
            forgot = s.needs_told.is_none();
        }
        (notes, m, forgot)
    };
    let (notes, m, forgot) = lowered(true, false, false);
    assert_eq!(
        notes.noted,
        [format!(
            "CLEARED seq=101 {CLEARED_NEEDS} attention=OK the session's autopilot is off"
        )]
    );
    assert_eq!(m.needs_attention, None);
    assert_eq!(m.needs_calls, ["unset attention owner=needs"]);
    assert!(forgot);
    let (notes, m, forgot) = lowered(true, false, true);
    assert!(
        notes.noted.contains(&format!(
            "CLEARED seq=0 {CLEARED_NEEDS} attention=OK the loop ended"
        )),
        "{:#?}",
        notes.noted
    );
    assert_eq!(m.needs_attention, None);
    assert!(forgot);
    // Behind another supervisor's claim: forgotten, never unset.
    let (notes, m, forgot) = lowered(true, true, false);
    assert!(m.needs_calls.is_empty(), "{:#?}", m.needs_calls);
    assert_eq!(m.needs_attention.as_deref(), Some(entry.as_str()));
    assert!(
        !notes.noted.iter().any(|l| l.contains(CLEARED_NEEDS)),
        "{:#?}",
        notes.noted
    );
    assert!(forgot);
    // NEGATIVE CONTROL: nothing put up.
    let (notes, m, _) = lowered(false, false, false);
    assert!(m.needs_calls.is_empty(), "{:#?}", m.needs_calls);
    assert!(
        !notes.noted.iter().any(|l| l.contains(CLEARED_NEEDS)),
        "{:#?}",
        notes.noted
    );
}

/// THE WORKER'S NEEDS ARE NEVER PUT UP BEHIND ANOTHER SUPERVISOR'S CLAIM: a
/// loop watching behind a holder (a proven self-claim's codex-alab raises
/// its own) folds the point and its NEEDS YOU line in, and sends no
/// `owner=needs` request and journals no `NEEDS`. NEGATIVE CONTROL: the same
/// point with the claim its own puts the entry up, once — the point decided
/// again adds nothing.
#[test]
fn the_workers_needs_are_never_put_up_behind_another_supervisors_claim() {
    let run = |behind: bool| {
        let mut m = Mock::new(vec![needing("Pushed the parser fix; the release is next.")]);
        m.autopilot = Some("cruise");
        let opts = SuperviseOpts {
            policy: owners_policy(),
            ..auto(30)
        };
        let mut notes = Notes::default();
        {
            let mut s = session(&mut m, None);
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            if behind {
                s.claim = claim::Claim::Behind {
                    holder: "another".to_string(),
                    tried: Instant::now(),
                };
            }
            let screen = s.screen().expect("the point");
            let point = s.turn_of(screen);
            let _ = s
                .turn_end_at_point(&point, Some(Duration::from_secs(720)), &opts)
                .expect("decided");
            for _ in 0..2 {
                s.task_edge(&point.screen, &opts, &mut notes)
                    .expect("the point's edge");
            }
        }
        (m.needs_calls.clone(), notes.noted)
    };
    let (calls, noted) = run(true);
    assert!(calls.is_empty(), "{calls:#?}");
    assert!(
        !noted.iter().any(|l| l.starts_with(&format!("{NEEDS} "))),
        "{noted:#?}"
    );
    // NEGATIVE CONTROL: the claim its own.
    let (calls, noted) = run(false);
    assert_eq!(calls.len(), 1, "once: {calls:#?}");
    assert!(calls[0].starts_with("set attention owner=needs kind=ask "));
    assert_eq!(
        noted
            .iter()
            .filter(|l| l.starts_with(&format!("{NEEDS} ")))
            .count(),
        1,
        "{noted:#?}"
    );
}

/// A PERSON'S TURN TAKES THE NEEDS DOWN: the worker's NEEDS YOU is up, and
/// a person, at the keyboard through the point (their grace holds the
/// loop's act), types a turn of their own — someone else's turn, after
/// which the need no longer stands (the policy forgets it there) — so the
/// entry comes down at that turn's end, journaled `CLEARED … needs … none
/// stands now`. NEGATIVE CONTROL: the same run with nobody at the keyboard,
/// the loop's own act answered by a turn with no NEEDS YOU line, leaves the
/// entry up until the loop's end (a need stands until a person's turn, the
/// worker's `NEEDS YOU: nothing` or a defense with none).
#[test]
fn a_persons_turn_takes_the_needs_down() {
    let run = |person: bool| {
        let (dir, journal) = journal_file(&format!("needs-person-{person}"));
        let mut m = Mock::new(vec![
            busy_screen(),
            needing("Pushed the parser fix; the release is next."),
            busy_screen(),
            said(&["The schema keeps its v2 field, as you asked."]),
        ]);
        m.autopilot = Some("cruise");
        if person {
            m.human = vec!["0"];
        } else {
            m.turn_gates = vec![1];
        }
        m.vanish_after = Some(3);
        m.stall_sleep = Some(Duration::from_secs(20));
        let opts = SuperviseOpts {
            policy: owners_policy(),
            journal: Some(journal.clone()),
            ..auto(30)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
        });
        let cleared: Vec<String> = journaled(&journal, "CLEARED ")
            .into_iter()
            .filter(|l| l.contains(&format!(" {CLEARED_NEEDS} ")))
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        (needs_calls(&m), cleared)
    };
    let (calls, cleared) = run(true);
    assert_eq!(
        calls.iter().filter(|c| c.starts_with("set ")).count(),
        1,
        "{calls:#?}"
    );
    assert_eq!(cleared.len(), 1, "{cleared:#?}");
    assert!(cleared[0].ends_with("none stands now"), "{cleared:#?}");
    // NEGATIVE CONTROL: nobody at the keyboard.
    let (calls, cleared) = run(false);
    assert_eq!(
        calls.iter().filter(|c| c.starts_with("set ")).count(),
        1,
        "{calls:#?}"
    );
    assert_eq!(cleared.len(), 1, "{cleared:#?}");
    assert!(cleared[0].ends_with("the loop ended"), "{cleared:#?}");
}

/// The entry a loop before this one put up for [`NEED`].
fn left_entry() -> String {
    format!("claude needs you: {NEED} — carrying on")
}

/// A NEEDS ENTRY A LOOP BEFORE THIS ONE LEFT COMES DOWN AT THE FIRST POINT
/// WHERE NO NEED STANDS (`lifecycle.rs`'s `reconcile`, `escalate.rs`'s
/// `NEEDS_LEFT`): a loop that crashed left `owner=needs` up, which no loop
/// would ever take down, so the menu bar said "needs you" for ever. A new
/// loop finds it at its start (`attention_kinds=`) and its first point,
/// naming no need, takes it down — `meta unset attention owner=needs`,
/// journaled `CLEARED seq=<n> needs attention=OK no need stands at the
/// loop's start`; a first point that names a need puts its own up in the
/// entry's place, with nothing taken down first. Behind another
/// supervisor's claim nothing is touched (beside a proven self-claim the
/// agent raises its own). NEGATIVE CONTROL: with no entry left, the same
/// first point sends no `owner=needs` request and journals nothing of it.
#[test]
fn a_needs_entry_left_by_a_loop_before_comes_down_at_the_first_point() {
    let run = |left: bool, behind: bool, screen: Vec<String>| {
        let mut m = Mock::new(vec![screen]);
        m.autopilot = Some("cruise");
        if left {
            m.needs_attention = Some(left_entry());
        }
        let opts = SuperviseOpts {
            policy: owners_policy(),
            ..auto(30)
        };
        let mut notes = Notes::default();
        {
            let mut s = session(&mut m, None);
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            if behind {
                s.claim = claim::Claim::Behind {
                    holder: "another".to_string(),
                    tried: Instant::now(),
                };
            }
            s.reconcile(&mut notes).expect("the loop's start");
            let screen = s.screen().expect("the point");
            let point = s.turn_of(screen);
            let _ = s
                .turn_end_at_point(&point, Some(Duration::from_secs(720)), &opts)
                .expect("decided");
            for _ in 0..2 {
                s.task_edge(&point.screen, &opts, &mut notes)
                    .expect("the point's edge");
            }
        }
        let cleared: Vec<String> = notes
            .noted
            .iter()
            .filter(|l| l.starts_with("CLEARED ") && l.contains(&format!(" {CLEARED_NEEDS} ")))
            .cloned()
            .collect();
        (m.needs_calls.clone(), m.needs_attention.clone(), cleared)
    };
    let plain = || said(&["Pushed the parser fix; the release is next."]);
    let (calls, standing, cleared) = run(true, false, plain());
    assert_eq!(calls, ["unset attention owner=needs"]);
    assert_eq!(standing, None);
    assert_eq!(cleared.len(), 1, "{cleared:#?}");
    assert!(
        cleared[0].contains(" attention=OK ")
            && cleared[0].ends_with("no need stands at the loop's start"),
        "{cleared:#?}"
    );
    // A need standing at the first point: its own entry in the old one's place.
    let (calls, standing, cleared) = run(true, false, needing("Pushed the parser fix."));
    assert_eq!(
        calls,
        [format!(
            "set attention owner=needs kind=ask claude needs you: {NEED} — carrying on"
        )]
    );
    assert_eq!(standing, Some(left_entry()));
    assert!(cleared.is_empty(), "{cleared:#?}");
    // Behind another supervisor's claim: never touched.
    let (calls, standing, cleared) = run(true, true, plain());
    assert!(calls.is_empty(), "{calls:#?}");
    assert_eq!(standing, Some(left_entry()));
    assert!(cleared.is_empty(), "{cleared:#?}");
    // NEGATIVE CONTROL: nothing left.
    let (calls, standing, cleared) = run(false, false, plain());
    assert!(calls.is_empty(), "{calls:#?}");
    assert_eq!(standing, None);
    assert!(cleared.is_empty(), "{cleared:#?}");
}

/// THE WHOLE LOOP TAKES A LEFT NEEDS ENTRY DOWN AT ITS FIRST POINT: the
/// watch's start reads the entry a loop before it left, and the first
/// point, naming no need, takes it down, journaled with the start's reason
/// — once: the loop's end has nothing left to take down. NEGATIVE CONTROL:
/// the same watch with no entry left sends no `owner=needs` request.
#[test]
fn the_loop_takes_a_left_needs_entry_down_at_its_first_point() {
    let screens = || {
        vec![
            busy_screen(),
            said(&["Pushed the parser fix; the release is next."]),
        ]
    };
    let (m, dir, journal) = watched_with("needs-left", screens(), vec![], |m| {
        m.needs_attention = Some(left_entry());
    });
    assert_eq!(needs_calls(&m), ["unset attention owner=needs"]);
    assert_eq!(m.needs_attention, None);
    let down: Vec<String> = journaled(&journal, "CLEARED ")
        .into_iter()
        .filter(|l| l.contains(&format!(" {CLEARED_NEEDS} ")))
        .collect();
    assert_eq!(down.len(), 1, "{down:#?}");
    assert!(
        down[0].ends_with("no need stands at the loop's start"),
        "{down:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    // NEGATIVE CONTROL: nothing left.
    let (m, dir, journal) = watched("needs-left-none", screens(), vec![]);
    assert!(needs_calls(&m).is_empty(), "{:#?}", needs_calls(&m));
    assert!(
        !journaled(&journal, "CLEARED ")
            .iter()
            .any(|l| l.contains(&format!(" {CLEARED_NEEDS} "))),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// codex-alab's own NEEDS YOU entry, as it raises it beside its proven
/// self-claim (the contract's §5 NEEDS YOU row).
fn self_claims_entry() -> String {
    format!("Codex needs you: {NEED} — carrying on")
}

/// The proven self-claim's holder the beside tests watch beside.
const SELF_CLAIM: &str = "codex-alab@4242";

/// BESIDE A PROVEN SELF-CLAIM THE NEEDS ENTRY IS NEVER THIS LOOP'S (the
/// codex-alab contract's §5 NEEDS YOU row; `beside.rs`'s
/// `needs_not_ours`): codex-alab raises and takes down `owner=needs`
/// itself, so the hosted loop watching beside it — through a report naming
/// a need, the worker's `NEEDS YOU: nothing`, and an Off park — sends no
/// `owner=needs` request (none even refused locally: nothing of it is
/// journaled), and codex-alab's standing entry is left as it stands, never
/// adopted as one a loop before this one left. NEGATIVE CONTROL: the same
/// watches as aterm's own loop (its own claim) do write it — the cruise
/// watch puts its need up in the standing entry's place, and the Off park
/// takes the standing entry down as one a loop before it left.
#[test]
fn beside_a_proven_self_claim_the_loop_never_writes_or_unsets_the_needs_entry() {
    let run = |beside: bool, word: &'static str| {
        let (dir, journal) = journal_file(&format!("needs-beside-{beside}-{word}"));
        let screens = vec![
            busy_screen(),
            needing("Pushed the parser fix; the release is next."),
            busy_screen(),
            said(&["Released it.", "NEEDS YOU: nothing"]),
        ];
        let last = u32::try_from(screens.len()).expect("a short script");
        let mut m = Mock::new(screens);
        m.needs_attention = Some(self_claims_entry());
        m.autopilot = Some(word);
        m.turn_gates = vec![];
        m.vanish_after = Some(last - 1);
        m.stall_sleep = Some(Duration::from_secs(20));
        let opts = SuperviseOpts {
            policy: owners_policy(),
            journal: Some(journal.clone()),
            ..auto(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
            if beside {
                s.set_beside(Some(SELF_CLAIM.to_string()));
            }
        });
        let needs_rows: Vec<String> = journaled(&journal, "")
            .into_iter()
            .filter(|l| {
                l.starts_with(&format!("{NEEDS} "))
                    || (l.starts_with("CLEARED ") && l.contains(&format!(" {CLEARED_NEEDS} ")))
            })
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        (
            m.needs_calls.clone(),
            m.needs_attention.clone(),
            needs_rows,
            lines,
        )
    };
    for word in ["cruise", "off"] {
        let (calls, standing, rows, lines) = run(true, word);
        assert!(calls.is_empty(), "{word}: {calls:#?}");
        assert_eq!(standing, Some(self_claims_entry()), "{word}");
        assert!(rows.is_empty(), "{word}: {rows:#?}");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("WATCHING a proven self-claim supervises this session")),
            "{word}: {lines:#?}"
        );
    }
    // NEGATIVE CONTROL: aterm's own loop writes it.
    let (calls, _, rows, _) = run(false, "cruise");
    assert!(
        calls.first().is_some_and(|c| c
            == &format!(
                "set attention owner=needs kind=ask claude needs you: {NEED} — carrying on"
            )),
        "{calls:#?}"
    );
    assert!(
        rows.iter().any(|l| l.starts_with(&format!("{NEEDS} "))),
        "{rows:#?}"
    );
    let (calls, standing, _, _) = run(false, "off");
    assert_eq!(calls, ["unset attention owner=needs"]);
    assert_eq!(standing, None);
}

/// BESIDE A PROVEN SELF-CLAIM NOTHING IS UNSET, WHATEVER THE CLAIM'S STATE
/// AT THAT MOMENT (`beside.rs`'s `needs_not_ours`, not the claim alone): a
/// set this loop holds told — however it came to — is only forgotten at an
/// Off park whose claim was given back (`Claim::GivenBack`, which watches
/// behind no one) and at the loop's end after its claim went
/// (`Claim::Off`), never taken down: the entry is codex-alab's. NEGATIVE
/// CONTROL: the same moments in aterm's own loop take it down.
#[test]
fn beside_a_self_claim_a_given_back_or_released_claim_unsets_no_needs_entry() {
    let lowered = |beside: bool, claim: claim::Claim, end: bool| {
        let mut m = Mock::new(vec![idle_screen()]);
        m.needs_attention = Some(self_claims_entry());
        let mut notes = Notes::default();
        let forgot;
        {
            let mut s = session(&mut m, None);
            if beside {
                s.set_beside(Some(SELF_CLAIM.to_string()));
            }
            s.needs_told = Some(escalate::NEEDS_LEFT);
            s.claim = claim;
            if end {
                s.release_attention(&mut notes);
            } else {
                assert_eq!(
                    s.needs_down(101, "the session's autopilot is off", &mut notes),
                    Ok(())
                );
            }
            forgot = s.needs_told.is_none();
        }
        (
            m.needs_calls.clone(),
            m.needs_attention.clone(),
            notes.noted,
            forgot,
        )
    };
    let given_back = || claim::Claim::GivenBack {
        behind: Some(SELF_CLAIM.to_string()),
    };
    for (claim, end) in [(given_back(), false), (claim::Claim::Off, true)] {
        let (calls, standing, noted, forgot) = lowered(true, claim, end);
        assert!(calls.is_empty(), "{calls:#?}");
        assert_eq!(standing, Some(self_claims_entry()));
        assert!(
            !noted.iter().any(|l| l.contains(CLEARED_NEEDS)),
            "{noted:#?}"
        );
        assert!(forgot, "the set is forgotten");
    }
    // NEGATIVE CONTROL: aterm's own loop.
    for (claim, end) in [
        (claim::Claim::GivenBack { behind: None }, false),
        (claim::Claim::Off, true),
    ] {
        let (calls, standing, _, forgot) = lowered(false, claim, end);
        assert_eq!(calls, ["unset attention owner=needs"]);
        assert_eq!(standing, None);
        assert!(forgot);
    }
}
