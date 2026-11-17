// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WALL RUNG'S READ IN THE LOOP (FL-118b): a Claude Code session waiting at its account's usage
//! wall, holding the ladder's due rung on a build whose panel was measured over a wall, types
//! `/usage` (the rung's one read, its reason `at-wall`); a panel with room lifts the wall through
//! `accounts::note_room`, and every waiter resumes at its next ask. Elsewhere the gate stays shut
//! and the wall ends on its schedule.

use std::sync::atomic::{AtomicI64, Ordering};

use super::*;
use crate::harness::accounts::{self, Sample};
use crate::harness::provider_read::{self, InUse, ProbeEvent};

const WORK: &str = "work";

/// When the wall was observed: rung 0 stands 30 s before the loop's point.
const WALLED: i64 = PROBE_NOW - 90;

const HOLDER: &str = "s-hold";

const WAITERS: [&str; 3] = [HOLDER, "s-b", "s-c"];

const MEASURED: Option<&str> = Some("2.1.325");

/// A host over a real accounts state: the account walled at [`WALLED`], its sessions waiting on
/// its ladder, the loop's session ([`HOLDER`]) the longest; it keeps the ladder with the steps the
/// window's host takes (`harness_host::WorkerIdle::wall_ladder`, `wall_rung`, `wall_claim`:
/// `accounts::wall_ladder_step`, `AccountAdvice::or_wall_rung`, `accounts::wall_read_due`,
/// `accounts::claim_wall_read`) and records a sampled panel as it does
/// (`accounts::note_panel_read`, the read claimed at the claim's time).
#[derive(Debug)]
struct WallHost {
    state: Mutex<accounts::State>,
    /// The host's clock, unix seconds: [`PROBE_NOW`] unless a test moves it.
    now: AtomicI64,
    /// When the rung's read was claimed.
    claimed: AtomicI64,
    measured_on: Option<&'static str>,
    running: Option<&'static str>,
    said: Mutex<Vec<String>>,
    events: Mutex<Vec<accounts::Event>>,
}

impl WallHost {
    fn on(measured_on: Option<&'static str>, running: Option<&'static str>) -> Arc<Self> {
        let mut st = accounts::State::default();
        accounts::note_wall(
            &mut st,
            WORK,
            accounts::SESSION_LIMIT,
            Some(PROBE_NOW + 86_400),
            WALLED,
            WALLED,
        );
        for (k, sid) in (0i64..).zip(WAITERS) {
            accounts::note_wall_wait(&mut st, WORK, sid, WALLED + 10 * k);
        }
        Arc::new(Self {
            state: Mutex::new(st),
            now: AtomicI64::new(PROBE_NOW),
            claimed: AtomicI64::new(0),
            measured_on,
            running,
            said: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
        })
    }

    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }

    fn say(&self, word: String) {
        self.said.lock().unwrap().push(word);
    }

    fn said(&self) -> Vec<String> {
        self.said.lock().unwrap().clone()
    }

    /// What `sid` is advised at its wall now: a resume once a read lifted the wall (it leaves the
    /// ladder), else its next rung's ask on a measured build, else nothing (its wall's schedule).
    fn advice_of(&self, sid: &str) -> Option<AccountAdvice> {
        let now = self.now();
        let mut st = self.state.lock().unwrap();
        let advice = st
            .accounts
            .get(WORK)
            .and_then(|a| a.cleared_since(accounts::SESSION_LIMIT, WALLED, now))
            .map(|cleared_at| AccountAdvice::Resume { cleared_at });
        if self.measured_on.is_none() {
            return advice;
        }
        let leaves = advice.as_ref().is_some_and(AccountAdvice::leaves_wall);
        let Some(after) = accounts::wall_ladder_step(&mut st, WORK, sid, leaves, now) else {
            return advice;
        };
        let rung = Instant::now() + Duration::from_secs(u64::try_from(after).unwrap_or(0));
        AccountAdvice::or_wall_rung(advice, rung)
    }

    fn lifts(&self) -> Vec<accounts::Event> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.kind == accounts::Kind::Wall && e.why.contains(accounts::WALL_LIFTED))
            .cloned()
            .collect()
    }
}

