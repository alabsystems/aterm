// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Tests for `NEEDS YOU` screen escalation and clearing in the supervise loop.

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
/// seq=<n> needs attention=OK …`).
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
/// many more stand (`(+1 more)`), journaled `count=2`.
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
/// escalation's badge.
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
    let (notes, m, forgot) = lowered(true, true, false);
    assert!(m.needs_calls.is_empty(), "{:#?}", m.needs_calls);
    assert_eq!(m.needs_attention.as_deref(), Some(entry.as_str()));
    assert!(
        !notes.noted.iter().any(|l| l.contains(CLEARED_NEEDS)),
        "{:#?}",
        notes.noted
    );
    assert!(forgot);
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
/// `owner=needs` request and journals no `NEEDS`.
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
/// stands now`.
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
/// would ever take down, so the menu bar said "needs you" for ever.
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
    let (calls, standing, cleared) = run(true, false, needing("Pushed the parser fix."));
    assert_eq!(
        calls,
        [format!(
            "set attention owner=needs kind=ask claude needs you: {NEED} — carrying on"
        )]
    );
    assert_eq!(standing, Some(left_entry()));
    assert!(cleared.is_empty(), "{cleared:#?}");
    let (calls, standing, cleared) = run(true, true, plain());
    assert!(calls.is_empty(), "{calls:#?}");
    assert_eq!(standing, Some(left_entry()));
    assert!(cleared.is_empty(), "{cleared:#?}");
    let (calls, standing, cleared) = run(false, false, plain());
    assert!(calls.is_empty(), "{calls:#?}");
    assert_eq!(standing, None);
    assert!(cleared.is_empty(), "{cleared:#?}");
}

/// THE WHOLE LOOP TAKES A LEFT NEEDS ENTRY DOWN AT ITS FIRST POINT: the
/// watch's start reads the entry a loop before it left, and the first
/// point, naming no need, takes it down, journaled with the start's reason
/// — once: the loop's end has nothing left to take down.
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
/// adopted as one a loop before this one left.
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
/// (`Claim::Off`), never taken down: the entry is codex-alab's.
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

/// A need of 150 characters: whole on the window's host, cut to the entry's
/// 64 cells on the wire.
const LONG_NEED: &str = "grant Screen Recording to com.aterm.aterm in System Settings ▸ Privacy \
                         & Security ▸ Screen Recording, so the windowed checks can capture the \
                         menu bars";

/// The door the window's host is told the needs set at, on a tape.
const NEEDS_DOOR: &str = "host.needs_standing";

/// A window's host that keeps every NEEDS YOU set it is told
/// ([`IdleHost::needs_standing`]), with the entry's text it was told with.
#[derive(Debug, Default)]
struct Hears {
    told: Mutex<Vec<(Vec<String>, String)>>,
}

