// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SIZE: (u16, u16) = (61, 200);
const SHORT: (u16, u16) = (60, 200);

struct Rig {
    m: RepaintNudge,
    f: Facts,
    now: u64,
}

impl Rig {
    fn new(mode: Mode) -> Self {
        let mut rig = Self {
            m: RepaintNudge::new(),
            f: Facts::eligible(mode, SIZE),
            now: 100_000,
        };
        assert!(rig.m.arm(mode, Reason::Debt, 1, rig.now));
        rig
    }

    fn step(&mut self) -> Act {
        self.m.step(&self.f, self.now)
    }

    fn at(&mut self, ms: u64) -> Act {
        self.now += ms;
        self.step()
    }

    fn answer(&mut self) {
        self.f.full_clears += 1;
    }

    fn hold(&mut self) {
        assert_eq!(self.step(), Act::Hold);
        self.f.pty = SHORT;
    }

    fn release(&mut self) {
        self.f.pty = SIZE;
    }
}

#[test]
fn a_standing_gate_defers_the_hold_and_a_capped_one_is_abandoned() {
    type Set = fn(&mut Facts, bool);
    let gates: [(&str, Set, Option<Gate>); 4] = [
        ("lease", |f, on| f.driver_may_type = on, Some(Gate::Lease)),
        ("selection", |f, on| f.selection = on, Some(Gate::Selection)),
        ("frame", |f, on| f.frame_open = on, Some(Gate::Busy)),
        ("other hold", |f, on| f.other_hold = on, None),
    ];
    for (name, set, gate) in gates {
        let mut r = Rig::new(Mode::On);
        set(&mut r.f, true);
        for _ in 0..20 {
            assert!(matches!(r.at(100), Act::WaitUntil(_)), "{name}");
        }
        assert_eq!(r.m.phase(), Phase::Armed, "{name}");
        set(&mut r.f, false);
        assert_eq!(r.at(1), Act::Hold, "{name}");
        let Some(gate) = gate else { continue };
        let mut r = Rig::new(Mode::On);
        set(&mut r.f, true);
        let mut last = Act::Nothing;
        for _ in 0..600 {
            last = r.at(250);
            if matches!(last, Act::GaveUp(_)) {
                break;
            }
        }
        assert_eq!(last, Act::GaveUp(GiveUp::Deferred(gate)), "{name}");
        assert_eq!(r.m.phase(), Phase::Rest, "{name}");
    }
}

#[test]
fn nudge_never_runs_within_1500ms_of_a_persons_key() {
    let mut r = Rig::new(Mode::On);
    r.f.person_quiet_ms = 100;
    assert_eq!(r.step(), Act::WaitUntil(r.now + PERSON_GATE_MS - 100));
    r.f.person_quiet_ms = PERSON_GATE_MS;
    assert_eq!(r.step(), Act::Hold);
}

#[test]
fn a_session_never_stamped_waits_1500ms_from_attach() {
    assert_eq!(person_quiet_ms(None, 300), 300);
    assert_eq!(person_quiet_ms(Some(40), 9_000), 40);
    let mut r = Rig::new(Mode::On);
    r.f.person_quiet_ms = person_quiet_ms(None, 300);
    assert!(matches!(r.step(), Act::WaitUntil(_)));
    r.f.person_quiet_ms = person_quiet_ms(None, 1_500);
    assert_eq!(r.step(), Act::Hold);
}

#[test]
fn nudge_stops_at_one_try_per_epoch_and_three_per_hour() {
    let mut r = Rig::new(Mode::On);
    for epoch in 1..=3u64 {
        if epoch > 1 {
            r.now += COOLDOWN_MS;
            assert!(r.m.arm(Mode::On, Reason::Debt, epoch, r.now));
        }
        r.hold();
        r.at(ANSWER_TIMEOUT_MS);
        r.release();
        r.at(ANSWER_TIMEOUT_MS);
        assert_eq!(r.m.phase(), Phase::Rest);
        assert!(!r.m.arm(Mode::On, Reason::Debt, epoch, r.now));
    }
    r.now += COOLDOWN_MS;
    assert!(r.m.arm(Mode::On, Reason::Debt, 4, r.now));
    assert_eq!(r.step(), Act::GaveUp(GiveUp::Budget));
    r.now += HOUR_MS;
    assert!(r.m.arm(Mode::On, Reason::Debt, 5, r.now));
    assert_eq!(r.step(), Act::Hold);
}

#[test]
fn the_cooldown_spaces_two_attempts_by_a_minute() {
    let mut r = Rig::new(Mode::On);
    r.hold();
    r.at(ANSWER_TIMEOUT_MS);
    r.release();
    r.at(ANSWER_TIMEOUT_MS);
    assert!(r.m.arm(Mode::On, Reason::Debt, 2, r.now));
    assert!(matches!(r.step(), Act::WaitUntil(t) if t > r.now));
}

#[test]
fn nudge_skips_a_shared_session_and_a_dragged_window() {
    for shared in [true, false] {
        let mut r = Rig::new(Mode::On);
        r.f.shared = shared;
        r.f.dragging = !shared;
        assert_eq!(r.step(), Act::Nothing);
        assert_eq!(r.m.phase(), Phase::Armed);
    }
}

