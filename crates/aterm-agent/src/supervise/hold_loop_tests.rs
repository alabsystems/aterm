// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// WHY LAND STILL HOLDS, TOLD BY THE LOOP (`docs/DESIGN-harness-autopilot-2026-10-15.md`
// §12.3.3 item 2): what a recording host is told through
// `IdleHost::land_held` — by the loop's one teller (`Session::tell_hold`),
// where the face is told (each start, each point folded in, each live read
// of someone else's turn), at each act's ledger row, at an Off park, and
// `None` at the loop's end where a hold was told — only where the hold
// moved. And a tape recorded before the door replays in acts mode. Each
// behaviour has its negative control. Included from `run_tests.rs` (`mod
// hold_loop`), so the `Mock` and its helpers are in scope.

use crate::harness::acts::{HoldLeg, LandHeld, TaskFace};
use crate::supervise::policy::turn_end::{RULE_LAND, RULE_LAND_NUDGE};

/// The door a hold crosses on a tape.
const HOLD_DOOR: &str = "host.land_held";

/// A reply that says no done.
const PROGRESS: &str = "Committing the parser now; the lexer is next.";
/// The retired token: a done claim, no defense.
const TOKEN: &str = "Built and tested the parser.\nATERM-TASK-DONE";
/// A defense that names open work: Land's strict reading holds it.
const OPEN_DEFENSE: &str = "Nothing left: the parser is pushed.\n- Its branch is not merged yet.";
/// The journal's note of a landing at the instruction's yield, as the loop
/// writes it.
const LANDED_NOTE: &str = "DONE seq=104 rule=land@v1 the worker said it is done after the land \
     instruction (autopilot land): nothing more is typed until a person or an orchestrator types";
/// The landing's clean defense.
const LANDING_REPLY: &str = "Committed and pushed the parser and its tests.\nNothing left: all of \
                             it is merged into the remote main branch.";

/// What the host was told, in order: the task's face, the hold, and each
/// decided idle point of a Claude Code (`IdleHost::usage_tap`, told as the
/// point is read, before it is folded in): the mark a tell made before any
/// point stands before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Told {
    Face(TaskFace),
    Hold(Option<LandHeld>),
    Point,
}

/// A host that records each face, each hold and each point it is told.
#[derive(Debug, Default)]
struct Holds {
    told: Mutex<Vec<Told>>,
}

impl Holds {
    fn told(&self) -> Vec<Told> {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The holds told, in order.
    fn holds(&self) -> Vec<Option<LandHeld>> {
        self.told()
            .into_iter()
            .filter_map(|t| match t {
                Told::Hold(h) => Some(h),
                Told::Face(_) | Told::Point => None,
            })
            .collect()
    }

    /// The faces and holds told, in order, the points left out.
    fn tells(&self) -> Vec<Told> {
        self.told()
            .into_iter()
            .filter(|t| *t != Told::Point)
            .collect()
    }
}

impl IdleHost for Holds {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn task_face(&self, face: TaskFace) {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Told::Face(face));
    }
    fn land_held(&self, held: Option<LandHeld>) {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Told::Hold(held));
    }
    fn usage_tap(&self) {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Told::Point);
    }
}

/// A turn that ended after 3 minutes of work, the worker's last words `said`
/// (each line its own row), at an empty composer.
fn ended(said: &str) -> Vec<String> {
    let mut lines: Vec<String> = said.lines().map(|l| format!("⏺ {l}")).collect();
    for l in lines.iter_mut().skip(1) {
        *l = l.replacen("⏺ ", "  ", 1);
    }
    lines.extend([
        String::new(),
        "✻ Worked for 3m 2s · done 4:24 PM".to_string(),
        String::new(),
    ]);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut r = rows(&refs);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// The scripted server over `screens`, the session's word `word`, its
/// screens at `gates` held until the loop types.
fn scripted(screens: Vec<Vec<String>>, word: &'static str, gates: Vec<usize>) -> Mock {
    let mut m = Mock::new(screens);
    m.autopilot = Some(word);
    m.turn_gates = gates;
    m.vanish_after = Some(6);
    m.stall_sleep = Some(Duration::from_millis(20));
    m.history_served();
    m
}

/// The hosted loop's options over `host`, its approval ledger at `ledger`.
fn hosted(host: &Arc<Holds>, ledger: Option<&Path>) -> SuperviseOpts {
    SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..owners_policy()
        },
        journal: ledger.map(crate::supervise::approvals::journal_beside),
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..auto(30)
    }
}