impl Hears {
    fn told(&self) -> Vec<(Vec<String>, String)> {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl IdleHost for Hears {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn needs_standing(&self, needs: &[String], entry: &str) {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((needs.to_vec(), entry.to_string()));
    }
}

/// `screens` watched under Cruise with `host` as the window's, every screen
/// of `gates` sitting until the loop's next act, RECORDED, beside a proven
/// self-claim when `beside` (codex-alab's own entry standing): the scripted
/// server and the recorded run, whose tape replays (strict and in acts mode)
/// as recorded.
fn heard(
    screens: Vec<Vec<String>>,
    gates: Vec<usize>,
    host: &Arc<Hears>,
    beside: bool,
) -> (Mock, RecordedWatch) {
    heard_with(screens, gates, host, beside, Vec::new())
}

/// [`heard`], the scripted server answering the `owner=needs` requests with
/// `replies` first (none left: `OK`).
fn heard_with(
    screens: Vec<Vec<String>>,
    gates: Vec<usize>,
    host: &Arc<Hears>,
    beside: bool,
    replies: Vec<CtlReply>,
) -> (Mock, RecordedWatch) {
    let last = u32::try_from(screens.len()).expect("a short script");
    let mut m = Mock::new(screens);
    m.autopilot = Some("cruise");
    m.turn_gates = gates;
    m.needs_replies = replies.into();
    m.vanish_after = Some(last - 1);
    m.stall_sleep = Some(Duration::from_secs(20));
    if beside {
        m.needs_attention = Some(self_claims_entry());
    }
    let opts = SuperviseOpts {
        policy: owners_policy(),
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..auto(30)
    };
    let run = record_watch(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
        if beside {
            s.set_beside(Some(SELF_CLAIM.to_string()));
        }
    });
    crate::supervise::tape::testing::assert_replays(&run.tape, &run.header, &run.sent, &run.lines);
    (m, run)
}

/// The recorded run's tells of the needs set and its `owner=needs`
/// requests, in the tape's order: `host <entry>` for a tell, `ctl set` or
/// `ctl unset` for a request.
fn needs_doors(run: &RecordedWatch) -> Vec<String> {
    let read = crate::supervise::tape::recover::recover(&run.tape.sink.bytes());
    read.items
        .iter()
        .filter_map(|i| match i.record()? {
            astream_effects::EffectRecord::Emit { door, bytes } if door == NEEDS_DOOR => {
                let text = String::from_utf8_lossy(bytes).into_owned();
                let entry = text.split_once("\nentry=").map_or("?", |(_, e)| e);
                Some(format!("host {entry}"))
            }
            astream_effects::EffectRecord::Exchange { door, request, .. }
                if door == crate::supervise::tape::doors::CTL =>
            {
                let words = crate::supervise::tape::codec::ctl_args(request).ok()?;
                let at = words.iter().position(|w| w == "owner=needs")?;
                Some(format!("ctl {}", words[at - 2]))
            }
            _ => None,
        })
        .collect()
}

/// The report of a run's NEEDS YOU set: [`LONG_NEED`] said at two points in
/// turn, then `NEEDS YOU: nothing`.
fn long_need_then_none() -> Vec<Vec<String>> {
    vec![
        busy_screen(),
        said(&[
            "Pushed the parser fix; the release is next.",
            &format!("NEEDS YOU: {LONG_NEED}"),
        ]),
        busy_screen(),
        said(&[
            "Built the release notes; still carrying on.",
            &format!("NEEDS YOU: {LONG_NEED}"),
        ]),
        busy_screen(),
        said(&["Shipped the docs.", "NEEDS YOU: nothing"]),
    ]
}

/// THE HOST HEARS THE WHOLE SET BEFORE THE ENTRY GOES UP
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.2.3, §12.2.6): over a
/// recorded run, the window's host is told the set — a 150-character need
/// whole, where the entry carries it cut at 64 cells — and the entry's exact
/// text BEFORE the `meta set attention owner=needs` that puts it up, and
/// `(&[], "")` before the `unset` that takes it down; told once per change:
/// nothing at the busy reads, nothing at the next point that says the same
/// need.
#[test]
fn the_host_hears_the_whole_set_before_the_entry_goes_up() {
    assert_eq!(LONG_NEED.chars().count(), 150);
    let host = Arc::new(Hears::default());
    let (m, run) = heard(long_need_then_none(), vec![1, 3], &host, false);
    let sets: Vec<&String> = m
        .needs_calls
        .iter()
        .filter(|c| c.starts_with("set "))
        .collect();
    assert_eq!(sets.len(), 1, "{:#?}", m.needs_calls);
    let entry = sets[0]
        .strip_prefix("set attention owner=needs kind=ask ")
        .expect("the needs owner's entry")
        .to_string();
    assert!(
        entry.contains('…') && !entry.contains(LONG_NEED),
        "the entry carries the need cut: {entry}"
    );
    assert_eq!(
        host.told(),
        [
            (vec![LONG_NEED.to_string()], entry.clone()),
            (Vec::new(), String::new())
        ],
        "the set whole with the entry's exact text, then the take-down, once each"
    );
    assert_eq!(
        needs_doors(&run),
        [
            format!("host {entry:?}"),
            "ctl set".to_string(),
            "host \"\"".to_string(),
            "ctl unset".to_string(),
        ],
        "the host hears first, each time"
    );
    let host = Arc::new(Hears::default());
    let (m, _) = heard(long_need_then_none(), vec![1, 3], &host, true);
    assert!(host.told().is_empty(), "{:#?}", host.told());
    assert!(m.needs_calls.is_empty(), "{:#?}", m.needs_calls);
    let host = Arc::new(Hears::default());
    let (m, _) = heard(
        vec![
            busy_screen(),
            said(&["Pushed the parser fix; the release is next."]),
            busy_screen(),
            said(&["Built the release notes; still carrying on."]),
        ],
        vec![1],
        &host,
        false,
    );
    assert!(host.told().is_empty(), "{:#?}", host.told());
    assert!(m.needs_calls.is_empty(), "{:#?}", m.needs_calls);
}

/// The window's host, the scripted server's `owner=needs` requests and the
/// entry it holds after [`NEED`]'s point is decided and carried out
/// `edges` times, the server answering those requests with `replies` first.
fn told_at_one_point(replies: Vec<CtlReply>, edges: usize) -> (Arc<Hears>, Mock) {
    let mut m = Mock::new(vec![needing("Pushed the parser fix; the release is next.")]);
    m.autopilot = Some("cruise");
    m.needs_replies = replies.into();
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let host = Arc::new(Hears::default());
    let mut notes = Notes::default();
    {
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        let _ = s
            .turn_end_at_point(&point, Some(Duration::from_secs(720)), &opts)
            .expect("decided");
        for _ in 0..edges {
            s.task_edge(&point.screen, &opts, &mut notes)
                .expect("the point's edge");
        }
    }
    (host, m)
}

/// A CAP'S REFUSAL TELLS THE HOST THE CUT TEXT (§12.2.3): the server refuses
/// the entry as too long for a tighter cap than the loop's own, the loop
/// cuts it to that cap and sets it again — and tells the host again first,
/// with the cut text, so what the host holds is the text the server stores.
#[test]
fn a_cap_refusal_tells_the_host_the_cut_text() {
    let (host, m) = told_at_one_point(vec![err("attention too long (max 40 bytes)")], 1);
    let sets: Vec<&String> = m
        .needs_calls
        .iter()
        .filter(|c| c.starts_with("set "))
        .collect();
    assert_eq!(sets.len(), 2, "the refused set and the cut one: {sets:#?}");
    let stored = m.needs_attention.clone().expect("the cut entry stands");
    assert!(stored.len() <= 40, "{stored}");
    let full = format!("claude needs you: {NEED} — carrying on");
    assert_eq!(
        host.told(),
        [
            (vec![NEED.to_string()], full),
            (vec![NEED.to_string()], stored.clone())
        ]
    );
    assert_eq!(
        host.told().last().map(|(_, e)| e.as_str()),
        Some(stored.as_str()),
        "the host holds what the server stores"
    );
}