impl IdleHost for WallHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn can_restart(&self) -> bool {
        true
    }
    fn account(&self, point: AccountPoint) -> Option<AccountAdvice> {
        let AccountPoint::Wall { .. } = point else {
            self.say("account:turn-end".to_string());
            return None;
        };
        let advice = self.advice_of(HOLDER);
        self.say(match &advice {
            Some(AccountAdvice::Resume { .. }) => "account:resume".to_string(),
            Some(_) => "account:rung".to_string(),
            None => "account:wall".to_string(),
        });
        advice
    }
    fn wall_read(&self, person: bool) -> bool {
        let now = self.now();
        let mut st = self.state.lock().unwrap();
        if person {
            accounts::note_wall_person(&mut st, WORK, HOLDER, now);
        }
        let due = accounts::wall_read_due(&st, WORK, HOLDER, self.measured_on, self.running, now);
        drop(st);
        self.say(format!("wall-read:{due}"));
        due
    }
    fn account_probe(&self) -> Result<provider_read::Use, ProbeNo> {
        let mut st = self.state.lock().unwrap();
        let now = self.now();
        let claimed = accounts::claim_wall_read(&mut st, WORK, HOLDER, now);
        drop(st);
        match claimed {
            Ok(claim) => {
                self.claimed.store(now, Ordering::SeqCst);
                self.say(format!("probe:rung-{}", claim.rung));
                Ok(claim.read_use())
            }
            Err(no) => {
                self.say(format!("probe:{}", no.word()));
                Err(ProbeNo::NotDue)
            }
        }
    }
    fn account_probe_typed(&self, use_token: provider_read::Use) {
        let begun = use_token.begin(accounts::panel_account(WORK), self.now(), None);
        assert!(begun.is_ok(), "the rung's use reads its own account");
    }
    fn account_probed(&self, read: Result<&Sample, ProbeMiss>) {
        let Ok(sample) = read else {
            self.say(format!("probed:{}", read.err().map_or("", ProbeMiss::word)));
            return;
        };
        self.say("probed:ok".to_string());
        let claimed = self.claimed.load(Ordering::SeqCst);
        let now = self.now();
        let mut st = self.state.lock().unwrap();
        let events = accounts::note_panel_read(&mut st, WORK, sample, claimed, now);
        drop(st);
        self.events.lock().unwrap().extend(events);
    }
}

/// The settled Usage tab with `from` drawn as `to`.
fn panel_with(from: &str, to: &str) -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(panel_fx::USAGE_SETTLED)
        .into_iter()
        .map(|row| row.replace(from, to))
        .collect()
}

/// What one wall point did.
struct AtWall {
    decided: TurnEndAction,
    /// The decided wait's length from its decision.
    waits: Option<Duration>,
    probed: Probed,
    /// The provider reads the probe and the point decided again made.
    reads: Vec<ProbeEvent>,
    said: Vec<String>,
    writes: Vec<String>,
}