/// The test's pace: a 30 ms back-off.
fn at_test_speed(s: &mut Watched<'_, '_>) {
    s.set_turn_end_timing(TurnEndTiming {
        short_backoff: Duration::from_millis(30),
        short_backoff_max: Duration::from_millis(60),
        ..TurnEndTiming::default()
    });
}

/// A run of `m` over `host`: the lines said.
fn run(m: &mut Mock, host: &Arc<Holds>, ledger: Option<&Path>) -> Vec<String> {
    let opts = hosted(host, ledger);
    let (lines, _) = watch_lines_with(m, &opts, |s| {
        at_test_speed(s);
        s.set_approval_ledger(ledger.map(Path::to_path_buf));
    });
    lines
}

/// The instruction, a nudge and the work between: the instruction's yield
/// is progress (no defense), the nudge's a done claim (the same legs, its
/// back-off read through), then SOMEONE ELSE'S TURN read live, and after
/// its end another nudge.
fn a_landing_with_someone_elses_turn() -> Vec<Vec<String>> {
    vec![
        busy_screen(),
        ended(PROGRESS),
        busy_screen(),
        ended(PROGRESS),
        busy_screen(),
        ended(TOKEN),
        busy_screen(),
        ended(PROGRESS),
        busy_screen(),
    ]
}

/// A hold of `nudges` since `since`, read with `legs` (none: unread).
fn hold(since: Option<i64>, nudges: u32, legs: Option<&[HoldLeg]>) -> LandHeld {
    LandHeld {
        since_ms: since,
        nudges,
        read: legs.is_some(),
        legs: legs.unwrap_or_default().iter().fold(0, |b, l| b | l.bit()),
        ..LandHeld::default()
    }
}

/// THE HOLD IS TOLD ONCE PER MOVE, AND AT EACH LAND ACT'S ROW (§12.3.6,
/// 3b): none at the start; the instruction's row tells its `since` and no
/// legs; its yield tells the legs that held it (no defense); the nudge's
/// row tells the new count at once; the nudge's yield, held the same way,
/// tells nothing (two equal points, one call); someone else's turn read live
/// drops the legs, one call; and the next nudge's row counts two. NEGATIVE
/// CONTROLS, as mutants of the loop (the slice's commit names each): a
/// teller asked at every point, its move unchecked, tells the equal points
/// twice; one never asked where the face is told tells nothing past the
/// rows. That each tell comes before the next point, not only in this
/// order, is [`a_land_acts_row_and_a_live_read_are_told_before_any_point`]'s.
#[test]
fn the_hold_is_told_once_per_move_and_at_each_nudge() {
    let host = Arc::new(Holds::default());
    let mut m = scripted(a_landing_with_someone_elses_turn(), "land", vec![1, 3, 7]);
    let lines = run(&mut m, &host, None);
    assert_eq!(
        count(&m, "turn"),
        3,
        "the instruction and two nudges: {lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND} "))),
        "{lines:#?}"
    );
    let told = host.holds();
    let since = told
        .iter()
        .flatten()
        .next()
        .and_then(|h| h.since_ms)
        .expect("the instruction's row is the hold's since");
    let since = Some(since);
    let defense: &[HoldLeg] = &[HoldLeg::Defense];
    assert_eq!(
        told,
        [
            None,
            Some(hold(since, 0, None)),
            Some(hold(since, 0, Some(defense))),
            Some(hold(since, 1, Some(defense))),
            Some(hold(since, 1, None)),
            Some(hold(since, 2, None)),
        ],
        "{lines:#?}"
    );
    // Told beside the face, before any point: the start's face first, then
    // its hold.
    assert_eq!(
        host.told().get(..2),
        Some(&[Told::Face(TaskFace::Open), Told::Hold(None)][..])
    );
}