/// A REQUEST THE CONNECTION LOST TELLS THE HOST NOTHING TWICE: the set's
/// first `meta set` is lost (the server replaced under it), so the next
/// point sends it again — the same set, the same text — and the host, told
/// before the first, is not told again: once per change, never where nothing
/// moved.
#[test]
fn a_lost_request_tells_the_host_nothing_twice() {
    let (host, m) = told_at_one_point(
        vec![replaced(
            "aterm (pid 10) went away before it answered (exit 75)",
        )],
        2,
    );
    let sets: Vec<&String> = m
        .needs_calls
        .iter()
        .filter(|c| c.starts_with("set "))
        .collect();
    assert_eq!(
        sets.len(),
        2,
        "the lost set and the one sent again: {sets:#?}"
    );
    let full = format!("claude needs you: {NEED} — carrying on");
    assert_eq!(m.needs_attention.as_deref(), Some(full.as_str()));
    assert_eq!(host.told(), [(vec![NEED.to_string()], full)]);
}

/// A SET THAT MOVES UNDER THE SAME ENTRY IS TOLD (§12.2.3 item 2): `[A, B]`
/// and then `[A, C]` put up the same entry (`claude needs you: A (+1 more)
/// — carrying on`), so only the window's host can learn that the second
/// need moved: it is told each set whole, the same entry's text with each,
/// and then the take-down, while the loop sends the entry again for the
/// moved set.
#[test]
fn a_set_that_moves_under_the_same_entry_is_told() {
    let (a, b, c) = (
        NEED,
        "the release credential to sign 0.48.0",
        "a reply on the pricing page wording",
    );
    let host = Arc::new(Hears::default());
    let (m, _) = heard(
        vec![
            busy_screen(),
            said(&[
                "Pushed the parser fix; the release is next.",
                &format!("NEEDS YOU: {a}"),
                &format!("NEEDS YOU: {b}"),
            ]),
            busy_screen(),
            said(&[
                "Built the release notes; still carrying on.",
                &format!("NEEDS YOU: {a}"),
                &format!("NEEDS YOU: {c}"),
            ]),
            busy_screen(),
            said(&["Shipped the docs.", "NEEDS YOU: nothing"]),
        ],
        vec![1, 3],
        &host,
        false,
    );
    let entry = format!("claude needs you: {a} (+1 more) — carrying on");
    let set = |n: &[&str]| n.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
    assert_eq!(
        host.told(),
        [
            (set(&[a, b]), entry.clone()),
            (set(&[a, c]), entry.clone()),
            (Vec::new(), String::new()),
        ],
        "{:#?}",
        m.needs_calls
    );
    let put_up = format!("set attention owner=needs kind=ask {entry}");
    assert_eq!(
        m.needs_calls,
        [put_up.as_str(), &put_up, "unset attention owner=needs"],
        "the entry sent again, the same text, for the moved set"
    );
}

/// A LOST REQUEST LEAVES THE WINDOW TOLD WHAT STANDS: each tell comes
/// before its request, so a request the connection lost leaves the
/// window's host told what never landed — (a) the take-down of an entry
/// whose `unset` was lost twice and that still stands, (b) another set
/// whose `meta set` was lost twice while the first stands.
#[test]
fn a_lost_request_leaves_the_window_told_what_stands() {
    let lost = || replaced("aterm (pid 10) went away before it answered (exit 75)");
    let other = "the release credential to sign 0.48.0";
    let three = |first: &str, second: &str| {
        vec![
            busy_screen(),
            said(&[
                "Pushed the parser fix; the release is next.",
                &format!("NEEDS YOU: {first}"),
            ]),
            busy_screen(),
            said(&[
                "Built the release notes; still carrying on.",
                &format!("NEEDS YOU: {second}"),
            ]),
            busy_screen(),
            said(&[
                "Shipped the docs; carrying on.",
                &format!("NEEDS YOU: {first}"),
            ]),
            busy_screen(),
        ]
    };
    let entry = format!("claude needs you: {NEED} — carrying on");
    let put_up = format!("set attention owner=needs kind=ask {entry}");
    let down = "unset attention owner=needs";
    let gone = (Vec::new(), String::new());
    let host = Arc::new(Hears::default());
    let (m, run) = heard_with(
        three(NEED, "nothing"),
        vec![1, 3, 5],
        &host,
        false,
        vec![ok("OK\n"), lost(), lost()],
    );
    assert_eq!(
        host.told(),
        [
            (vec![NEED.to_string()], entry.clone()),
            gone.clone(),
            (vec![NEED.to_string()], entry.clone()),
            gone.clone(),
        ],
        "{:#?}",
        m.needs_calls
    );
    assert_eq!(m.needs_calls, [put_up.as_str(), down, down, down]);
    assert_eq!(
        needs_doors(&run),
        [
            format!("host {entry:?}"),
            "ctl set".to_string(),
            "host \"\"".to_string(),
            "ctl unset".to_string(),
            "ctl unset".to_string(),
            format!("host {entry:?}"),
            "host \"\"".to_string(),
            "ctl unset".to_string(),
        ],
        "told again with no request, before the loop's end takes it down"
    );
    let host = Arc::new(Hears::default());
    let (m, _) = heard_with(
        three(NEED, other),
        vec![1, 3, 5],
        &host,
        false,
        vec![ok("OK\n"), lost(), lost()],
    );
    let other_entry = format!("claude needs you: {other} — carrying on");
    assert_eq!(
        host.told(),
        [
            (vec![NEED.to_string()], entry.clone()),
            (vec![other.to_string()], other_entry.clone()),
            (vec![NEED.to_string()], entry.clone()),
            gone.clone(),
        ],
        "{:#?}",
        m.needs_calls
    );
    let other_up = format!("set attention owner=needs kind=ask {other_entry}");
    assert_eq!(m.needs_calls, [put_up.as_str(), &other_up, &other_up, down]);
    let host = Arc::new(Hears::default());
    let (m, _) = heard_with(
        three(NEED, "nothing"),
        vec![1, 3, 5],
        &host,
        false,
        vec![
            err("attention too long (max 40 bytes)"),
            ok("OK\n"),
            lost(),
            lost(),
        ],
    );
    let cut = m.needs_calls[1]
        .strip_prefix("set attention owner=needs kind=ask ")
        .expect("the cut entry")
        .to_string();
    assert!(cut.len() <= 40 && cut != entry, "{cut}");
    assert_eq!(
        host.told(),
        [
            (vec![NEED.to_string()], entry),
            (vec![NEED.to_string()], cut.clone()),
            gone.clone(),
            (vec![NEED.to_string()], cut),
            gone,
        ],
        "{:#?}",
        m.needs_calls
    );
}

