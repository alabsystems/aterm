// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `HOLD` journal records across point decisions, background breaks, and handoffs.

use super::*;

const SAID: &str = "Fixed the parser; the suite is green.";

fn break_screen() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Waiting for the build.",
        "",
        "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
        "",
    ]);
    r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
    r
}

fn screen_at(seq: u64, rows: Vec<String>) -> Screen {
    Screen {
        seq,
        ..read_screen(rows)
    }
}

fn holds(noted: &[String]) -> Vec<&str> {
    noted
        .iter()
        .filter(|l| l.starts_with("HOLD "))
        .map(String::as_str)
        .collect()
}

fn background_reads(host: Option<Arc<dyn IdleHost>>, reads: &[bool]) -> Vec<String> {
    let mut m = Mock::new(Vec::new());
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.stall_host = host;
    s.setup.background_settle = Duration::ZERO;
    let mut heard = Heard::default();
    let mut since = None;
    for (seq, on_break) in (1..).zip(reads) {
        let rows = if *on_break {
            break_screen()
        } else {
            busy_screen()
        };
        s.host_steps_in_background(&screen_at(seq, rows), &mut since, None, &mut heard)
            .expect("the break's step");
    }
    heard.noted
}

#[test]
fn a_background_break_no_host_wants_is_journaled_once_per_episode() {
    let reads = [true, true, true, false, true, true];
    let once_each = [
        "HOLD seq=1 background no-host-step",
        "HOLD seq=5 background no-host-step",
    ];
    let noted = background_reads(None, &reads);
    assert_eq!(holds(&noted), once_each, "{noted:#?}");
    let asks_nothing = Arc::new(Withholds::default());
    let noted = background_reads(Some(asks_nothing as Arc<dyn IdleHost>), &reads);
    assert_eq!(holds(&noted), once_each, "{noted:#?}");
    let host = Arc::new(BackgroundHost::default());
    let noted = background_reads(Some(Arc::clone(&host) as Arc<dyn IdleHost>), &reads);
    assert!(holds(&noted).is_empty(), "{noted:#?}");
    assert!(
        noted
            .iter()
            .any(|l| l.starts_with("HOST seq=1 background ")),
        "{noted:#?}"
    );
    assert!(host.backgrounds.load(Ordering::SeqCst) > 0, "{noted:#?}");
}

#[test]
fn a_held_break_names_its_guard_once() {
    let mut m = Mock::new(Vec::new());
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let host = Arc::new(BackgroundHost::default());
    s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
    s.setup.background_settle = Duration::ZERO;
    let opts = hosted(30);
    let point = screen_at(1, ended(SAID));
    let reading = s.turn_end_reading(&point, None, &opts);
    s.turn_end.acted(
        &TurnEndAction::Type {
            text: "keep going".to_string(),
            rule_id: RULE_CONTINUE,
        },
        &reading,
        Instant::now(),
    );
    assert!(s.turn_end.act_untaken());
    let mut heard = Heard::default();
    let mut since = None;
    for seq in 2..5 {
        s.host_steps_in_background(
            &screen_at(seq, break_screen()),
            &mut since,
            None,
            &mut heard,
        )
        .expect("the break's step");
    }
    assert_eq!(
        holds(&heard.noted),
        ["HOLD seq=2 background held why=act-untaken"],
        "{:#?}",
        heard.noted
    );
    assert_eq!(host.backgrounds.load(Ordering::SeqCst), 0);
    s.host_steps_in_background(&screen_at(5, busy_screen()), &mut since, None, &mut heard)
        .expect("the break's step");
    s.took(5, None, &mut heard);
    for seq in 6..8 {
        s.host_steps_in_background(
            &screen_at(seq, break_screen()),
            &mut since,
            None,
            &mut heard,
        )
        .expect("the break's step");
    }
    assert_eq!(holds(&heard.noted).len(), 1, "{:#?}", heard.noted);
    assert!(
        host.backgrounds.load(Ordering::SeqCst) > 0,
        "{:#?}",
        heard.noted
    );
}