/// A LAND ACT'S ROW AND A LIVE READ ARE TOLD BEFORE ANY POINT (§12.3.3,
/// item 2): a session gone at its first wait after the nudge is typed, no
/// read of the nudge's turn, was told the nudge's count; one gone while
/// someone else's turn runs, read live and never ended, was told its legs
/// dropped. Neither waits for a point that never comes. NEGATIVE CONTROLS
/// (mutants of the loop): a teller asked only where the face is told
/// leaves the count one behind; one beside `fold_point`'s call alone, not
/// inside `tell_face`, leaves the last yield's legs standing.
#[test]
fn a_land_acts_row_and_a_live_read_are_told_before_any_point() {
    // The instruction's yield sits until the nudge is typed; the session
    // is gone at the first wait after it.
    let host = Arc::new(Holds::default());
    let mut m = scripted(
        vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(PROGRESS),
        ],
        "land",
        vec![1, 3],
    );
    m.exit_when = Some(|m| m.turns >= 2);
    let lines = run(&mut m, &host, None);
    assert_eq!(
        count(&m, "turn"),
        2,
        "the instruction and a nudge: {lines:#?}"
    );
    let last = host.holds().last().copied().flatten().expect("held");
    assert_eq!(
        (last.nudges, last.read),
        (1, true),
        "the nudge's count, its yield's legs kept: {:#?}",
        host.told()
    );

    // The nudge's yield read, then someone else's turn, read live (the
    // script of the test above, to that turn); the session is gone before
    // its end.
    let host = Arc::new(Holds::default());
    let mut screens = a_landing_with_someone_elses_turn();
    screens.truncate(7);
    let mut m = scripted(screens, "land", vec![1, 3]);
    m.exit_when = Some(|m| m.served >= 7);
    let lines = run(&mut m, &host, None);
    assert_eq!(
        count(&m, "turn"),
        2,
        "the instruction and a nudge: {lines:#?}"
    );
    let told = host.holds();
    assert!(
        told.iter().flatten().any(|h| h.read && h.nudges == 1),
        "the nudge's yield read first: {told:#?}"
    );
    let last = told.last().copied().flatten().expect("held");
    assert_eq!(
        (last.nudges, last.read, last.legs),
        (1, false, 0),
        "the legs dropped at the live read: {told:#?}\n{lines:#?}"
    );
}

/// A ledger row as the loop writes it, `ago` before now: the row, and its
/// stamp (unix ms).
fn ledger_row(rule_id: &str, command: &str, ago: Duration) -> (String, i64) {
    let row = crate::supervise::approvals::Row {
        rule_id,
        outcome: crate::supervise::approvals::Outcome::Typed,
        command,
        reason: "-",
        box_seq: 1,
    };
    let at = crate::supervise::journal::unix_ms() - i64::try_from(ago.as_millis()).unwrap();
    (row.to_json(at, None), at)
}