/// THE WINDOW FORGETS WITH THE LOOP WHERE THE ENTRY STOPS BEING ITS OWN
/// (`Session::needs_not_ours`): a set this loop put up and told, then a
/// claim behind another supervisor, then an Off park — the loop forgets
/// its set and sends nothing (the entry is the other's to keep), and the
/// window's host is told `(&[], "")`, so it keeps no words for an entry
/// this loop no longer holds; the claim back and the same set named again,
/// the host hears it again with the entry put up again.
#[test]
fn the_window_forgets_with_the_loop_where_the_entry_stops_being_ours() {
    let mut m = Mock::new(vec![needing("Pushed the parser fix; the release is next.")]);
    m.autopilot = Some("cruise");
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let host = Arc::new(Hears::default());
    let mut notes = Notes::default();
    let (calls_behind, forgot) = {
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        let _ = s
            .turn_end_at_point(&point, Some(Duration::from_secs(720)), &opts)
            .expect("decided");
        s.task_edge(&point.screen, &opts, &mut notes)
            .expect("the point's edge");
        let ours = std::mem::replace(
            &mut s.claim,
            claim::Claim::Behind {
                holder: "another".to_string(),
                tried: Instant::now(),
            },
        );
        assert_eq!(
            s.needs_down(101, "the session's autopilot is off", &mut notes),
            Ok(())
        );
        let forgot = s.needs_told.is_none();
        let calls_behind = s.ctl.needs_calls.len();
        s.claim = ours;
        s.task_edge(&point.screen, &opts, &mut notes)
            .expect("the point's edge, the claim back");
        (calls_behind, forgot)
    };
    let entry = format!("claude needs you: {NEED} — carrying on");
    let put_up = format!("set attention owner=needs kind=ask {entry}");
    assert!(forgot, "the set is forgotten");
    assert_eq!(calls_behind, 1, "nothing sent behind: {:#?}", m.needs_calls);
    assert_eq!(m.needs_calls, [put_up.as_str(), &put_up]);
    assert_eq!(
        host.told(),
        [
            (vec![NEED.to_string()], entry.clone()),
            (Vec::new(), String::new()),
            (vec![NEED.to_string()], entry),
        ]
    );
    let mut m = Mock::new(vec![idle_screen()]);
    let host = Arc::new(Hears::default());
    {
        let mut s = session(&mut m, None);
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        s.needs_told = Some(escalate::NEEDS_LEFT);
        s.claim = claim::Claim::Behind {
            holder: "another".to_string(),
            tried: Instant::now(),
        };
        assert_eq!(s.needs_down(101, "the loop ended", &mut notes), Ok(()));
    }
    assert!(host.told().is_empty(), "{:#?}", host.told());
    assert!(m.needs_calls.is_empty(), "{:#?}", m.needs_calls);
}

/// A TAPE RECORDED BEFORE THE HOST HEARD THE SET (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §12.7, "the engine lift"): the recorded run of
/// [`the_host_hears_the_whole_set_before_the_entry_goes_up`] with its
/// `host.needs_standing` outputs taken off, as a build before them taped it.
#[test]
fn an_old_tape_replays_in_acts_mode_and_strictly_differs_at_the_new_call() {
    use crate::supervise::tape::replay::{Options, replay};
    use crate::supervise::tape::report::Verdict;

    let host = Arc::new(Hears::default());
    let (_, run) = heard(long_need_then_none(), vec![1, 3], &host, false);
    let tape = crate::supervise::tape::show::open(&run.tape.reframed(&run.header.encode()), false)
        .expect("the tape reads");
    let kept: Vec<_> = tape
        .items
        .iter()
        .filter(|i| {
            !matches!(i.record(),
                Some(astream_effects::EffectRecord::Emit { door, .. }) if door == NEEDS_DOOR)
        })
        .cloned()
        .collect();
    assert_eq!(tape.items.len() - kept.len(), 2, "two tells taken off");
    let old = crate::supervise::tape::text::render(tape.header.as_deref(), &kept).into_bytes();
    let strict = replay(&old, &Options::default()).expect("a strict replay");
    match &strict.verdict {
        Verdict::Diverged(d) => {
            assert_eq!(d.door.as_deref(), Some(NEEDS_DOOR), "{}", strict.render());
        }
        other => panic!("the strict replay differs at the new call: {other:?}"),
    }
    let acts = replay(
        &old,
        &Options {
            candidate: Some(crate::supervise::tape::acts::Candidate::default()),
            ..Options::default()
        },
    )
    .expect("an acts replay");
    assert_eq!(acts.verdict, Verdict::Reproduced, "{}", acts.render());
    let account = &acts.acts.as_ref().expect("an acts report").account;
    assert_eq!(account.unmatched_outputs, 2, "{account:?}");
    assert_eq!(account.taken, account.acts, "{account:?}");
    assert_eq!(
        (
            account.dropped,
            account.repeated,
            account.keyed,
            account.tainted
        ),
        (0, 0, 0, 0),
        "{account:?}"
    );
}