/// A review half that runs attended (`watch`), noting what it is told.
#[derive(Default)]
struct Attended {
    noted: Vec<String>,
}

impl Review for Attended {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, _turn: &Turn) -> Result<bool, String> {
        Ok(true)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        crate::supervise::ledger::assert_marked(line);
        Ok(())
    }
    fn note(&mut self, line: &str) {
        crate::supervise::ledger::assert_marked(line);
        self.noted.push(line.to_string());
    }
    fn unattended(&self) -> bool {
        false
    }
}

#[test]
fn a_nothing_point_is_journaled_once_per_change() {
    let host = Arc::new(Withholds {
        on: AtomicBool::new(true),
    });
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut m = Mock::new(vec![ended(SAID)]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let mut heard = Heard::default();
    let decide = |s: &mut Session<'_, Mock>, seq: u64, action: TurnEndAction, heard: &mut Heard| {
        s.decided_withheld = host.on.load(Ordering::SeqCst);
        let point = s.turn_of(screen_at(seq, ended(SAID)));
        s.turn_end_execute(&point, action, &opts, &[], heard)
            .expect("the point's act");
    };
    let wait = || TurnEndAction::WaitUntil {
        until: Instant::now() + Duration::from_secs(600),
        why: "a person typing".to_string(),
        wait: WaitKind::Other,
    };
    decide(&mut s, 1, TurnEndAction::Nothing, &mut heard);
    decide(&mut s, 2, TurnEndAction::Nothing, &mut heard);
    host.on.store(false, Ordering::SeqCst);
    decide(&mut s, 3, TurnEndAction::Nothing, &mut heard);
    decide(&mut s, 4, wait(), &mut heard);
    decide(&mut s, 5, TurnEndAction::Nothing, &mut heard);
    assert_eq!(
        holds(&heard.noted),
        [
            "HOLD seq=1 point nothing why=withheld",
            "HOLD seq=3 point nothing why=policy",
            "HOLD seq=5 point nothing why=policy",
        ],
        "{:#?}",
        heard.noted
    );
    assert!(
        heard.noted.iter().any(|l| l.starts_with("WAITING seq=4 ")),
        "{:#?}",
        heard.noted
    );
    let mut attended = Attended::default();
    let point = s.turn_of(screen_at(6, ended(SAID)));
    s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut attended)
        .expect("the point's act");
    assert_eq!(
        holds(&attended.noted),
        ["HOLD seq=6 point nothing why=attended"]
    );
    let mut fresh = Mock::new(vec![ended(SAID)]);
    let mut s = session(&mut fresh, Some("@s-1".to_string()));
    let mut heard = Heard::default();
    decide(&mut s, 1, wait(), &mut heard);
    assert!(holds(&heard.noted).is_empty(), "{:#?}", heard.noted);
}

#[test]
fn a_successor_journals_its_first_hold_after_a_handoff() {
    let (dir, journal) = journal_file("hold-successor");
    let host = Arc::new(Withholds {
        on: AtomicBool::new(true),
    });
    let one_loop = || {
        let mut m = Mock::new(vec![ended(SAID)]);
        m.vanish_after = Some(1);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(journal.clone()),
            ..hosted(30)
        };
        let _ = watch_lines(&mut m, &opts);
        journal_records(&journal)
            .0
            .into_iter()
            .filter(|r| r.kind == "hold")
            .map(|r| (r.phase, r.summary))
            .collect::<Vec<_>>()
    };
    let held = ("point".to_string(), "nothing why=withheld".to_string());
    let first = one_loop();
    assert_eq!(first, std::slice::from_ref(&held));
    let both = one_loop();
    assert_eq!(both, [held.clone(), held]);
    let _ = std::fs::remove_dir_all(&dir);
    let until = Instant::now() + Duration::from_secs(600);
    let waited = || {
        let mut m = Mock::new(vec![ended(SAID)]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        let mut heard = Heard::default();
        for _ in 0..2 {
            let point = s.turn_of(screen_at(1, ended(SAID)));
            let action = TurnEndAction::WaitUntil {
                until,
                why: "a person typing".to_string(),
                wait: WaitKind::Other,
            };
            s.turn_end_execute(&point, action, &hosted(30), &[], &mut heard)
                .expect("the point's act");
        }
        heard
            .noted
            .iter()
            .filter(|l| l.starts_with("WAITING "))
            .count()
    };
    assert_eq!(waited(), 1, "once a loop");
    assert_eq!(waited(), 1, "and once its successor");
}