/// A ledger holding `rows`, its journal beside it holding `journal`.
fn ledger(tag: &str, rows: &[String], journal: &[String]) -> (PathBuf, PathBuf) {
    let dir = crate::supervise::test_scratch_path("hold-loop", tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("s-land.jsonl");
    let body = |rows: &[String]| rows.iter().map(|r| format!("{r}\n")).collect::<String>();
    std::fs::write(&path, body(rows)).expect("ledger");
    std::fs::write(
        crate::supervise::approvals::journal_beside(&path),
        body(journal),
    )
    .expect("journal");
    (dir, path)
}

/// EACH START TELLS THE SEED ONCE (§12.3.6, 3b; the loop's starts all reach
/// `tell_start_face`): a restart over a Land episode its ledger holds — the
/// instruction ten minutes back, a nudge five — tells, before any point,
/// the hold it seeds (the instruction row's stamp, one nudge, unread) and
/// nothing again until it moves; a restart over a landing (its face told
/// at the start's later site, once the session's word is read) tells none;
/// a fresh start tells none. NEGATIVE CONTROL (a mutant of the loop): a
/// `tell_start_face` without the hold's teller tells nothing at the start,
/// and the seeded hold first after the first point.
#[test]
fn each_start_tells_the_seed_once() {
    let min = |n: u64| Duration::from_secs(60 * n);
    // A seeded episode.
    let (land_row, since) = ledger_row(RULE_LAND, "land", min(10));
    let (nudge_row, _) = ledger_row(RULE_LAND_NUDGE, "still landing", min(5));
    let (dir, path) = ledger("seeded", &[land_row, nudge_row], &[]);
    let host = Arc::new(Holds::default());
    let mut m = scripted(vec![ended(PROGRESS)], "land", Vec::new());
    m.vanish_after = Some(0);
    let lines = run(&mut m, &host, Some(&path));
    let _ = std::fs::remove_dir_all(&dir);
    let seeded = Some(hold(Some(since), 1, None));
    let told = host.told();
    assert!(told.contains(&Told::Point), "a point was read: {told:#?}");
    assert_eq!(
        told.get(..2),
        Some(&[Told::Face(TaskFace::Open), Told::Hold(seeded)][..]),
        "before any point: {lines:#?}"
    );
    assert_eq!(
        host.holds().iter().filter(|h| **h == seeded).count(),
        1,
        "told once: {told:#?}"
    );

    // A restart over a landing (the journal's `DONE` note of the
    // instruction's yield): its face, then no hold.
    let (land_row, _) = ledger_row(RULE_LAND, "land", min(10));
    let at = crate::supervise::journal::unix_ms() - i64::try_from(min(9).as_millis()).unwrap();
    let note = crate::supervise::journal::JournalRecord::of_line(at, None, LANDED_NOTE, None);
    let (dir, path) = ledger("landed", &[land_row], &[note.to_json()]);
    let host = Arc::new(Holds::default());
    let mut m = scripted(vec![ended(LANDING_REPLY)], "land", Vec::new());
    m.vanish_after = Some(0);
    let lines = run(&mut m, &host, Some(&path));
    let _ = std::fs::remove_dir_all(&dir);
    let tells = host.tells();
    assert_eq!(
        tells.get(..2),
        Some(&[Told::Face(TaskFace::Landed), Told::Hold(None)][..]),
        "{tells:#?}\n{lines:#?}"
    );
    assert_eq!(host.holds(), [None], "{tells:#?}");

    // A fresh start, nothing to seed: none, once.
    let host = Arc::new(Holds::default());
    let mut m = scripted(vec![ended(PROGRESS)], "off", Vec::new());
    let _ = run(&mut m, &host, None);
    assert_eq!(host.holds(), [None]);
}

/// THE HOLD ENDS AT A LANDING, told beside the face as it moves (§12.3.3):
/// the instruction's yield names open work (held, read); the nudge's clean
/// defense lands it — told none right after `landed`. (A person's own
/// exchange moves the face the same way, through the same teller: the
/// Tier-1 walk, `supervise_conformance_land.rs`, holds the hold told there
/// to none.) NEGATIVE CONTROL: the nudge's yield naming open work again
/// lands nothing, and the hold stands, read.
#[test]
fn the_hold_ends_at_a_landing() {
    let landing = |last: &str| {
        let host = Arc::new(Holds::default());
        let mut m = scripted(
            vec![
                busy_screen(),
                ended(PROGRESS),
                busy_screen(),
                ended(OPEN_DEFENSE),
                busy_screen(),
                ended(last),
            ],
            "land",
            vec![1, 3],
        );
        let lines = run(&mut m, &host, None);
        (host.tells(), lines)
    };
    let (told, lines) = landing(LANDING_REPLY);
    let tail: Vec<Told> = told.iter().rev().take(2).rev().copied().collect();
    assert_eq!(
        tail,
        [Told::Face(TaskFace::Landed), Told::Hold(None)],
        "{told:#?}\n{lines:#?}"
    );
    // NEGATIVE CONTROL: open work named again holds it.
    let (told, lines) = landing(OPEN_DEFENSE);
    let last = told.iter().rev().find_map(|t| match t {
        Told::Hold(h) => Some(*h),
        Told::Face(_) | Told::Point => None,
    });
    let open = last.flatten().expect("held");
    assert!(
        open.read && open.holds(HoldLeg::Open) && open.nudges >= 1,
        "{open:?}\n{lines:#?}"
    );
    assert!(!told.contains(&Told::Face(TaskFace::Landed)), "{told:#?}");
}

/// A Land episode with its instruction typed and its first yield judged:
/// the hold read (no defense yet), in `st`.
fn read_hold(st: &mut crate::supervise::policy::turn_end::TurnEndState) {
    use crate::supervise::policy::turn_end::{TurnEndReading, decide_turn_end};
    let now = Instant::now();
    let point = |said: &str, worked: Option<Duration>| {
        let r = ended(said);
        TurnEndReading {
            autopilot: Autopilot::Land,
            worked,
            ..TurnEndReading::of(
                &aterm_phase::ClaudeReader.read(&r, Some(2)),
                &r,
                false,
                None,
                None,
                None,
            )
        }
    };
    let cfg = owners_policy();
    let first = point(PROGRESS, Some(Duration::from_secs(180)));
    st.observe(&first, now);
    let a = decide_turn_end(st, &first, &cfg, now + Duration::from_secs(3600));
    assert_eq!(a.rule_id(), Some(RULE_LAND), "{a:?}");
    st.acted(&a, &first, now);
    st.ledgered(RULE_LAND, 1_771_000_000_000);
    let reply = point(PROGRESS, Some(Duration::from_secs(240)));
    st.observe(&reply, now + Duration::from_secs(300));
    assert!(
        st.land_held().is_some_and(|h| h.read),
        "{:?}",
        st.land_held()
    );
}

/// THE HOLD IS TOLD GONE AT AN OFF PARK AND AT THE LOOP'S END (§12.3.3): a
/// hold told, then the park's first act (`off_stood`, which ends the
/// episode with no point folded in) tells none; a hold told, then the
/// loop's end (`release_attention`) tells none; a loop that told none tells
/// nothing at its end. NEGATIVE CONTROLS (mutants of the loop): without the
/// park's tell, or the end's, the window keeps the hold.
#[test]
fn the_hold_is_told_gone_at_an_off_park_and_at_the_loops_end() {
    let mut notes = Alone;
    for at_off in [true, false] {
        let mut m = Mock::new(vec![idle_screen()]);
        let host = Arc::new(Holds::default());
        {
            let mut s = session(&mut m, None);
            s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
            read_hold(&mut s.turn_end);
            s.tell_hold();
            if at_off {
                s.off_stood(101);
            } else {
                s.release_attention(&mut notes);
            }
        }
        let told = host.holds();
        assert_eq!(told.len(), 2, "at_off={at_off}: {told:#?}");
        assert!(told[0].is_some_and(|h| h.read), "{told:#?}");
        assert_eq!(told[1], None, "at_off={at_off}");
    }
    // A loop that told no hold tells nothing at its end.
    let mut m = Mock::new(vec![idle_screen()]);
    let host = Arc::new(Holds::default());
    {
        let mut s = session(&mut m, None);
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        s.tell_hold();
        s.release_attention(&mut notes);
    }
    assert_eq!(host.holds(), [None], "the first ask alone");
}

/// CRUISE CHOSEN AT A HELD POINT ENDS THE HOLD AS ITS ACT IS TYPED (the
/// third review of 3b): the instruction typed, its yield names open work
/// (held, read), and the owner switches the session to Cruise just as that
/// yield is drawn. The point is folded in under the word last read (Land:
/// the hold read is told), then decided under the word read fresh: Cruise's
/// act, typed and ledgered, tells none at its row — the screen has not
/// moved since (no live read of the act's turn, which would tell it a read
/// later), and the session is gone at the first wait after it. NEGATIVE
/// CONTROL (a mutant of the loop, the landing commit names it): a
/// `ledger_row` that tells the hold at Land's own rows alone leaves the read
/// hold told under Cruise.
#[test]
fn a_cruise_act_at_a_held_point_tells_the_hold_gone_as_it_is_typed() {
    let host = Arc::new(Holds::default());
    let mut m = scripted(
        vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(OPEN_DEFENSE),
        ],
        "land",
        vec![1, 3],
    );
    m.autopilot_on.insert(3, Some("cruise"));
    m.exit_when = Some(|m| m.turns >= 2);
    let lines = run(&mut m, &host, None);
    assert_eq!(
        count(&m, "turn"),
        2,
        "the instruction and Cruise's act: {lines:#?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND_NUDGE} "))),
        "no nudge under Cruise: {lines:#?}"
    );
    let told = host.holds();
    let read = told
        .iter()
        .flatten()
        .find(|h| h.read)
        .copied()
        .unwrap_or_else(|| panic!("the yield's hold told, read: {told:#?}\n{lines:#?}"));
    assert!(read.holds(HoldLeg::Open), "{read:?}");
    assert_eq!(
        told.last(),
        Some(&None),
        "Cruise's act tells the hold gone: {:#?}\n{lines:#?}",
        host.told()
    );
}