/// A LOOP'S FIRST POINT READS A REPLY CUT AT THE ROWS' TOP WHOLE ON AGY
/// (`TurnEndState::reads_turn_whole`; the review of b9/spawn-residuals, F1):
/// a reply whose `NEEDS YOU:` row is above the screen, in the history, its
/// message's caret row with it, is read up to that message and its need put
/// up — a successor's first read after a seamless update, a restart after a
/// fault, Off to Land.
#[test]
fn a_first_point_reads_a_reply_cut_at_the_rows_top_whole() {
    use aterm_phase::agy::fixtures::lab::WORKER_FIRST_PROMPT;
    use aterm_phase::prompt::fixtures::screen;
    const ROWS: usize = 30;
    let fixture = screen(WORKER_FIRST_PROMPT);
    let rule = fixture
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("a turn's rule");
    let caret = fixture
        .iter()
        .rposition(|r| r == ">")
        .expect("the composer");
    let draw = |message: &[String], reply: &[String]| {
        let mut drawn = fixture[..=rule].to_vec();
        drawn.extend(message.iter().cloned());
        drawn.push(String::new());
        drawn.extend(reply.iter().map(|r| format!("  {r}")));
        drawn.push(String::new());
        drawn.extend(fixture[caret - 1..].iter().cloned());
        if drawn.len() > ROWS {
            let shown = drawn.split_off(drawn.len() - ROWS);
            (shown, drawn)
        } else {
            (drawn, Vec::new())
        }
    };
    let run = |(shown, above): (Vec<String>, Vec<String>), history: bool| {
        let mut m = Mock::new(vec![shown.clone()]);
        m.program = "agy";
        m.screen_cursors.insert(
            0,
            aterm_phase::agy::composer(&shown).expect("agy's composer"),
        );
        m.autopilot = Some("land");
        m.alt_screen = Some(false);
        if history {
            m.scrollback = Some(above);
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
            s.reconcile(&mut notes).expect("the loop's start");
            let screen = s.screen().expect("the point");
            let point = s.turn_of(screen);
            let _ = s.turn_end_at_point(&point, None, &opts).expect("decided");
            for _ in 0..2 {
                s.task_edge(&point.screen, &opts, &mut notes)
                    .expect("the point's edge");
            }
        }
        m.needs_calls.clone()
    };
    let raised = |calls: &[String]| {
        calls
            .iter()
            .any(|c| c.starts_with("set attention owner=needs ") && c.contains(NEED))
    };
    let prompt = vec!["> Fix the parser and land it.".to_string()];
    let report = |lines: usize| {
        let mut reply = vec![
            "Merged the lexer and its suites.".to_string(),
            format!("NEEDS YOU: {NEED}"),
        ];
        reply.extend((0..lines).map(|i| format!("- merged branch {i:02} and ran its suite")));
        reply.push("Pushing the parser next.".to_string());
        reply
    };
    let tall = draw(&prompt, &report(40));
    assert!(
        tall.1.iter().any(|r| r.contains(NEED)),
        "the need is above the screen"
    );
    let calls = run(tall, true);
    assert!(raised(&calls), "{calls:#?}");
    let short = draw(&prompt, &report(2));
    assert!(short.1.is_empty());
    let calls = run(short, true);
    assert!(raised(&calls), "{calls:#?}");
    let land = crate::supervise::policy::turn_end::land_text(None);
    let act: Vec<String> = {
        let words: Vec<&str> = land.split_whitespace().collect();
        let mut rows = Vec::new();
        let mut row = String::new();
        for w in words {
            if !row.is_empty() && row.chars().count() + 1 + w.chars().count() > 47 {
                rows.push(std::mem::take(&mut row));
            }
            if !row.is_empty() {
                row.push(' ');
            }
            row.push_str(w);
        }
        rows.push(row);
        rows.iter()
            .enumerate()
            .map(|(i, r)| format!("{}{r}", if i == 0 { "> " } else { "  " }))
            .collect()
    };
    let filler: Vec<String> = (0..6)
        .map(|i| format!("Step {i}: the parser's module and suite are in."))
        .collect();
    for history in [true, false] {
        let (mut shown, mut above) = draw(&act, &filler);
        above.append(&mut shown);
        let at = above
            .iter()
            .position(|r| r.starts_with("> [aterm harness]"))
            .expect("the act's first row");
        let shown = above.split_off(at + 2);
        assert!(
            above.iter().any(|r| r.starts_with("> [aterm harness]")),
            "the act's first row is above the screen"
        );
        assert!(
            shown.iter().any(|r| r.contains("NEEDS YOU:")),
            "the act's NEEDS YOU: row is on the screen"
        );
        let calls = run((shown, above), history);
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("set attention owner=needs ")),
            "history={history}: {calls:#?}"
        );
    }
}