#[test]
fn the_release_waits_for_40ms_of_quiet() {
    let mut r = Rig::new(Mode::On);
    r.hold();
    r.answer();
    r.f.output_quiet_ms = 10;
    assert_eq!(r.step(), Act::WaitUntil(r.now + 30));
    r.f.output_quiet_ms = 40;
    assert_eq!(r.step(), Act::Release);
    let mut r = Rig::new(Mode::On);
    r.hold();
    r.answer();
    r.f.frame_open = true;
    assert!(matches!(r.step(), Act::WaitUntil(_)));
    r.f.frame_open = false;
    assert_eq!(r.step(), Act::Release);
}

#[test]
fn a_program_that_is_not_the_agent_class_is_never_held_whatever_the_mode() {
    for mode in [Mode::On, Mode::Log] {
        let mut r = Rig::new(mode);
        r.f.agent = false;
        assert_eq!(r.step(), Act::Dropped(Dropped::NotAgent));
        assert_eq!(r.m.nudged(), 0);
    }
}

#[test]
fn mode_log_writes_would_nudge_and_touches_no_pty() {
    let mut r = Rig::new(Mode::Log);
    assert_eq!(r.step(), Act::WouldNudge);
    assert_eq!(r.m.phase(), Phase::Rest);
    assert_eq!(r.m.nudged(), 0);
    assert!(!r.m.arm(Mode::Log, Reason::Debt, 1, r.now));
    assert!(r.m.arm(Mode::Log, Reason::Debt, 2, r.now));
}

#[test]
fn mode_off_runs_nothing() {
    let mut m = RepaintNudge::new();
    assert!(!m.arm(Mode::Off, Reason::Debt, 1, 0));
    assert_eq!(m.phase(), Phase::Idle);
    assert_eq!(m.step(&Facts::eligible(Mode::Off, SIZE), 0), Act::Nothing);
    let mut r = Rig::new(Mode::On);
    r.f.mode = Mode::Off;
    assert_eq!(r.step(), Act::Dropped(Dropped::Off));
}

#[test]
fn a_program_that_repainted_by_itself_is_not_nudged() {
    let mut r = Rig::new(Mode::On);
    r.f.owed = false;
    assert_eq!(r.step(), Act::Dropped(Dropped::NothingOwed));
    assert_eq!(r.m.nudged(), 0);
    let mut m = RepaintNudge::new();
    let mut f = Facts::eligible(Mode::On, SIZE);
    f.owed = false;
    assert!(m.arm(Mode::On, Reason::Adoption, u64::MAX, 0));
    assert_eq!(m.step(&f, 0), Act::Hold);
}

#[test]
fn every_exit_path_lifts_the_hold() {
    let mut r = Rig::new(Mode::On);
    r.hold();
    r.f.alt_screen = false;
    assert_eq!(r.at(1), Act::Release);
    let mut r = Rig::new(Mode::On);
    r.hold();
    r.f.frozen = true;
    assert_eq!(r.at(1), Act::Release);
    let mut r = Rig::new(Mode::On);
    r.hold();
    assert!(r.m.let_go(r.now));
    assert_eq!(r.m.phase(), Phase::Restoring);
    let mut r = Rig::new(Mode::On);
    assert!(!r.m.let_go(r.now));
}

#[test]
fn the_second_size_answered_without_the_first_is_a_repaint_not_a_heal() {
    let mut r = Rig::new(Mode::On);
    r.hold();
    r.at(ANSWER_TIMEOUT_MS);
    r.release();
    r.at(200);
    r.answer();
    assert_eq!(r.at(1), Act::Dropped(Dropped::Repainted));
}

#[test]
fn a_first_answer_with_no_second_is_judged_by_what_the_engine_owes() {
    for (reason, owed, end) in [
        (Reason::Debt, true, Act::GaveUp(GiveUp::NoAnswer)),
        (Reason::Debt, false, Act::Dropped(Dropped::NothingOwed)),
        (Reason::Adoption, false, Act::GaveUp(GiveUp::NoAnswer)),
    ] {
        let mut r = Rig::new(Mode::On);
        r.m = RepaintNudge::new();
        assert!(r.m.arm(Mode::On, reason, 1, r.now));
        r.hold();
        r.answer();
        assert_eq!(r.at(5), Act::Release);
        r.release();
        r.f.owed = owed;
        assert_eq!(r.at(ANSWER_TIMEOUT_MS), end, "{reason:?} owed={owed}");
    }
}

#[test]
fn a_screen_replacement_is_an_answer_too() {
    let mut r = Rig::new(Mode::On);
    r.hold();
    r.f.screen_replaced += 1;
    assert_eq!(r.at(1), Act::Release);
}

#[test]
fn the_quiet_wait_is_bounded() {
    let mut r = Rig::new(Mode::On);
    r.f.output_quiet_ms = 0;
    assert!(matches!(r.at(1), Act::WaitUntil(_)));
    assert_eq!(r.at(QUIET_MAX_MS), Act::Hold);
}