/// THE HOLD IS TOLD ONLY WHERE IT IS THIS LOOP'S (§12.3.5,
/// `Session::hold_is_ours`): a hold told, then the session's word read in
/// another mode (a box's `meta` under Cruise, no reading of the switch
/// folded in yet) tells none, the episode kept, and the word read Land
/// again tells the hold again; a word left to the table's tells the
/// episode's hold. Behind another supervisor's claim the hold is told none
/// (2b's rule for the needs: that loop keeps the episode), and the claim
/// back tells it again. NEGATIVE CONTROLS: the word read Land again, and
/// the claim given back, each tell the kept hold, so it was the word and
/// the claim that made it none; and a mutant of the loop that tells
/// whatever the episode keeps (no `hold_is_ours`) keeps the hold told
/// under Cruise and behind the claim (the landing commit names it).
#[test]
fn a_hold_is_told_none_outside_land_and_behind_another_claim() {
    let reply = |word: &str| format!("OK title=- autopilot={word}\n");
    for (word, ours) in [
        ("cruise", false),
        ("off", false),
        ("land", true),
        ("-", true),
    ] {
        let mut m = Mock::new(vec![idle_screen()]);
        let host = Arc::new(Holds::default());
        {
            let mut s = session(&mut m, None);
            s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
            read_hold(&mut s.turn_end);
            s.note_meta(&reply("land"));
            s.tell_hold();
            s.note_meta(&reply(word));
            s.tell_hold();
            s.note_meta(&reply("land"));
            s.tell_hold();
        }
        let told = host.holds();
        let held = told[0].expect("the hold read, told");
        assert!(held.read, "{told:#?}");
        if ours {
            assert_eq!(told, [Some(held)], "word={word}: no move");
        } else {
            assert_eq!(
                told,
                [Some(held), None, Some(held)],
                "word={word}: none, then the hold again under Land"
            );
        }
    }
    let mut m = Mock::new(vec![idle_screen()]);
    let host = Arc::new(Holds::default());
    {
        let mut s = session(&mut m, None);
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        read_hold(&mut s.turn_end);
        s.tell_hold();
        let ours = std::mem::replace(
            &mut s.claim,
            claim::Claim::Behind {
                holder: "another".to_string(),
                tried: Instant::now(),
            },
        );
        s.tell_hold();
        s.tell_hold();
        s.claim = ours;
        s.tell_hold();
    }
    let told = host.holds();
    let held = told[0].expect("the hold read, told");
    assert_eq!(
        told,
        [Some(held), None, Some(held)],
        "none behind the claim, once; the hold again with it back"
    );
}