/// The measured column-0 cells of agy's screen
/// `aterm_phase::agy::fixtures::lab::PERSON_PARAGRAPHS_LONG_REPLY`, by grid
/// row (`supervise/fixtures/agy-1.4.0-lab-person-paragraphs-long-reply.cells`).
fn agy_paragraph_cells() -> BTreeMap<(usize, usize), String> {
    include_str!("fixtures/agy-1.4.0-lab-person-paragraphs-long-reply.cells")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let (row, reply) = l.split_once(' ')?;
            Some(((row.parse().ok()?, 0), reply.to_string()))
        })
        .collect()
}

/// A PERSON'S PARAGRAPHS IN AGY ARE NEVER AGY'S NEED
/// (`Session::message_bounded_by_cells`; lab r16o): on agy's measured screen
/// of a person's message of two paragraphs — the second `NEEDS YOU: the
/// person's own words after a blank line.`, agy's blank row between them
/// like the one that ends the message — under the stub's long reply, the
/// loop reads the message's column-0 cells (the measured ones), takes the
/// blank row drawn in its caret row's style out of the message, and puts up
/// no need at the point: the reply is agy's alone.
#[test]
fn a_persons_paragraphs_in_agy_are_never_its_need() {
    use aterm_phase::agy::fixtures::lab::PERSON_PARAGRAPHS_LONG_REPLY;
    use aterm_phase::prompt::fixtures::screen;
    let theirs = "the person's own words after a blank line.";
    let run = |shown: Vec<String>, cells: bool| {
        let mut m = Mock::new(vec![shown.clone()]);
        m.program = "agy";
        m.screen_cursors.insert(
            0,
            aterm_phase::agy::composer(&shown).expect("agy's composer"),
        );
        m.autopilot = Some("cruise");
        if cells {
            m.cells = agy_paragraph_cells();
        } else {
            m.cell_unreadable = true;
        }
        let opts = SuperviseOpts {
            policy: owners_policy(),
            ..auto(30)
        };
        let mut notes = Notes::default();
        let again = {
            let mut s = session(&mut m, None);
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            s.reconcile(&mut notes).expect("the loop's start");
            let screen = s.screen().expect("the point");
            let point = s.turn_of(screen);
            let _ = s.turn_end_at_point(&point, None, &opts).expect("decided");
            for _ in 0..2 {
                s.task_edge(&point.screen, &opts, &mut notes)
                    .expect("the point's edge");
            }
            s.screen().expect("the same screen again").rows
        };
        let cells: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("cell "))
            .cloned()
            .collect();
        (m.needs_calls.clone(), cells, again)
    };
    let shown = screen(PERSON_PARAGRAPHS_LONG_REPLY);
    let (calls, cells, again) = run(shown.clone(), true);
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("set attention owner=needs ")),
        "{calls:#?}"
    );
    assert_eq!(
        cells[..3],
        ["cell 30 0", "cell 31 0", "cell 33 0"],
        "{cells:#?}"
    );
    assert!(cells.iter().all(|c| c.ends_with(" 0")), "{cells:#?}");
    let at = again
        .iter()
        .position(|r| r.starts_with("  NEEDS YOU: the person's own words"))
        .expect("the person's paragraph");
    assert_eq!(
        again[at - 1],
        "> Second message, first paragraph.",
        "{again:#?}"
    );
    assert!(again[at + 1].is_empty(), "{again:#?}");
    let mut needing = shown.clone();
    let advice = needing
        .iter()
        .position(|r| r.starts_with("  Nothing here is advice."))
        .expect("the reply's second paragraph");
    needing[advice] = format!("  NEEDS YOU: {NEED}");
    let (calls, _, _) = run(needing, true);
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("set attention owner=needs ") && c.contains(NEED)),
        "{calls:#?}"
    );
    let (calls, cells, _) = run(shown, false);
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("set attention owner=needs ") && c.contains(theirs)),
        "{calls:#?}"
    );
    assert!(!cells.is_empty(), "the cells were asked: {cells:#?}");
}

/// What the cells say of agy's screen at a loop's point (the runs of the
/// tests below): the screen read, [`Mock::cells`] for its grid rows or
/// none readable, and the history above it with [`Mock::history_cells`].
struct AgyCells {
    shown: Vec<String>,
    cells: Option<BTreeMap<(usize, usize), String>>,
    history: Option<(Vec<String>, Cells)>,
}

/// Measured `cell <r> <c>` replies by row and column ([`Mock::cells`]).
type Cells = BTreeMap<(usize, usize), String>;

/// What a loop's point on agy's screen came to ([`agy_point`]).
struct AgyPoint {
    /// The needs standing after it, every one (the entry names the first).
    needs: Vec<String>,
    /// The `cell` reads, in order.
    cells: Vec<String>,
    /// The screen read again after it.
    again: Vec<String>,
}