#[test]
fn a_point_decided_with_no_host_read_says_no_earlier_withheld() {
    let host = Arc::new(Withholds {
        on: AtomicBool::new(true),
    });
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut m = Mock::new(vec![ended(SAID)]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let mut heard = Heard::default();
    s.decided_withheld = true;
    let point = s.turn_of(screen_at(1, ended(SAID)));
    s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
        .expect("the point's act");
    let point = s.turn_of(screen_at(2, ended(SAID)));
    let wait = TurnEndAction::WaitUntil {
        until: Instant::now() + Duration::from_secs(600),
        why: "a person typing".to_string(),
        wait: WaitKind::Other,
    };
    s.turn_end_execute(&point, wait, &opts, &[], &mut heard)
        .expect("the point's act");
    s.claim = claim::Claim::Behind {
        holder: "another".to_string(),
        tried: Instant::now(),
    };
    let point = s.turn_of(screen_at(3, ended(SAID)));
    let action = s
        .turn_end_at_point(&point, None, &opts)
        .expect("the point decided");
    assert_eq!(action, TurnEndAction::Nothing);
    s.turn_end_execute(&point, action, &opts, &[], &mut heard)
        .expect("the point's act");
    assert_eq!(
        holds(&heard.noted),
        [
            "HOLD seq=1 point nothing why=withheld",
            "HOLD seq=3 point nothing why=policy",
        ],
        "{:#?}",
        heard.noted
    );
}

#[test]
fn a_break_no_host_wants_is_said_only_once_it_stood_the_settle() {
    let clock = Arc::new(crate::supervise::fx::ScriptedClock::default());
    clock.stand_mono();
    let mut m = Mock::new(Vec::new());
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(&clock))));
    let settle = crate::supervise::run::background_settle();
    assert!(settle > Duration::ZERO, "the shipped settle");
    s.setup.background_settle = settle;
    let mut heard = Heard::default();
    let mut since = None;
    let mut read = |s: &mut Session<'_, Mock>, seq: u64, on_break: bool, heard: &mut Heard| {
        let rows = if on_break {
            break_screen()
        } else {
            busy_screen()
        };
        s.host_steps_in_background(&screen_at(seq, rows), &mut since, None, heard)
            .expect("the break's step");
    };
    for seq in 1..=8 {
        read(&mut s, seq, seq % 2 == 1, &mut heard);
        clock.skip(Duration::from_secs(1));
    }
    assert!(holds(&heard.noted).is_empty(), "{:#?}", heard.noted);
    read(&mut s, 9, true, &mut heard);
    clock.skip(settle - Duration::from_millis(1));
    read(&mut s, 10, true, &mut heard);
    assert!(holds(&heard.noted).is_empty(), "{:#?}", heard.noted);
    clock.skip(Duration::from_millis(1));
    read(&mut s, 11, true, &mut heard);
    read(&mut s, 12, true, &mut heard);
    assert_eq!(
        holds(&heard.noted),
        ["HOLD seq=11 background no-host-step"],
        "{:#?}",
        heard.noted
    );
}

#[path = "occupancy_loop_tests.rs"]
mod occupancy;