/// The loop's session at its wall point under `host`: the point decided, the probe run there, and
/// the point decided again, as the loop does after a probe; `frames` is the Usage tab a `/usage`
/// opens; `setup` shapes the session before the point is read.
fn at_wall(
    tag: &str,
    host: Arc<dyn IdleHost>,
    frames: Vec<Vec<String>>,
    setup: impl FnOnce(&mut Session<'_, Mock>),
) -> AtWall {
    at_wall_then(tag, host, frames, setup, None, || {})
}

/// [`at_wall`], with `between` run after the probe and before the point is decided again; `seq`
/// holds the wall screen at that seq at every read, as an idle Claude Code wall draws nothing new.
fn at_wall_then(
    tag: &str,
    host: Arc<dyn IdleHost>,
    frames: Vec<Vec<String>>,
    setup: impl FnOnce(&mut Session<'_, Mock>),
    seq: Option<u64>,
    between: impl FnOnce(),
) -> AtWall {
    let (dir, ledger) = ledger_at(tag);
    let world = ProbeHost::scripted(&[], &[]);
    // The session's wall resets two hours past the point (9:20 PM): only a lift resumes it now.
    let wall: Vec<String> = limited_screen()
        .into_iter()
        .map(|row| row.replace("resets 7:30pm", "resets 11:30pm"))
        .collect();
    let mut m = probe_mock(vec![wall], &[panel_fx::USAGE_SETTLED], &world);
    m.panel.as_mut().expect("a panel world").frames = frames;
    if let Some(seq) = seq {
        m.screen_gens.insert(0, (1, seq));
    }
    let opts = SuperviseOpts {
        idle_host: Some(host),
        ..hosted(30)
    };
    let (decided, waits, probed, reads, said) = {
        let mut s = session(&mut m, None);
        s.set_clock(PROBE_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.set_approval_ledger(Some(ledger.clone()));
        s.ledger = crate::supervise::approvals::Ledger::open(Some(&ledger), None, &mut Vec::new());
        let ms = Duration::from_millis;
        s.set_probe_waits(ms(60), ms(200), ms(10), ms(100));
        setup(&mut s);
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        assert!(
            matches!(point.phase, Phase::Limited { .. }),
            "{tag}: a wall"
        );
        let decided = s
            .turn_end_at_point(&point, Some(Duration::from_secs(182)), &opts)
            .unwrap_or_else(|e| panic!("{tag}: {e:?}"));
        let waits = match &decided {
            TurnEndAction::WaitUntil { until, .. } => {
                Some(until.saturating_duration_since(s.fx.mono()))
            }
            _ => None,
        };
        let mut heard = Heard::default();
        let (probed, mut reads) =
            provider_read::captured(|| s.usage_probe(&point, &decided, &opts, &mut heard));
        let probed = probed.unwrap_or_else(|e| panic!("{tag}: the probe: {e:?}"));
        between();
        let (again, more) =
            provider_read::captured(|| s.turn_end_now(&point, &opts, &[], &mut heard));
        again.unwrap_or_else(|e| panic!("{tag}: decided again: {e:?}"));
        reads.extend(more);
        (decided, waits, probed, reads, heard.said)
    };
    let writes = m.writes();
    let _ = std::fs::remove_dir_all(&dir);
    AtWall {
        decided,
        waits,
        probed,
        reads,
        said,
        writes,
    }
}

fn at_wall_reads() -> Vec<ProbeEvent> {
    vec![ProbeEvent {
        at: PROBE_NOW,
        read: provider_read::Read::ClaudePanel,
        account: Some(WORK.into()),
        login: None,
        why: InUse::AtWall,
    }]
}

#[test]
fn a_panel_with_room_lifts_the_wall_within_its_rung_and_resumes_every_waiter() {
    let host = WallHost::on(MEASURED, MEASURED);
    let seen = at_wall(
        "wall-rung-room",
        host.clone(),
        vec![aterm_phase::prompt::fixtures::screen(
            panel_fx::USAGE_SETTLED,
        )],
        |_| {},
    );
    let log = format!("{:#?}\n{:#?}\n{:#?}", host.said(), seen.said, seen.writes);
    assert_eq!(seen.probed, Probed::Done, "{log}");
    assert_eq!(
        seen.reads,
        at_wall_reads(),
        "the rung's one read, its reason at-wall: {log}"
    );
    assert_eq!(
        seen.writes.first().map(String::as_str),
        Some("text:/usage"),
        "{log}"
    );
    assert_eq!(
        seen.writes.get(1).map(String::as_str),
        Some("enter"),
        "{log}"
    );
    assert_eq!(seen.writes.get(2).map(String::as_str), Some("esc"), "{log}");
    assert!(
        seen.writes.iter().any(|w| w.contains(CLEARED)),
        "the holder resumes at its point decided again: {log}"
    );
    let said = host.said();
    assert!(
        said.contains(&"probe:rung-0".to_string()) && said.contains(&"account:resume".to_string()),
        "{log}"
    );
    let lifts = host.lifts();
    assert_eq!(lifts.len(), 1, "one Wall row lifts it: {lifts:?}");
    let st = host.state.lock().unwrap().clone();
    let rung_next = accounts::wall_rung_at(WALLED, 1);
    assert!(
        lifts[0].t < rung_next,
        "lifted within its rung, before the next stands: {lifts:?}"
    );
    drop(st);
    for sid in WAITERS {
        assert!(
            matches!(host.advice_of(sid), Some(AccountAdvice::Resume { .. })),
            "{sid} resumes at its next ask"
        );
    }
    assert!(
        host.state.lock().unwrap().wall_reads.is_empty(),
        "nothing waits: the ladder stopped"
    );

    // A PANEL THAT LIFTS NOTHING CLIMBS THE RUNG: refused (the lab's jail), unsettled, or full.
    for (case, frame) in [
        (
            "refused",
            panel_with("Scanning local sessions…", "Could not refresh usage data"),
        ),
        (
            "unsettled",
            aterm_phase::prompt::fixtures::screen(panel_fx::USAGE_REFRESHING),
        ),
        ("full", panel_with("  1% used", "100% used")),
    ] {
        let host = WallHost::on(MEASURED, MEASURED);
        let seen = at_wall(
            &format!("wall-rung-{case}"),
            host.clone(),
            vec![frame],
            |_| {},
        );
        let log = format!("{case}: {:#?}\n{:#?}", host.said(), seen.writes);
        assert_eq!(seen.reads, at_wall_reads(), "{log}");
        assert!(host.lifts().is_empty(), "{log}");
        let st = host.state.lock().unwrap();
        assert!(
            !st.accounts[WORK].walls[accounts::SESSION_LIMIT].cleared,
            "the wall stands: {log}"
        );
        assert_eq!(
            st.wall_reads[WORK].read,
            Some(0),
            "the rung is climbed: {log}"
        );
        assert!(
            !accounts::wall_read_due(&st, WORK, HOLDER, MEASURED, MEASURED, PROBE_NOW),
            "{log}"
        );
        drop(st);
        assert!(!seen.writes.iter().any(|w| w.contains(CLEARED)), "{log}");
        assert!(
            !host.said().contains(&"account:resume".to_string()),
            "no waiter resumes: {log}"
        );
    }
}

#[test]
fn the_wall_probe_is_shut_where_the_panel_is_unmeasured() {
    let settled = || {
        vec![aterm_phase::prompt::fixtures::screen(
            panel_fx::USAGE_SETTLED,
        )]
    };
    let host = WallHost::on(None, MEASURED);
    let shut = at_wall("wall-unmeasured", host.clone(), settled(), |_| {});
    let log = format!("{:#?}\n{:#?}", host.said(), shut.writes);
    assert_eq!(shut.probed, Probed::No, "{log}");
    assert!(shut.reads.is_empty(), "no read is made: {log}");
    assert!(
        !shut.writes.iter().any(|w| w.contains("/usage")),
        "nothing typed: {log}"
    );
    assert!(host.said().iter().all(|w| w != "account:resume"), "{log}");
    assert!(
        !host.state.lock().unwrap().accounts[WORK].walls[accounts::SESSION_LIMIT].cleared,
        "{log}"
    );

    // The wall ends on its schedule: the wait a host with no wall door decides at that wall.
    let no_door: Arc<dyn IdleHost> = ProbeHost::scripted(&[], &[]);
    let schedule = at_wall("wall-no-door", no_door, settled(), |_| {});
    assert!(
        matches!(shut.decided, TurnEndAction::WaitUntil { .. }),
        "{:?}",
        shut.decided
    );
    let (a, b) = (shut.waits.expect("a wait"), schedule.waits.expect("a wait"));
    assert!(
        a.abs_diff(b) < Duration::from_secs(5) && a > Duration::from_secs(3600),
        "the wall's own reset: {a:?} vs {b:?}"
    );

    // A build other than the one measured keeps it shut too.
    let host = WallHost::on(MEASURED, Some("2.1.324"));
    let other = at_wall("wall-other-build", host.clone(), settled(), |_| {});
    assert!(other.reads.is_empty() && other.probed == Probed::No);

    // A PERSON TYPING IN THE WAITING TAB STARTS THE LADDER AGAIN: no read inside their grace,
    // and the first rung counts from their typing.
    let host = WallHost::on(MEASURED, MEASURED);
    let person = at_wall("wall-person", host.clone(), settled(), |s| {
        s.ctl.human_ms = Some(5_000);
        s.ctl.typed_ms = Some(5_000);
    });
    let log = format!("{:#?}\n{:#?}", host.said(), person.writes);
    assert!(person.reads.is_empty(), "{log}");
    assert!(!person.writes.iter().any(|w| w.contains("/usage")), "{log}");
    let st = host.state.lock().unwrap();
    let ladder = &st.wall_reads[WORK];
    assert_eq!(
        (ladder.since, ladder.read),
        (PROBE_NOW, None),
        "the ladder starts again from the person's typing: {log}"
    );
    drop(st);

    // A scroll or a click in the waiting tab is no typing (the window's last key an hour back):
    // the ladder keeps its rungs.
    let host = WallHost::on(MEASURED, MEASURED);
    let scrolled = at_wall("wall-scroll", host.clone(), settled(), |s| {
        s.ctl.human_ms = Some(5_000);
        s.ctl.typed_ms = Some(3_600_000);
    });
    let log = format!("{:#?}\n{:#?}", host.said(), scrolled.writes);
    let st = host.state.lock().unwrap();
    assert_eq!(
        st.wall_reads.get(WORK).map(|l| l.since),
        Some(WALLED),
        "a scroll restarts nothing: {log}"
    );
}

/// A Claude Code wall screen draws nothing new while it waits: the rung that comes due on that
/// same screen (its seq held) is asked for and claimed, and its `/usage` typed, when the rung's ask
/// decides the point again. (The held seq keeps the mock from drawing what is typed, so the read's
/// panel is the other tests'.)
#[test]
fn a_wall_rung_due_on_an_unchanged_screen_is_read_at_its_ask() {
    let host = WallHost::on(MEASURED, MEASURED);
    // The point is first decided before rung 0 stands.
    host.now.store(WALLED + 30, Ordering::SeqCst);
    let rung_at = accounts::wall_rung_at(WALLED, 0);
    let moved = host.clone();
    let seen = at_wall_then(
        "wall-rung-later",
        host.clone(),
        vec![aterm_phase::prompt::fixtures::screen(
            panel_fx::USAGE_SETTLED,
        )],
        |_| {},
        Some(101),
        move || moved.now.store(rung_at, Ordering::SeqCst),
    );
    let log = format!("{:#?}\n{:#?}\n{:#?}", host.said(), seen.said, seen.writes);
    assert_eq!(
        seen.probed,
        Probed::No,
        "nothing due at the first ask: {log}"
    );
    let said = host.said();
    let asked: Vec<&str> = said
        .iter()
        .map(String::as_str)
        .filter(|w| w.starts_with("wall-read:"))
        .collect();
    assert_eq!(
        asked,
        ["wall-read:false", "wall-read:true"],
        "the door is asked again at the rung's ask, on the same screen: {log}"
    );
    assert!(said.contains(&"probe:rung-0".to_string()), "{log}");
    assert_eq!(
        seen.writes.first().map(String::as_str),
        Some("text:/usage"),
        "{log}"
    );
    assert_eq!(
        host.state.lock().unwrap().wall_reads[WORK].read,
        Some(0),
        "{log}"
    );
}