/// A loop's point under Cruise on agy's screen as `at` serves it, as
/// `a_persons_paragraphs_in_agy_are_never_its_need` runs one; the entry it
/// put up, if any, names the needs standing.
fn agy_point(at: AgyCells) -> AgyPoint {
    let mut m = Mock::new(vec![at.shown.clone()]);
    m.program = "agy";
    m.screen_cursors.insert(
        0,
        aterm_phase::agy::composer(&at.shown).expect("agy's composer"),
    );
    m.autopilot = Some("cruise");
    m.alt_screen = Some(false);
    match at.cells {
        Some(cells) => m.cells = cells,
        None => m.cell_unreadable = true,
    }
    if let Some((above, cells)) = at.history {
        m.scrollback = Some(above);
        m.history_cells = cells;
    }
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let mut notes = Notes::default();
    let (needs, again) = {
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.reconcile(&mut notes).expect("the loop's start");
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        let _ = s.turn_end_at_point(&point, None, &opts).expect("decided");
        for _ in 0..2 {
            s.task_edge(&point.screen, &opts, &mut notes)
                .expect("the point's edge");
        }
        let needs = s.turn_end.standing_needs().to_vec();
        (needs, s.screen().expect("the same screen again").rows)
    };
    let up = m
        .needs_calls
        .iter()
        .any(|c| c.starts_with("set attention owner=needs "));
    assert_eq!(up, !needs.is_empty(), "{:#?}", m.needs_calls);
    let cells: Vec<String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("cell "))
        .cloned()
        .collect();
    AgyPoint {
        needs,
        cells,
        again,
    }
}

/// The measured screen `PERSON_PARAGRAPHS_LONG_REPLY` with its reply drawn
/// `more` paragraphs of three longer (the stub's three again, a blank row
/// before each, as agy draws a reply's paragraphs: DRAWN, not measured),
/// and its cells: the measured ones, the rows drawn in a reply's
/// (`d0d0d0`, no attribute, as every measured reply row's), the composer's
/// moved down with it.
fn agy_long_reply(more: usize) -> (Vec<String>, BTreeMap<(usize, usize), String>) {
    use aterm_phase::agy::fixtures::lab::PERSON_PARAGRAPHS_LONG_REPLY;
    use aterm_phase::prompt::fixtures::screen;
    let measured = screen(PERSON_PARAGRAPHS_LONG_REPLY);
    let rule = measured
        .iter()
        .rposition(|r| r.starts_with('>'))
        .map(|caret| caret - 1)
        .expect("the composer's top rule");
    let first = measured
        .iter()
        .position(|r| r.starts_with("  This is a long canned reply"))
        .expect("the reply's first row");
    let paragraphs = &measured[first..rule - 1];
    let mut drawn = measured[..rule].to_vec();
    for _ in 0..more {
        drawn.push(String::new());
        drawn.extend(paragraphs.iter().cloned());
    }
    let added = drawn.len() - rule;
    drawn.extend(measured[rule..].iter().cloned());
    let measured_cells = agy_paragraph_cells();
    let reply_cell = "OK %20 d0d0d0 111318 none".to_string();
    let cells = (0..drawn.len())
        .map(|row| {
            let cell = if row < rule {
                measured_cells[&(row, 0)].clone()
            } else if row < rule + added {
                reply_cell.clone()
            } else {
                measured_cells[&(row - added, 0)].clone()
            };
            ((row, 0), cell)
        })
        .collect();
    (drawn, cells)
}

/// The person's paragraph after a blank line in the measured message.
const THEIRS: &str = "the person's own words after a blank line.";

/// `shown` with the first row from the bottom that starts with `prefix`
/// agy's own need, two columns in as its reply's rows.
fn agys_need_at(shown: &[String], prefix: &str) -> Vec<String> {
    let mut rows = shown.to_vec();
    let at = rows
        .iter()
        .rposition(|r| r.starts_with(prefix))
        .expect("a reply row");
    rows[at] = format!("  NEEDS YOU: {NEED}");
    rows
}

/// A PERSON'S PARAGRAPHS ABOVE THE ROWS READ ARE NEVER AGY'S NEED:
/// on a pane taller than the loop's 40-row tail
/// whose reply under the person's message runs past it — the message's
/// caret row, its blank row and its `NEEDS YOU:` paragraph above the tail's
/// first row, so its read bounds nothing and the reply reads cut at the
/// rows' top — the point's read of the turn whole reads the grid and bounds
/// the message there by its cells (`cell <r> 0` of the grid's rows), and no
/// need stands; agy's own need in that reply still does, alone.
#[test]
fn a_persons_paragraphs_above_the_rows_read_are_never_agys_need() {
    let (shown, cells) = agy_long_reply(3);
    let caret = shown
        .iter()
        .position(|r| r == "> Second message, first paragraph.")
        .expect("the person's caret row");
    assert!(
        shown.len() - 40 > caret + 3,
        "the message's rows are above the tail: {} rows",
        shown.len()
    );
    let p = agy_point(AgyCells {
        shown: shown.clone(),
        cells: Some(cells.clone()),
        history: None,
    });
    assert!(p.needs.is_empty(), "{:#?}", p.needs);
    let at = |row: usize| format!("cell {row} 0");
    assert!(
        p.cells
            .windows(3)
            .any(|w| w == [at(caret), at(caret + 1), at(caret + 3)]),
        "the grid's cells were read: {:#?}",
        p.cells
    );
    let p = agy_point(AgyCells {
        shown: agys_need_at(&shown, "  Nothing here is advice."),
        cells: Some(cells),
        history: None,
    });
    assert!(
        p.needs.len() == 1 && p.needs[0].starts_with(NEED),
        "{:#?}",
        p.needs
    );
    let p = agy_point(AgyCells {
        shown,
        cells: None,
        history: None,
    });
    assert!(p.needs.iter().any(|n| n == THEIRS), "{:#?}", p.needs);
}