/// A TAPE RECORDED BEFORE THE HOLD'S DOOR (§12.7, "the engine lift"): the
/// recorded run of [`the_hold_is_told_once_per_move_and_at_each_nudge`]'s
/// script with its `host.land_held` outputs taken off, as a build before
/// them taped it. In acts mode it is REPRODUCED, every act taken, the tells
/// the only records the replay could not match (`unmatched_outputs`,
/// nothing dropped, repeated, keyed or tainted); a strict replay diverges,
/// and at `host.land_held`, the new call. NEGATIVE CONTROL: the tape as
/// recorded replays strict and in acts mode exactly (`assert_replays`).
#[test]
fn an_old_tape_replays_in_acts_mode_and_strictly_differs_at_the_hold() {
    use crate::supervise::tape::replay::{Options, replay};
    use crate::supervise::tape::report::Verdict;

    let host = Arc::new(Holds::default());
    let mut m = scripted(a_landing_with_someone_elses_turn(), "land", vec![1, 3, 7]);
    m.vanish_after = Some(8);
    m.stall_sleep = Some(Duration::from_secs(20));
    let opts = hosted(&host, None);
    let run = record_watch(&mut m, &opts, at_test_speed);
    crate::supervise::tape::testing::assert_replays(&run.tape, &run.header, &run.sent, &run.lines);
    let tells = host.holds().len();
    assert!(tells >= 4, "{:#?}", host.told());
    let tape = crate::supervise::tape::show::open(&run.tape.reframed(&run.header.encode()), false)
        .expect("the tape reads");
    let kept: Vec<_> = tape
        .items
        .iter()
        .filter(|i| {
            !matches!(i.record(),
                Some(astream_effects::EffectRecord::Emit { door, .. }) if door == HOLD_DOOR)
        })
        .cloned()
        .collect();
    assert_eq!(tape.items.len() - kept.len(), tells, "every tell taken off");
    let old = crate::supervise::tape::text::render(tape.header.as_deref(), &kept).into_bytes();
    let strict = replay(&old, &Options::default()).expect("a strict replay");
    match &strict.verdict {
        Verdict::Diverged(d) => {
            assert_eq!(d.door.as_deref(), Some(HOLD_DOOR), "{}", strict.render());
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
    assert_eq!(account.unmatched_outputs, tells, "{account:?}");
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