/// A PERSON'S PARAGRAPHS SCROLLED INTO THE HISTORY ARE NEVER AGY'S NEED:
/// the person's message's caret row, its
/// blank row and its `NEEDS YOU:` paragraph scrolled up out of the grid, its
/// reply's later rows and the composer on it, the point's read of the turn
/// whole joins the history above the grid and bounds the message there by
/// the cells of the history's rows (`cell abs=<n> 0`, which reads a history
/// row's cells as they were drawn: `aterm-gui`'s
/// `a_history_rows_cell_reads_as_it_did_on_the_screen`), and no need
/// stands; agy's own need in that reply still does, alone.
#[test]
fn a_persons_paragraphs_scrolled_into_the_history_are_never_agys_need() {
    let (drawn, cells) = agy_long_reply(3);
    let caret = drawn
        .iter()
        .position(|r| r == "> Second message, first paragraph.")
        .expect("the person's caret row");
    let split = caret + 5;
    let run = |drawn: &[String], readable: bool| {
        let above = drawn[..split].to_vec();
        let shown = drawn[split..].to_vec();
        assert!(shown.len() <= 40, "{} rows", shown.len());
        let history_cells: BTreeMap<(usize, usize), String> = cells
            .iter()
            .filter(|((row, _), _)| *row < split)
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        let grid_cells: BTreeMap<(usize, usize), String> = cells
            .iter()
            .filter(|((row, _), _)| *row >= split)
            .map(|((row, col), v)| ((row - split, *col), v.clone()))
            .collect();
        agy_point(AgyCells {
            shown,
            cells: readable.then_some(grid_cells),
            history: Some((above, history_cells)),
        })
    };
    let p = run(&drawn, true);
    assert!(p.needs.is_empty(), "{:#?}", p.needs);
    let at = |row: usize| format!("cell abs={row} 0");
    assert!(
        p.cells
            .windows(3)
            .any(|w| w == [at(caret), at(caret + 1), at(caret + 3)]),
        "the history's cells were read: {:#?}",
        p.cells
    );
    let p = run(&agys_need_at(&drawn, "  Nothing here is advice."), true);
    assert!(
        p.needs.len() == 1 && p.needs[0].starts_with(NEED),
        "{:#?}",
        p.needs
    );
    let p = run(&drawn, false);
    assert!(p.needs.iter().any(|n| n == THEIRS), "{:#?}", p.needs);
}

/// A MESSAGE'S CELLS FOLD NOTHING THEY DO NOT PROVE
/// (`Session::message_cells`): on agy's
/// measured screen, the message's blank rows are taken out only where the
/// caret row's cell reads agy's caret, every blank row taken out reads a
/// blank in its style, and a plain blank row under them ends the message —
/// otherwise the screen stays as its text reads (FAIL TOWARD A KEPT NEED):
/// every blank row read in the caret's style (no row ends the message)
/// keeps agy's own need in its reply standing; a blank row that reads a
/// glyph, or reads nothing, under the message keeps agy's need on its
/// reply's first row standing; a caret row whose cell reads no `>` (the
/// screen moved under the reads) folds nothing, and the person's paragraph
/// stands as the text reads it.
#[test]
fn a_messages_cells_fold_nothing_they_do_not_prove() {
    use aterm_phase::agy::fixtures::lab::PERSON_PARAGRAPHS_LONG_REPLY;
    use aterm_phase::prompt::fixtures::screen;
    let shown = screen(PERSON_PARAGRAPHS_LONG_REPLY);
    let measured = agy_paragraph_cells();
    let caret = shown
        .iter()
        .position(|r| r == "> Second message, first paragraph.")
        .expect("the person's caret row");
    let (_, blanks) = aterm_phase::agy::message_blanks(&shown).expect("the message's blanks");
    assert_eq!(blanks[..2], [caret + 1, caret + 3], "{blanks:?}");
    let own = measured[&(caret + 1, 0)].clone();
    let tail = |rows: &[String]| rows[rows.len() - 40..].to_vec();
    let run = |shown: &[String], cells: BTreeMap<(usize, usize), String>| {
        agy_point(AgyCells {
            shown: shown.to_vec(),
            cells: Some(cells),
            history: None,
        })
    };
    let mut all_own = measured.clone();
    for &row in &blanks {
        all_own.insert((row, 0), own.clone());
    }
    let needing = agys_need_at(&shown, "  Nothing here is advice.");
    let p = run(&needing, all_own);
    assert!(
        p.needs.iter().any(|n| n.starts_with(NEED)),
        "every blank own: {:#?}",
        p.needs
    );
    assert_eq!(p.again, tail(&needing), "every blank own: nothing folded");
    let needing = agys_need_at(&shown, "  This is a long canned reply");
    for (name, cell) in [
        ("a glyph", "OK x d0d0d0 111318 none"),
        ("nothing", "ERR evicted"),
    ] {
        let mut odd = measured.clone();
        odd.insert((caret + 3, 0), cell.to_string());
        let p = run(&needing, odd);
        assert!(
            p.needs.iter().any(|n| n.starts_with(NEED)),
            "{name}: {:#?}",
            p.needs
        );
        assert_eq!(p.again, tail(&needing), "{name}: nothing folded");
    }
    let mut moved = measured.clone();
    moved.insert((caret, 0), own);
    let p = run(&shown, moved);
    assert!(
        p.needs.iter().any(|n| n == THEIRS),
        "no caret: {:#?}",
        p.needs
    );
    assert_eq!(p.again, tail(&shown), "no caret: nothing folded");
    let p = run(&shown, measured);
    assert!(p.needs.is_empty(), "{:#?}", p.needs);
    assert_ne!(p.again, tail(&shown));
}
