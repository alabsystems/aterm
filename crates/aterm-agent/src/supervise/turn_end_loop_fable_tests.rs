// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Fable limit's move onto Opus and its return, driven through the supervision loop.

use super::*;
use crate::supervise::approvals::{
    ACCOUNT_MOVE_COMMAND, Ledger, OpenSwitch, Outcome, Row, account_switch_reason,
    model_switch_reason, open_model_switch, open_wall,
};
use crate::supervise::policy::turn_end::{
    AccountAt, FABLE_FALLBACK, FableWeek, LaunchModel, RULE_ACCOUNT, RULE_CONTINUE,
    RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE, Restart, RestartAnswer, RestartAsk, RestartClaim,
    decide_turn_end,
};
use aterm_phase::prompt::fixtures as fx;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

const SID: &str = "@s-1";

/// 23:36 PDT on 2026-10-15, the minute the measured Fable wall's done rows name.
const NOW: i64 = 1_792_132_560;

/// The reset the measured Fable wall names (`resets 1:36am (America/Los_Angeles)`), two hours on.
const RESET: i64 = NOW + 2 * 3600;

fn ended(said: &str) -> Vec<String> {
    let mut r = rows(&[
        &format!("⏺ {said}"),
        "",
        "✻ Worked for 12m 2s · done 4:24 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

fn ledger_at(tag: &str) -> (PathBuf, PathBuf) {
    let dir = crate::supervise::test_scratch_path("te-fable-ledger", tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("s-1.jsonl");
    (dir, path)
}

fn point_at(s: &Session<'_, Mock>, rows: Vec<String>, seq: u64) -> Turn {
    s.turn_of(Screen {
        rows,
        seq,
        ..Screen::default()
    })
}

/// The measured Fable wall (Claude Code 2.1.301), its notice naming a reset.
fn fable_wall() -> Vec<String> {
    fx::screen(fx::WALL_FABLE_RESET_2_1_301)
}

/// [`fable_wall`] with every `Fable` the screen says spelled `model`: another model's own limit.
fn bucket_wall(model: &str) -> Vec<String> {
    fable_wall()
        .into_iter()
        .map(|r| r.replace("Fable", model))
        .collect()
}

/// What a step of the loop said (printed, journaled, and in a host's log) and what it only
/// noted (the journal alone).
#[derive(Default)]
struct Heard {
    said: Vec<String>,
    noted: Vec<String>,
}

impl Review for Heard {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, _turn: &Turn) -> Result<bool, String> {
        Ok(true)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        crate::supervise::ledger::assert_marked(line);
        self.said.push(line.to_string());
        Ok(())
    }
    fn note(&mut self, line: &str) {
        crate::supervise::ledger::assert_marked(line);
        self.noted.push(line.to_string());
    }
    fn unattended(&self) -> bool {
        true
    }
}

/// A window's host as the Fable move meets it: what the agent's launch says of its model
/// ([`IdleHost::launch_model`]), the relaunches it is asked for and its scripted answers, the
/// agent's transcript, and the agent process it names.
#[derive(Debug, Default)]
struct FableHost {
    /// [`IdleHost::launch_model`], as a test sets it, and how often the loop asked it.
    launch: Mutex<LaunchModel>,
    launch_reads: AtomicUsize,
    /// `[harness] relaunch = false`: no restart can be asked.
    off: AtomicBool,
    /// Each relaunch answer in turn, the last for ever; none: the host answers nothing.
    answers: Mutex<VecDeque<&'static str>>,
    answered: Mutex<Option<&'static str>>,
    /// What each answer read of the conversation's record, beside [`Self::answers`] (the last
    /// for ever; none: [`RestartClaim::Unread`]).
    claims: Mutex<VecDeque<RestartClaim>>,
    claimed: Mutex<Option<RestartClaim>>,
    /// Each restart asked ([`IdleHost::restart`]), and the ask it came as, in order.
    asked: Mutex<Vec<Restart>>,
    asks: Mutex<Vec<RestartAsk>>,
    /// The agent's transcript before the first restart asked and after it.
    tails: Mutex<Option<(String, String)>>,
    tail_asks: AtomicUsize,
    /// The agent process: pid, start (unix seconds) and conversation.
    pid: Mutex<Option<u32>>,
    started: Mutex<Option<i64>>,
    conversation: Mutex<Option<String>>,
    /// The process the host reads only once the point asks whether the agent resumes
    /// ([`IdleHost::agent_resumed`]), as a window's host refreshes the snapshot it forgot at the
    /// last agent's exit.
    late_agent: Mutex<Option<(u32, i64, Option<&'static str>)>>,
    relaunch_owed: AtomicBool,
    /// A step of the host's own is owed at the next idle point ([`IdleHost::at_idle`]), once,
    /// answering [`Self::step`] (`None`: `done:resumed`); how many were taken.
    carry: AtomicBool,
    step: Mutex<Option<&'static str>>,
    carried: AtomicUsize,
    /// The account's record of Fable's week ([`IdleHost::fable_week`]), and how often it was read.
    fable_week: Mutex<Option<FableWeek>>,
    week_reads: AtomicUsize,
}

impl FableHost {
    fn launched(launch: LaunchModel, answers: &[&'static str]) -> Arc<Self> {
        Arc::new(Self {
            launch: Mutex::new(launch),
            answers: Mutex::new(answers.iter().copied().collect()),
            ..Self::default()
        })
    }

    fn claiming(
        launch: LaunchModel,
        answers: &[&'static str],
        claims: &[RestartClaim],
    ) -> Arc<Self> {
        let host = Self::launched(launch, answers);
        *host.claims.lock().unwrap() = claims.iter().copied().collect();
        host
    }

    fn asked(&self) -> Vec<Restart> {
        self.asked.lock().unwrap().clone()
    }

    fn words(&self) -> Vec<&'static str> {
        self.asked().iter().map(Restart::word).collect()
    }
}

impl IdleHost for FableHost {
    fn wants(&self) -> bool {
        self.carry.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        if !self.carry.swap(false, Ordering::SeqCst) {
            return None;
        }
        self.carried.fetch_add(1, Ordering::SeqCst);
        let step = self.step.lock().unwrap().unwrap_or("done:resumed");
        Some(HostStep {
            line: format!("carry-on step={step}"),
            moved: crate::harness::upgrade_drive::moved(step),
            typed: crate::harness::upgrade_drive::host_turn(step),
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn can_restart(&self) -> bool {
        !self.off.load(Ordering::SeqCst)
    }
    fn relaunch_off(&self) -> bool {
        self.off.load(Ordering::SeqCst)
    }
    fn launch_model(&self) -> LaunchModel {
        self.launch_reads.fetch_add(1, Ordering::SeqCst);
        self.launch.lock().unwrap().clone()
    }
    fn fable_week(&self) -> Option<FableWeek> {
        self.week_reads.fetch_add(1, Ordering::SeqCst);
        *self.fable_week.lock().unwrap()
    }
    fn restart(&self, why: &Restart, ask: &RestartAsk) -> Option<RestartAnswer> {
        self.asked.lock().unwrap().push(why.clone());
        self.asks.lock().unwrap().push(ask.clone());
        let mut last = self.answered.lock().unwrap();
        if let Some(next) = self.answers.lock().unwrap().pop_front() {
            *last = Some(next);
        }
        let mut claimed = self.claimed.lock().unwrap();
        if let Some(next) = self.claims.lock().unwrap().pop_front() {
            *claimed = Some(next);
        }
        last.map(|step| RestartAnswer {
            step: step.to_string(),
            claim: claimed.unwrap_or(RestartClaim::Unread),
        })
    }
    fn transcript_tail(&self) -> Option<String> {
        self.tail_asks.fetch_add(1, Ordering::SeqCst);
        let (before, after) = self.tails.lock().unwrap().clone()?;
        Some(if self.asked.lock().unwrap().is_empty() {
            before
        } else {
            after
        })
    }
    fn agent_pid(&self) -> Option<u32> {
        *self.pid.lock().unwrap()
    }
    fn agent_started(&self) -> Option<i64> {
        *self.started.lock().unwrap()
    }
    fn agent_conversation(&self) -> Option<String> {
        self.conversation.lock().unwrap().clone()
    }
    fn relaunch_owed(&self) -> bool {
        self.relaunch_owed.load(Ordering::SeqCst)
    }
    fn agent_resumed(&self) -> Option<AgentResumed> {
        let (pid, started, conversation) = self.late_agent.lock().unwrap().take()?;
        *self.pid.lock().unwrap() = Some(pid);
        *self.started.lock().unwrap() = Some(started);
        *self.conversation.lock().unwrap() = conversation.map(str::to_string);
        Some(AgentResumed {
            pid,
            started: Some(started),
            resumed: false,
        })
    }
}

fn opts_for(host: &Arc<FableHost>) -> SuperviseOpts {
    SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..SuperviseOpts::hosted_with(&owners_policy())
    }
}

/// A loop on the session at `now`, its ledger at `ledger`, deciding at once.
fn loop_at<'m>(m: &'m mut Mock, ledger: &std::path::Path, now: i64) -> Session<'m, Mock> {
    let mut s = session(m, Some(SID.to_string()));
    s.set_zone_lookup(test_zones);
    s.set_clock(now, PDT);
    s.ledger = Ledger::open(Some(ledger), Some(SID), &mut Vec::new());
    s.set_turn_end_timing(TurnEndTiming {
        take_within: Duration::ZERO,
        ..TurnEndTiming::default()
    });
    s
}

/// A later loop on the session: seeded from the ledger an earlier one left.
fn later_loop<'m>(m: &'m mut Mock, ledger: &std::path::Path, now: i64) -> Session<'m, Mock> {
    let mut s = loop_at(m, ledger, now);
    s.seed_model_switch(ledger);
    s.seed_wall_memory(open_wall(ledger, Some(SID)));
    s
}

fn moves_to_opus(a: &TurnEndAction) -> bool {
    matches!(
        a,
        TurnEndAction::Restart {
            why: Restart::Model { to },
            rule_id: RULE_MODEL_FALLBACK,
            ..
        } if to == FABLE_FALLBACK
    )
}

fn moves_model(a: &TurnEndAction) -> bool {
    matches!(
        a,
        TurnEndAction::Restart {
            why: Restart::Model { .. },
            ..
        }
    )
}

fn returns(a: &TurnEndAction) -> Option<Option<String>> {
    match a {
        TurnEndAction::Restart {
            why: Restart::ModelBack { to },
            rule_id: RULE_MODEL_RESTORE,
            ..
        } => Some(to.clone()),
        _ => None,
    }
}

/// The ledger's rows of `rule` the loop typed: (command, reason).
fn typed_rows(ledger: &std::path::Path, rule: &str) -> Vec<(String, String)> {
    std::fs::read_to_string(ledger)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let v: aterm_json::Value = aterm_json::from_str(l).ok()?;
            let text = |k: &str| {
                v.get(k)
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            (text("rule_id") == rule && text("decision") == "typed")
                .then(|| (text("command"), text("reason")))
        })
        .collect()
}

/// Seeds the ledger an earlier loop left at `path`: the fallback's relaunch onto Opus, from Fable,
/// its launch `launch`, back at `back_at_unix`, then a turn the loop asked on it, and (`restored`)
/// the return. `legacy` writes it as a build before the 2026-11-09 removal did: no `launch=` in its
/// reason. Returns the second the switch was made.
fn seed_switch(
    path: &std::path::Path,
    back_at_unix: Option<i64>,
    launch: Option<&str>,
    restored: bool,
    legacy: bool,
) -> i64 {
    seed_switch_rows(path, NOW, back_at_unix, launch, restored, legacy)
}

/// [`seed_switch`] as this build writes it, its rows dated before `now_unix`.
fn seed_switch_at(
    path: &std::path::Path,
    now_unix: i64,
    back_at_unix: Option<i64>,
    launch: Option<&str>,
    restored: bool,
) -> i64 {
    seed_switch_rows(path, now_unix, back_at_unix, launch, restored, false)
}

fn seed_switch_rows(
    path: &std::path::Path,
    now_unix: i64,
    back_at_unix: Option<i64>,
    launch: Option<&str>,
    restored: bool,
    legacy: bool,
) -> i64 {
    let now = now_unix * 1000;
    let (written, made) = (now - 30 * 60_000, (now - 31 * 60_000).div_euclid(1000));
    let mut reason = model_switch_reason(Some("fable"), launch, back_at_unix, made);
    if legacy {
        reason = reason.replace(&format!(" launch={}", launch.unwrap_or("-")), "");
    }
    let command = format!(
        "relaunch --model {}",
        if legacy { "opus" } else { FABLE_FALLBACK }
    );
    let mut rows = vec![
        Row {
            rule_id: RULE_MODEL_FALLBACK,
            outcome: Outcome::Typed,
            command: &command,
            reason: &reason,
            box_seq: 90,
        }
        .to_json(written, Some(SID)),
        Row {
            rule_id: RULE_CONTINUE,
            outcome: Outcome::Typed,
            command: "keep going",
            reason: "the turn-end policy",
            box_seq: 91,
        }
        .to_json(now - 29 * 60_000, Some(SID)),
    ];
    if restored {
        rows.push(
            Row {
                rule_id: RULE_MODEL_RESTORE,
                outcome: Outcome::Typed,
                command: "relaunch with no --model, as its launch",
                reason: "the turn-end policy",
                box_seq: 95,
            }
            .to_json(now - 10 * 60_000, Some(SID)),
        );
    }
    std::fs::write(path, rows.join("\n") + "\n").expect("the ledger");
    made
}

/// THE FABLE LIMIT MOVES THE SESSION ONTO OPUS, ON THE SAME ACCOUNT, AND A LATER LOOP TAKES IT BACK
/// AT THE RESET: a Claude Code session whose launch names no model, at Fable's own limit, is
/// relaunched with `--model claude-opus-5-5`; the loop says so (`RESTARTED … relaunch --model
/// claude-opus-5-5 (from fable, back at …; session-only)`) and writes the switch to the ledger
/// with its launch's model (none), the notice's reset and the second it was made. A loop that
/// starts after the reset reads the switch back from the ledger and relaunches the session as its
/// launch had it (no `--model`), writing the return; one that starts before the reset does not.
/// NEGATIVE CONTROLS: the ledger after the return opens no switch, and a later loop before the
/// reset returns nothing.
#[test]
fn a_fable_limit_moves_onto_opus_and_a_later_loop_takes_it_back_at_the_reset() {
    let (dir, ledger) = ledger_at("forward-and-back");
    let host = FableHost::launched(LaunchModel::Unnamed, &["relaunched"]);
    let opts = opts_for(&host);
    let mut m = Mock::new(vec![fable_wall()]);
    let mut s = loop_at(&mut m, &ledger, NOW);
    let p = point_at(&s, fable_wall(), 100);
    let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
    assert!(moves_to_opus(&a), "{a:?}");
    assert!(
        host.launch_reads.load(Ordering::SeqCst) >= 1,
        "the launch's model was read at the Fable limit"
    );
    let mut heard = Heard::default();
    s.turn_end_execute(&p, a, &opts, &[], &mut heard)
        .expect("relaunched");
    drop(s);
    assert_eq!(
        host.asked(),
        [Restart::Model {
            to: FABLE_FALLBACK.to_string()
        }]
    );
    assert!(
        heard
            .said
            .iter()
            .any(|l| l.starts_with("RESTARTED seq=100 ")
                && l.contains(
                    "rule=model-fallback@v1 relaunch --model claude-opus-5-5 (from fable, back at "
                )
                && l.ends_with("; session-only)")),
        "{:#?}",
        heard.said
    );
    assert_eq!(
        typed_rows(&ledger, RULE_MODEL_FALLBACK),
        [(
            "relaunch --model claude-opus-5-5".to_string(),
            model_switch_reason(Some("fable"), None, Some(RESET), NOW)
        )]
    );
    assert!(
        typed_rows(&ledger, RULE_MODEL_FALLBACK)[0]
            .1
            .contains(&format!("from=fable launch=- back_at={RESET} made={NOW}")),
        "the reason spells the switch's fields"
    );
    assert_eq!(
        open_model_switch(&ledger, Some(SID)),
        Some(OpenSwitch {
            from: Some("fable".to_string()),
            to: FABLE_FALLBACK.to_string(),
            launch: None,
            back_at_unix: Some(RESET),
            asked: false,
            made_unix: NOW,
            proc: None,
        })
    );

    // A later loop before the reset keeps the session on Opus.
    let mut m = Mock::new(vec![ended("Fixed the parser.")]);
    let mut s = later_loop(&mut m, &ledger, RESET - 600);
    let host2 = FableHost::launched(LaunchModel::Unnamed, &["relaunched"]);
    let opts2 = opts_for(&host2);
    let p = point_at(&s, ended("Fixed the parser."), 101);
    let a = s.turn_end_at_point(&p, None, &opts2).expect("a decision");
    assert_eq!(returns(&a), None, "before the reset: {a:?}");
    assert!(
        s.turn_end.model_switch().is_some(),
        "the switch stands for the reset"
    );
    drop(s);

    // A later loop past the reset relaunches the session as its launch had it.
    let mut m = Mock::new(vec![ended("Fixed the parser.")]);
    let mut s = later_loop(&mut m, &ledger, RESET + 600);
    let host3 = FableHost::launched(LaunchModel::Unnamed, &["relaunched"]);
    let opts3 = opts_for(&host3);
    let p = point_at(&s, ended("Fixed the parser."), 102);
    let a = s.turn_end_at_point(&p, None, &opts3).expect("a decision");
    assert_eq!(returns(&a), Some(None), "past the reset: {a:?}");
    let mut heard = Heard::default();
    s.pass_wake_settle();
    s.turn_end_execute(&p, a, &opts3, &[], &mut heard)
        .expect("returned");
    drop(s);
    assert_eq!(host3.asked(), [Restart::ModelBack { to: None }]);
    assert!(
        heard
            .said
            .iter()
            .any(|l| l.starts_with("RESTARTED seq=102 ")
                && l.ends_with("rule=model-restore@v1 relaunch with no --model, as its launch")),
        "{:#?}",
        heard.said
    );
    assert_eq!(
        typed_rows(&ledger, RULE_MODEL_RESTORE)
            .into_iter()
            .map(|(c, _)| c)
            .collect::<Vec<_>>(),
        ["relaunch with no --model, as its launch"]
    );
    assert_eq!(
        open_model_switch(&ledger, Some(SID)),
        None,
        "the return closed the switch"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// ONLY FABLE'S OWN LIMIT, IN A CLAUDE CODE SESSION WHOSE LAUNCH NAMES NO MODEL OR NAMES FABLE,
/// MOVES: the launch's model is the host's read of the agent's argv
/// ([`IdleHost::launch_model`]). A launch naming Fable moves, and its switch keeps that `--model`
/// for the return. NEGATIVE CONTROLS: a launch naming another model, a launch the host did not
/// read, Sonnet's or Opus's own limit, and a host that cannot relaunch each wait the limit out
/// with no restart asked; an ordinary turn end never asks the launch.
#[test]
fn only_fables_own_limit_under_a_launch_that_admits_it_moves_onto_opus() {
    let mut wrong = Vec::new();
    for (case, launch, screen, off, moves) in [
        (
            "no --model",
            LaunchModel::Unnamed,
            fable_wall(),
            false,
            true,
        ),
        (
            "--model fable",
            LaunchModel::Named("fable".to_string()),
            fable_wall(),
            false,
            true,
        ),
        (
            "--model claude-fable-5-0",
            LaunchModel::Named("claude-fable-5-0".to_string()),
            fable_wall(),
            false,
            true,
        ),
        (
            "control: --model claude-sonnet-4-6",
            LaunchModel::Named("claude-sonnet-4-6".to_string()),
            fable_wall(),
            false,
            false,
        ),
        (
            "control: a launch the host did not read",
            LaunchModel::Unread,
            fable_wall(),
            false,
            false,
        ),
        (
            "control: Sonnet's own limit",
            LaunchModel::Unnamed,
            bucket_wall("Sonnet"),
            false,
            false,
        ),
        (
            "control: Opus's own limit",
            LaunchModel::Unnamed,
            bucket_wall("Opus"),
            false,
            false,
        ),
        (
            "control: relaunch off",
            LaunchModel::Unnamed,
            fable_wall(),
            true,
            false,
        ),
    ] {
        let (dir, ledger) = ledger_at(&format!("launch-{}", case.replace([' ', ':', '\''], "-")));
        let host = FableHost::launched(launch.clone(), &["relaunched"]);
        host.off.store(off, Ordering::SeqCst);
        let opts = opts_for(&host);
        let mut m = Mock::new(vec![screen.clone()]);
        let mut s = loop_at(&mut m, &ledger, NOW);
        let p = point_at(&s, screen.clone(), 100);
        let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
        let got = moves_to_opus(&a);
        let waits = matches!(a, TurnEndAction::WaitUntil { .. });
        if got {
            s.turn_end_execute(&p, a.clone(), &opts, &[], &mut Alone)
                .expect("relaunched");
        }
        let switch = s.turn_end.model_switch().cloned();
        drop(s);
        let open = open_model_switch(&ledger, Some(SID));
        let _ = std::fs::remove_dir_all(&dir);
        if got != moves || (!moves && (moves_model(&a) || !waits || !host.asked().is_empty())) {
            wrong.push(format!("{case}: {a:?}, asked {:?}", host.asked()));
            continue;
        }
        if moves {
            let want = launch.back_to();
            if switch.as_ref().map(|m| m.launch.clone()) != Some(want.clone())
                || open.as_ref().map(|o| o.launch.clone()) != Some(want)
            {
                wrong.push(format!("{case}: switch {switch:?}, ledger {open:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");

    let host = FableHost::launched(LaunchModel::Unnamed, &["relaunched"]);
    let opts = opts_for(&host);
    let (dir, ledger) = ledger_at("launch-unasked");
    let mut m = Mock::new(vec![ended("Fixed the parser.")]);
    let mut s = loop_at(&mut m, &ledger, NOW);
    let p = point_at(&s, ended("Fixed the parser."), 100);
    let _ = s.turn_end_at_point(&p, None, &opts).expect("a decision");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        host.launch_reads.load(Ordering::SeqCst),
        0,
        "no limit, no read of the launch"
    );
}

/// THE RETURN PUTS BACK THE LAUNCH'S OWN `--model`: a switch made under `--model fable` returns
/// with `relaunch --model fable`, one made under a launch that named none with no `--model`.
#[test]
fn the_return_puts_back_the_launchs_own_model() {
    for (launch, line) in [
        (Some("fable"), "relaunch --model fable"),
        (None, "relaunch with no --model, as its launch"),
    ] {
        let (dir, ledger) = ledger_at(&format!("return-{}", launch.unwrap_or("none")));
        let back_at = NOW - 60;
        seed_switch(&ledger, Some(back_at), launch, false, false);
        let host = FableHost::launched(LaunchModel::Unnamed, &["relaunched"]);
        let opts = opts_for(&host);
        let mut m = Mock::new(vec![ended("Fixed the parser.")]);
        let mut s = later_loop(&mut m, &ledger, NOW);
        let p = point_at(&s, ended("Fixed the parser."), 100);
        let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
        assert_eq!(
            returns(&a),
            Some(launch.map(str::to_string)),
            "{launch:?}: {a:?}"
        );
        let mut heard = Heard::default();
        s.turn_end_execute(&p, a, &opts, &[], &mut heard)
            .expect("returned");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            heard
                .said
                .iter()
                .any(|l| l.starts_with("RESTARTED ") && l.ends_with(line)),
            "{launch:?}: {:#?}",
            heard.said
        );
    }
}

/// A FABLE NOTICE THAT NAMES NO RESET MOVES THE SESSION WITH NO RETURN GUESSED: the move says its
/// return waits on a `/usage` sample of Fable's week, the ledger's switch says `back_at=-`, and a
/// later loop, however late, returns nothing on it before a sample, which is due at its first
/// quiet moment. Where the account's record of Fable's week names a reset still ahead, the move
/// says so and the ledger's switch carries that reset, which a later loop past it returns at.
/// NEGATIVE CONTROL: a record whose reset has passed times nothing.
#[test]
fn a_fable_limit_with_no_reset_waits_on_a_sample_or_the_accounts_record_never_a_guess() {
    let screen = fx::screen(fx::WALL_FABLE_NO_RESET_2_1_301);
    let moved = |tag: &str, week: Option<FableWeek>| {
        let (dir, ledger) = ledger_at(tag);
        let host = FableHost::launched(LaunchModel::Unnamed, &["relaunched"]);
        *host.fable_week.lock().unwrap() = week;
        let opts = opts_for(&host);
        let mut m = Mock::new(vec![screen.clone()]);
        let mut s = loop_at(&mut m, &ledger, NOW);
        let p = point_at(&s, screen.clone(), 100);
        let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
        assert!(moves_to_opus(&a), "{tag}: {a:?}");
        let mut heard = Heard::default();
        s.turn_end_execute(&p, a, &opts, &[], &mut heard)
            .expect("relaunched");
        drop(s);
        assert!(
            host.week_reads.load(Ordering::SeqCst) >= 1,
            "{tag}: the record was read"
        );
        (dir, ledger, heard.said)
    };

    let (dir, ledger, said) = moved("no-reset", None);
    assert!(
        said.iter().any(|l| l.starts_with("RESTARTED ")
            && l.ends_with(
                "relaunch --model claude-opus-5-5 (from fable, the notice names no reset: the \
                 return waits on a /usage sample of Fable's week; session-only)"
            )),
        "{said:#?}"
    );
    assert_eq!(
        typed_rows(&ledger, RULE_MODEL_FALLBACK),
        [(
            "relaunch --model claude-opus-5-5".to_string(),
            model_switch_reason(Some("fable"), None, None, NOW)
        )]
    );
    let open = open_model_switch(&ledger, Some(SID)).expect("the open switch");
    assert_eq!(
        (open.from.as_deref(), open.to.as_str(), open.back_at_unix),
        (Some("fable"), FABLE_FALLBACK, None)
    );
    let host = FableHost::launched(LaunchModel::Unnamed, &["relaunched"]);
    let opts = opts_for(&host);
    let mut m = Mock::new(vec![ended("Fixed the parser.")]);
    let mut s = later_loop(&mut m, &ledger, NOW + 30 * 86_400);
    let p = point_at(&s, ended("Fixed the parser."), 101);
    let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
    let due = s.turn_end.model_sample_due(s.fx.mono());
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(returns(&a), None, "no reading, no return: {a:?}");
    assert!(host.asked().is_empty(), "{:?}", host.asked());
    assert!(due, "the sample is due at the later loop's quiet moment");

    let recorded = NOW + 3 * 86_400;
    let (dir, ledger, said) = moved(
        "record",
        Some(FableWeek {
            pct: Some(100),
            reset: Some(recorded),
        }),
    );
    assert!(
        said.iter().any(|l| l.starts_with("RESTARTED ")
            && l.contains("(from fable, back at ")
            && l.ends_with(
                ", the reset the account's record of Fable's week names; session-only)"
            )),
        "{said:#?}"
    );
    assert_eq!(
        open_model_switch(&ledger, Some(SID)).map(|o| o.back_at_unix),
        Some(Some(recorded))
    );
    let host = FableHost::launched(LaunchModel::Unnamed, &["relaunched"]);
    let opts = opts_for(&host);
    let mut m = Mock::new(vec![ended("Fixed the parser.")]);
    let mut s = later_loop(&mut m, &ledger, recorded + 60);
    let p = point_at(&s, ended("Fixed the parser."), 101);
    let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(returns(&a), Some(None), "past the record's reset: {a:?}");

    let (dir, ledger, _) = moved(
        "record-passed",
        Some(FableWeek {
            pct: Some(100),
            reset: Some(NOW - 60),
        }),
    );
    let open = open_model_switch(&ledger, Some(SID)).map(|o| o.back_at_unix);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(open, Some(None), "a passed reset times nothing");
}

/// A MODEL SWITCH IS READ BACK BY A LOOP THAT STARTS AFTER IT, and a switch an older build wrote
/// (no `launch=` in its reason; this build never makes its move) opens nothing: a later loop past
/// its reset returns nothing. NEGATIVE CONTROLS: a switch returned opens nothing either, and the
/// same row with its `launch=` is returned past its reset.
#[test]
fn a_model_switch_is_read_back_by_a_loop_that_starts_after_it_and_an_older_builds_is_not() {
    let back_at = NOW - 60;
    for (legacy, restored) in [(false, false), (false, true), (true, false)] {
        let (dir, ledger) = ledger_at(&format!("read-back-{legacy}-{restored}"));
        let made = seed_switch(&ledger, Some(back_at), None, restored, legacy);
        let open = open_model_switch(&ledger, Some(SID));
        let host = FableHost::launched(LaunchModel::Unnamed, &["relaunched"]);
        let opts = opts_for(&host);
        let mut m = Mock::new(vec![ended("Fixed the parser.")]);
        let mut s = later_loop(&mut m, &ledger, NOW);
        let p = point_at(&s, ended("Fixed the parser."), 100);
        let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
        let standing = !legacy && !restored;
        assert_eq!(
            open,
            standing.then(|| OpenSwitch {
                from: Some("fable".to_string()),
                to: FABLE_FALLBACK.to_string(),
                launch: None,
                back_at_unix: Some(back_at),
                asked: true,
                made_unix: made,
                proc: None,
            }),
            "legacy {legacy}, restored {restored}"
        );
        assert_eq!(
            returns(&a).is_some(),
            standing,
            "legacy {legacy}, restored {restored}: {a:?}"
        );
    }
    // The row a build before the removal wrote for its `/model` switch opens nothing either.
    let (dir, ledger) = ledger_at("read-back-slash-model");
    let row = Row {
        rule_id: RULE_MODEL_FALLBACK,
        outcome: Outcome::Typed,
        command: "/model opus",
        reason: &format!(
            "the turn-end policy (model switch: from=fable back_at={back_at} made={})",
            NOW - 3600
        ),
        box_seq: 90,
    }
    .to_json((NOW - 3600) * 1000, Some(SID));
    std::fs::write(&ledger, row + "\n").expect("the ledger");
    let open = open_model_switch(&ledger, Some(SID));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(open, None);
}

/// AN ACCOUNT MOVE MADE KEEPS THE MODEL SWITCH FOR THE LOOP AFTER IT: the account's relaunch keeps
/// the line's `--model`, so the return is still owed, and a later loop past the reset makes it.
/// A switch made again after the move stands as the newest.
#[test]
fn an_account_move_made_keeps_the_model_switch_for_the_loop_after_it() {
    let (dir, ledger) = ledger_at("account-keeps-switch");
    let back_at = NOW - 600;
    let row = |rule: &str, outcome: Outcome, command: &str, reason: &str, ts: i64| {
        Row {
            rule_id: rule,
            outcome,
            command,
            reason,
            box_seq: 1,
        }
        .to_json(ts, Some(SID))
    };
    let switch = |ts: i64, launch: Option<&str>| {
        row(
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model claude-opus-5-5",
            &model_switch_reason(Some("fable"), launch, Some(back_at), 1),
            ts,
        )
    };
    let to = format!("{ACCOUNT_MOVE_COMMAND}work-2");
    let moved = |ts: i64| {
        row(
            RULE_ACCOUNT,
            Outcome::Typed,
            &to,
            &account_switch_reason("work-2", AccountAt::Wall),
            ts,
        )
    };
    let write = |rows: &[String]| std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
    let open = || open_model_switch(&ledger, Some(SID)).map(|o| (o.to, o.launch, o.back_at_unix));
    let standing = |launch: Option<&str>| {
        Some((
            FABLE_FALLBACK.to_string(),
            launch.map(str::to_string),
            Some(back_at),
        ))
    };
    write(&[switch(1_000, None)]);
    assert_eq!(open(), standing(None));
    write(&[switch(1_000, None), moved(2_000)]);
    assert_eq!(open(), standing(None), "the move keeps the switch");
    write(&[
        switch(1_000, None),
        moved(2_000),
        switch(3_000, Some("fable")),
    ]);
    assert_eq!(open(), standing(Some("fable")), "the newest switch");
    write(&[switch(1_000, None), moved(2_000)]);
    let host = FableHost::launched(LaunchModel::Unnamed, &["relaunched"]);
    let opts = opts_for(&host);
    let mut m = Mock::new(vec![ended("Fixed the parser.")]);
    let mut s = later_loop(&mut m, &ledger, NOW);
    let p = point_at(&s, ended("Fixed the parser."), 100);
    let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(returns(&a), Some(None), "the return is still owed: {a:?}");
}

/// When a hosted run stops: once its window on the run's clock has passed, or once the host
/// shows what the run waits for and a tail after it has passed.
enum Until {
    After(Duration),
    Reached(Box<dyn Fn(&FableHost) -> bool + Send>, Duration),
}

impl Until {
    fn reached(reached: impl Fn(&FableHost) -> bool + Send + 'static, tail: Duration) -> Self {
        Self::Reached(Box::new(reached), tail)
    }

    fn judge(
        self,
        host: Arc<FableHost>,
        clock: Arc<crate::supervise::fx::ScriptedClock>,
        halt: Arc<AtomicBool>,
    ) -> Box<dyn FnMut() + Send> {
        let start = clock.mono();
        let hang = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let mut since: Option<Instant> = None;
        Box::new(move || {
            if halt.load(Ordering::SeqCst) {
                return;
            }
            let now = clock.mono();
            let due = match &self {
                Self::After(window) => now.saturating_duration_since(start) >= *window,
                Self::Reached(reached, tail) => {
                    if since.is_none() && reached(&host) {
                        since = Some(now);
                    }
                    since.is_some_and(|at| now.saturating_duration_since(at) >= *tail)
                }
            };
            assert!(
                due || Instant::now() < hang,
                "timed out after {:?} waiting for the run's stop to come due",
                aterm_spec::wait::MUST_HAPPEN
            );
            if due {
                halt.store(true, Ordering::SeqCst);
            }
        })
    }
}

/// A hosted run of the loop on `m` under `host`, its ledger at `ledger`, its clock at `clock`
/// (unix seconds; `None`: the system's second), taped and held to its replay.
fn hosted_run(
    m: &mut Mock,
    opts: &SuperviseOpts,
    host: &Arc<FableHost>,
    until: Until,
    timing: Option<TurnEndTiming>,
    ledger: &std::path::Path,
    clock: Option<i64>,
) -> (Result<(), String>, Vec<String>) {
    let halt = Arc::new(AtomicBool::new(false));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..opts.clone()
    };
    let mut out: Vec<u8> = Vec::new();
    let from = m.order.len();
    let run_clock = Arc::new(crate::supervise::fx::ScriptedClock::default());
    run_clock.run_virtual();
    if clock.is_none() {
        let now_ms = crate::supervise::journal::unix_ms();
        run_clock.set_at_ms(now_ms - now_ms.rem_euclid(1000));
        run_clock.set_running();
    }
    m.after_call = Some(until.judge(Arc::clone(host), Arc::clone(&run_clock), Arc::clone(&halt)));
    let (tape, mut taped) = memory_tape_on(m, run_clock);
    let mut s = session(&mut taped, Some(SID.to_string()));
    s.set_fx(tape.fx.clone());
    s.set_approval_ledger(Some(ledger.to_path_buf()));
    if let Some(t) = timing {
        s.set_turn_end_timing(t);
    }
    if let Some(now) = clock {
        s.set_clock(now, PDT);
        s.set_clock_running();
        s.set_zone_lookup(test_zones);
    }
    let header = hosted_header(&s, &opts);
    tape.core().reroot(header.roots.clone());
    let r = s.run_hosted(&opts, halt, &mut out);
    drop(s);
    drop(taped);
    m.after_call = None;
    tape.finish(&crate::supervise::tape::replay::hosted_end(&r));
    let lines: Vec<String> = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect();
    crate::supervise::tape::testing::assert_replays(&tape, &header, &sent(m, from), &lines);
    (r, lines)
}

/// A Fable wall that names a reset weeks on, and its transcript.
fn fable_named() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Running the suite.",
        "  ⎿  You've hit your Fable limit · resets Oct 30 at 9pm (America/Los_Angeles)",
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    r.extend(composer("  ? for shortcuts"));
    r
}

const FABLE_TRANSCRIPT: &str = r#"{"parentUuid":"a","isSidechain":false,"type":"user","message":{"role":"user","content":"run the suite"},"timestamp":"2026-10-08T16:35:31.654Z"}
{"parentUuid":"b","isSidechain":false,"type":"assistant","timestamp":"2026-10-08T16:35:31.676Z","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You've hit your Fable limit · resets Oct 30 at 9pm (America/Los_Angeles)"}]},"error":"rate_limit","isApiErrorMessage":true}
{"parentUuid":"c","isSidechain":false,"type":"system","subtype":"turn_duration","timestamp":"2026-10-08T16:35:31.677Z"}"#;

/// The clock of the hosted runs at [`fable_named`]: 12:00 PDT on 2026-10-08.
const NAMED_NOW: i64 = 1_791_486_000;

/// The reset [`fable_named`] names: 21:00 PDT on 2026-10-30.
const NAMED_RESET: i64 = 1_793_419_200;

/// THE FABLE LIMIT'S RELAUNCH KEEPS ITS WAY BACK WHEN THE TRANSCRIPT MOVES ON: the relaunch's own
/// carry-on is written after the notice, and the switch still names the notice's reset.
#[test]
fn a_fable_limits_relaunch_keeps_its_way_back_when_the_transcript_moves_on() {
    let carried = format!(
        "{FABLE_TRANSCRIPT}\n{}",
        r#"{"parentUuid":"d","isSidechain":false,"type":"user","message":{"role":"user","content":"aterm relaunched you on opus; carry on"},"timestamp":"2026-10-08T16:36:00.000Z"}"#
    );
    let run = |tails: Option<(String, String)>| {
        let (dir, ledger) = ledger_at("bucket-books");
        let host = FableHost::launched(LaunchModel::Unnamed, &["continued"]);
        *host.tails.lock().unwrap() = tails;
        let mut m = Mock::new(vec![busy_screen(), fable_named()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            ..SuperviseOpts::hosted()
        };
        let (r, lines) = hosted_run(
            &mut m,
            &opts,
            &host,
            Until::reached(|h| !h.asked().is_empty(), Duration::from_millis(500)),
            None,
            &ledger,
            Some(NAMED_NOW),
        );
        assert_eq!(r, Ok(()));
        let open = open_model_switch(&ledger, Some(SID));
        let _ = std::fs::remove_dir_all(&dir);
        let asks = host.tail_asks.load(Ordering::SeqCst);
        (lines, open.map(|o| (o.from, o.back_at_unix)), asks)
    };
    let want = Some((Some("fable".to_string()), Some(NAMED_RESET)));
    let (lines, open, _) = run(None);
    assert_eq!(open, want, "with no transcript: {lines:#?}");
    let (lines, open, asks) = run(Some((FABLE_TRANSCRIPT.to_string(), carried)));
    assert_eq!(open, want, "with the transcript moved on: {lines:#?}");
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.contains("relaunch --model claude-opus-5-5 (from fable, back at ")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("rule=wall-confirm@v1")),
        "nothing refused: {lines:#?}"
    );
    assert!(asks >= 1, "the decision asked: {asks}");
}

/// THE FABLE LIMIT'S RELAUNCH NOT MADE KEEPS THE DECISION'S VERDICT: a deferral or a refusal asks
/// the relaunch once, refuses nothing, and the transcript is read at the decision only.
#[test]
fn a_fable_limits_relaunch_not_made_keeps_the_decisions_verdict() {
    let moved = format!(
        "{FABLE_TRANSCRIPT}\n{}\n{}",
        r#"{"parentUuid":"d","isSidechain":false,"type":"user","message":{"role":"user","content":"carry on"},"timestamp":"2026-10-08T16:36:00.000Z"}"#,
        r#"{"parentUuid":"e","isSidechain":false,"type":"system","subtype":"turn_duration","timestamp":"2026-10-08T16:36:01.000Z"}"#
    );
    for answer in ["wait:changed", "refused:off"] {
        let (dir, ledger) = ledger_at("bucket-unmade-books");
        let host = FableHost::launched(LaunchModel::Unnamed, &[answer]);
        *host.tails.lock().unwrap() = Some((FABLE_TRANSCRIPT.to_string(), moved.clone()));
        let mut m = Mock::new(vec![busy_screen(), fable_named()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            ..SuperviseOpts::hosted()
        };
        let (r, lines) = hosted_run(
            &mut m,
            &opts,
            &host,
            Until::reached(|h| !h.asked().is_empty(), Duration::from_millis(500)),
            None,
            &ledger,
            Some(NAMED_NOW),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(r, Ok(()), "{answer}");
        assert_eq!(
            host.words(),
            ["model"],
            "{answer}: the relaunch asked once: {lines:#?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("rule=wall-confirm@v1")),
            "{answer}: nothing refused: {lines:#?}"
        );
        assert_eq!(
            host.tail_asks.load(Ordering::SeqCst),
            1,
            "{answer}: the transcript asked at the decision only: {lines:#?}"
        );
    }
}

/// THE FABLE LIMIT IS RELAUNCHED ON OPUS AND BACK, NEVER BY `/model`: the move and the return are
/// the host's relaunches, nothing typed into the session; a refused relaunch types nothing and
/// raises nothing. NEGATIVE CONTROL: the same run under a launch naming another model asks no
/// relaunch.
#[test]
fn a_fable_limit_is_relaunched_on_opus_and_back_never_by_model() {
    let fable = || {
        let mut r = rows(&[
            "⏺ Running the suite.",
            "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch \
             models with /model.",
            "",
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let run = |screens: Vec<Vec<String>>,
               launch: LaunchModel,
               answers: &[&'static str],
               ledger: &std::path::Path| {
        let host = FableHost::launched(launch, answers);
        let mut m = Mock::new(screens);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            ..SuperviseOpts::hosted()
        };
        let until = if answers.is_empty() {
            Until::After(Duration::from_secs(5))
        } else {
            Until::reached(|h| !h.asked().is_empty(), Duration::from_millis(300))
        };
        let (r, lines) = hosted_run(&mut m, &opts, &host, until, None, ledger, None);
        assert_eq!(r, Ok(()));
        (m, lines, host.words())
    };
    let no_model = |m: &Mock| !m.requests.iter().any(|r| r.contains("/model"));

    let (dir, ledger) = ledger_at("bucket-fallback");
    let (m, lines, restarted) = run(
        vec![busy_screen(), fable()],
        LaunchModel::Unnamed,
        &["adopted"],
        &ledger,
    );
    assert_eq!(restarted, ["model"], "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.contains("rule=model-fallback@v1 relaunch --model claude-opus-5-5 (from fable, ")),
        "the notice names no reset: none is guessed: {lines:#?}"
    );
    assert!(no_model(&m), "{:#?}", m.requests);
    let open = open_model_switch(&ledger, Some(SID)).expect("the open switch");
    assert_eq!(
        (
            open.from.as_deref(),
            open.to.as_str(),
            open.launch.as_deref(),
            open.back_at_unix
        ),
        (Some("fable"), FABLE_FALLBACK, None, None)
    );
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("bucket-other-launch");
    let (m, lines, restarted) = run(
        vec![busy_screen(), fable()],
        LaunchModel::Named("claude-sonnet-4-6".to_string()),
        &[],
        &ledger,
    );
    assert!(restarted.is_empty(), "{lines:#?}");
    assert!(no_model(&m), "{:#?}", m.requests);
    assert_eq!(open_model_switch(&ledger, Some(SID)), None);
    let _ = std::fs::remove_dir_all(&dir);

    for (launch, line) in [
        (
            None,
            "rule=model-restore@v1 relaunch with no --model, as its launch",
        ),
        (
            Some("fable"),
            "rule=model-restore@v1 relaunch --model fable",
        ),
    ] {
        let (dir, ledger) = ledger_at("bucket-back");
        let unix_now = crate::supervise::journal::unix_ms() / 1000;
        seed_switch_at(&ledger, unix_now, Some(unix_now - 60), launch, false);
        let (m, lines, restarted) = run(
            vec![busy_screen(), idle_screen()],
            LaunchModel::Unnamed,
            &["adopted"],
            &ledger,
        );
        assert_eq!(restarted, ["model-back"], "{launch:?}: {lines:#?}");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("RESTARTED seq=") && l.ends_with(line)),
            "{launch:?}: {lines:#?}"
        );
        assert!(no_model(&m), "{:#?}", m.requests);
        let _ = std::fs::remove_dir_all(&dir);
    }

    let (dir, ledger) = ledger_at("bucket-refused");
    let (m, lines, restarted) = run(
        vec![busy_screen(), fable()],
        LaunchModel::Unnamed,
        &["refused:line"],
        &ledger,
    );
    assert_eq!(restarted, ["model"]);
    assert!(no_model(&m), "{:#?}", m.requests);
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("RESTARTED") || l.starts_with("TYPED")),
        "{lines:#?}"
    );
    assert_eq!(
        m.requests
            .iter()
            .filter(|r| r.contains("meta set attention owner=supervisor"))
            .count(),
        0,
        "{:#?}",
        m.requests
    );
    let _ = std::fs::remove_dir_all(&dir);
}

const FABLE_SHELL: &str = include_str!(
    "../../../aterm-phase/src/fixtures/claude-2.1.283-wall-fable-goal-rate-limited-shell.txt"
);

/// A FABLE MOVE THE HOST KEEPS DEFERRING IS ASKED AGAIN AND NOTHING IS TYPED BLIND: at a wall no
/// reset ends, the loop asks the relaunch again on its back-off, types no turn and waits for no
/// reset.
#[test]
fn a_fable_move_the_host_keeps_deferring_is_asked_again_and_nothing_is_typed_blind() {
    let host = FableHost::launched(LaunchModel::Unnamed, &["wait:changed"]);
    let (jdir, journal) = journal_file("fable-waits");
    let (dir, ledger) = ledger_at("fable-waits");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        journal: Some(journal.clone()),
        ..SuperviseOpts::hosted()
    };
    let mut m = Mock::new(vec![busy_screen(), fx::screen(FABLE_SHELL)]);
    m.stall_sleep = Some(Duration::from_millis(5));
    let (r, _) = hosted_run(
        &mut m,
        &opts,
        &host,
        Until::reached(|h| h.asked().len() >= 12, Duration::from_millis(100)),
        Some(TurnEndTiming {
            restart_backoff: vec![Duration::from_millis(20)],
            reset_grace: Duration::ZERO,
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        }),
        &ledger,
        None,
    );
    assert_eq!(r, Ok(()));
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&jdir);
    let _ = std::fs::remove_dir_all(&dir);
    let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
    let asked = host.words();
    assert!(
        asked.len() >= 12 && asked.iter().all(|w| *w == "model"),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("restart:model waits")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(
            |l| l.starts_with("CONTINUED") || l.starts_with("WAITING ") && l.contains("resets")
        ),
        "nothing typed, no reset waited for, at a wall no reset ends: {lines:#?}"
    );
    assert_eq!(
        m.requests.iter().filter(|r| r.starts_with("turn")).count(),
        0,
        "{:#?}",
        m.requests
    );
}

/// A FABLE MOVE THE HOST KEEPS DEFERRING GIVES WAY TO THE CONTINUATION AT THE RESET: asked again
/// before the reset, no more often than its back-off allows, and the limit's continuation is typed
/// once the reset comes, the host still deferring.
#[test]
fn a_fable_move_the_host_keeps_deferring_gives_way_to_the_continuation_at_the_reset() {
    let reset = Duration::from_secs(1);
    let backoff = Duration::from_millis(20);
    let tail = Duration::from_millis(300);
    let resets = format!("· resets in {}s", reset.as_secs());
    let named: Vec<String> = fx::screen(FABLE_SHELL)
        .into_iter()
        .map(|r| {
            r.replace(
                "Run /usage-credits to continue or switch models with /model.",
                &resets,
            )
        })
        .collect();
    let host = FableHost::launched(LaunchModel::Unnamed, &["wait:changed"]);
    let (jdir, journal) = journal_file("fable-named");
    let (dir, ledger) = ledger_at("fable-named");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        journal: Some(journal.clone()),
        ..SuperviseOpts::hosted()
    };
    let mut m = Mock::new(vec![busy_screen(), named]);
    m.stall_sleep = Some(Duration::from_millis(5));
    let watched = journal.clone();
    let continued = move |_: &FableHost| {
        std::fs::read_to_string(&watched).is_ok_and(|j| {
            j.lines()
                .any(|l| l.contains("CONTINUED seq=") && l.contains("rule=usage-resume@v1"))
        })
    };
    let (r, _) = hosted_run(
        &mut m,
        &opts,
        &host,
        Until::reached(continued, tail),
        Some(TurnEndTiming {
            restart_backoff: vec![backoff],
            reset_grace: Duration::ZERO,
            reset_jitter: Duration::ZERO,
            limit_backoff: vec![Duration::from_secs(600)],
            ..TurnEndTiming::default()
        }),
        &ledger,
        None,
    );
    assert_eq!(r, Ok(()));
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&jdir);
    let _ = std::fs::remove_dir_all(&dir);
    let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
    let asked = host.asked().len();
    assert!(asked >= 2, "asked again before the reset: {lines:#?}");
    let room = usize::try_from((reset + tail).as_millis() / backoff.as_millis()).unwrap() + 2;
    assert!(
        asked <= room,
        "asked {asked} times, where the reset and the run's tail leave room for {room}: {:#?}",
        &lines[..lines.len().min(40)]
    );
    let first_turn = lines
        .iter()
        .position(|l| l.starts_with("CONTINUED ") && l.contains("usage-resume@v1"))
        .unwrap_or_else(|| panic!("the continuation at the reset: {lines:#?}"));
    assert!(
        lines[..first_turn]
            .iter()
            .any(|l| l.contains(" restart:model ask=model@") && l.contains(" step=wait:changed")),
        "{lines:#?}"
    );
}

/// A DEFERRED FABLE MOVE IS ASKED AGAIN AT THE RESET, ITS GRACE AND THE SESSION'S SHARE: a
/// back-off longer than the wait gives way to the notice's reset.
#[test]
fn a_deferred_fable_move_is_retried_at_the_reset_the_grace_and_the_sessions_share() {
    use super::super::turn_end_loop::resume_jitter;
    let named: Vec<String> = fx::screen(FABLE_SHELL)
        .into_iter()
        .map(|r| {
            r.replace(
                "Run /usage-credits to continue or switch models with /model.",
                "· resets in 3h",
            )
        })
        .collect();
    let grace = Duration::from_secs(150);
    let reset_unix = TEST_NOW + 3 * 3600;
    let run = |share: Duration| {
        let host = FableHost::launched(LaunchModel::Unnamed, &["wait:changed"]);
        let mut m = Mock::new(vec![named.clone()]);
        let mut s = session(&mut m, Some(SID.to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.set_turn_end_timing(TurnEndTiming {
            restart_backoff: vec![Duration::from_secs(10 * 3600)],
            reset_grace: grace,
            reset_jitter: share,
            ..TurnEndTiming::default()
        });
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..SuperviseOpts::hosted()
        };
        let point = Turn {
            phase: Phase::Idle,
            screen: Screen {
                rows: named.clone(),
                seq: 102,
                cursor_col: 2,
                ..Screen::default()
            },
            timed_out: false,
        };
        let action = TurnEndAction::Restart {
            why: Restart::Model {
                to: FABLE_FALLBACK.to_string(),
            },
            rule_id: RULE_MODEL_FALLBACK,
            otherwise: Box::new(TurnEndAction::Nothing),
        };
        let began = Instant::now();
        s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
            .expect("the restart is deferred, not failed");
        let took = began.elapsed();
        let Due { at, why, .. } = s.turn_end_due.clone().expect("a retry is due");
        assert!(why.contains("restart:model waits"), "{why}");
        (at.duration_since(began), took)
    };
    let span = Duration::from_secs(60);
    let share = resume_jitter(SID, reset_unix, span);
    assert!(
        share >= Duration::from_secs(5),
        "pick another sid: {share:?}"
    );
    let (due, took) = run(span);
    let want = Duration::from_secs(3 * 3600) + grace + share;
    assert!(
        due >= want && due <= want + took,
        "{due:?} vs {want:?} (the call took {took:?})"
    );
    let (due, took) = run(Duration::ZERO);
    let want = Duration::from_secs(3 * 3600) + grace;
    assert!(
        due >= want && due <= want + took,
        "no share: {due:?} vs {want:?} (the call took {took:?})"
    );
}

/// A RETURN THE HOST DEFERS NEVER HOLDS THE CONTINUATION: the return is asked at the point, once,
/// and the turn end goes on with its continuation; a return made types none.
#[test]
fn a_return_from_opus_the_host_defers_never_holds_the_continuation() {
    let run = |answers: &[&'static str]| {
        let (dir, ledger) = ledger_at("back-waits");
        let unix_now = crate::supervise::journal::unix_ms() / 1000;
        seed_switch_at(&ledger, unix_now, Some(unix_now - 60), None, false);
        let host = FableHost::launched(LaunchModel::Unnamed, answers);
        let mut done = rows(&["⏺ Done.", "", "✻ Worked for 3m 2s · done 4:24 PM", ""]);
        done.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        let mut m = Mock::new(vec![busy_screen(), done]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let (jdir, journal) = journal_file("back-waits");
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        let (r, lines) = hosted_run(
            &mut m,
            &opts,
            &host,
            Until::After(Duration::from_millis(1500)),
            Some(TurnEndTiming {
                restart_backoff: vec![Duration::from_millis(20)],
                ..TurnEndTiming::default()
            }),
            &ledger,
            None,
        );
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let journaled: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&jdir);
        (host.words(), lines, journaled)
    };
    let (asked, lines, journaled) = run(&["wait:changed"]);
    let continued = lines.iter().filter(|l| l.starts_with("CONTINUED")).count();
    assert_eq!(continued, 1, "{lines:#?}");
    assert_eq!(
        asked,
        ["model-back"],
        "asked at the point, not in a loop: {lines:#?}"
    );
    assert!(
        journaled
            .iter()
            .any(|l| l.starts_with("DEFERRED ") && l.contains("restart:model-back")),
        "{journaled:#?}"
    );
    let (asked, lines, _) = run(&["adopted"]);
    assert_eq!(asked, ["model-back"]);
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("CONTINUED")).count(),
        0,
        "{lines:#?}"
    );
}

/// The measured bucket wall (Claude Code 2.1.300, its auto-continue armed), its words Fable's.
fn fable_armed() -> Vec<String> {
    fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300)
        .into_iter()
        .map(|r| {
            r.replace("Sonnet 5.5 · Claude API", "Fable 5 · Claude API")
                .replace("your Sonnet limit", "your Fable limit")
        })
        .collect()
}

/// The reset [`fable_armed`] names: 16:20 PDT on 2026-10-17.
const ARMED_RESET: i64 = 1_792_279_200;

/// [`fable_armed`] as the relaunched session redraws it on Opus: the dialog's rows and the
/// auto-continue gone, and the limit's notice naming `model`.
fn armed_redrawn(model: &str) -> Vec<String> {
    fable_armed()
        .into_iter()
        .filter(|r| {
            ![
                "Usage limit reached",
                "Continuing automatically",
                "/rate-limit-options",
                "Claude Code will continue automatically",
                "to cancel the wait",
            ]
            .iter()
            .any(|g| r.contains(g))
        })
        .map(|r| {
            r.replace("Fable 5 · Claude API", "Opus 5.5 · Claude API")
                .replace("your Fable limit", &format!("your {model} limit"))
        })
        .collect()
}

/// `base` with `add` above its composer's frame.
fn above_composer(base: &[String], add: &[&str]) -> Vec<String> {
    let mut rows = base.to_vec();
    let top = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the composer's frame");
    rows.splice(top..top, add.iter().map(|s| (*s).to_string()));
    rows
}

/// `base` with `prompt` answered by Fable's limit again.
fn met_again(prompt: &str, base: &[String]) -> Vec<String> {
    above_composer(
        base,
        &[
            &format!("❯ {prompt}"),
            "  ⎿  You've hit your Fable limit · resets Oct 17 at 4:20pm (America/Los_Angeles)",
            "",
            "✻ Churned for 0s · done 8:49 AM",
            "",
        ],
    )
}

type Decided = Option<(i64, Option<&'static str>)>;

/// What the loop `s` decides at `rows`: when it acts, and by which rule (a wait's, the rule the
/// policy names at its end).
fn decided(
    s: &mut Session<'_, Mock>,
    rows: &[String],
    seq: u64,
    worked: Option<Duration>,
    opts: &SuperviseOpts,
) -> Decided {
    let point = point_at(s, rows.to_vec(), seq);
    match s
        .turn_end_at_point(&point, worked, opts)
        .expect("a decision")
    {
        TurnEndAction::WaitUntil { until, .. } => {
            let r = s.turn_end_reading(&point.screen, None, opts);
            Some((
                s.unix_of(until),
                decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
            ))
        }
        other => Some((s.now_unix(), other.rule_id())),
    }
}

/// A BUCKET THE FALLBACK'S RELAUNCH LEFT IS NO WALL OF THE RELAUNCHED SESSION: the Opus session
/// redrawn with Fable's old notice is continued at once, by this loop and a later one; a turn
/// asked on Opus that meets Fable's limit again, or a point since the relaunch, makes it a wall
/// once more. NEGATIVE CONTROL: Opus's own limit on the relaunched session is waited out.
#[test]
fn a_bucket_the_fallbacks_relaunch_left_is_no_wall_of_the_relaunched_session() {
    let now = ARMED_RESET - 2 * 86_400;
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let armed = fable_armed();
    let met = met_again("keep going", &armed_redrawn("Fable"));
    let successor = |ledger: &std::path::Path, at: i64, rows: &[String]| -> Decided {
        let mut m = Mock::new(vec![rows.to_vec()]);
        let mut s = later_loop(&mut m, ledger, at);
        decided(&mut s, rows, 101, None, &opts)
    };
    let short_turn = |d: Decided, from: i64| d == Some((from + 120, Some(RULE_CONTINUE)));
    let waits_out = |d: Decided| d.is_some_and(|(until, _)| until >= ARMED_RESET);
    let mut wrong = Vec::new();
    for (case, model) in [
        ("the bucket the relaunch left", "Fable"),
        ("control: another model's bucket", "Opus"),
        ("control: a point since the relaunch", "Fable"),
    ] {
        let moved_on = case.ends_with("since the relaunch");
        let (dir, ledger) = ledger_at(&format!("bucket-left-{model}-{moved_on}"));
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = loop_at(&mut m, &ledger, now);
        let _ = decided(&mut s, &armed, 100, None, &opts);
        let p = point_at(&s, armed.clone(), 101);
        let r = s.turn_end_reread(&p.screen, &opts);
        s.turn_end.restarted(
            &Restart::Model {
                to: FABLE_FALLBACK.to_string(),
            },
            true,
            &r,
            Instant::now(),
        );
        s.ledger_row(
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model claude-opus-5-5",
            &model_switch_reason(Some("fable"), None, Some(ARMED_RESET), now),
            101,
        );
        let screen = armed_redrawn(model);
        if moved_on {
            s.set_clock(now + 20, PDT);
            let _ = decided(
                &mut s,
                &ended("Here is the summary you asked for."),
                102,
                None,
                &opts,
            );
            s.set_clock(now + 30, PDT);
            let again = decided(&mut s, &met, 103, None, &opts);
            drop(s);
            let _ = std::fs::remove_dir_all(&dir);
            if !waits_out(again) {
                wrong.push(format!("{case}: {again:?}"));
            }
            continue;
        }
        s.set_clock(now + 30, PDT);
        let first = decided(&mut s, &screen, 102, None, &opts);
        let open = open_wall(&ledger, Some(SID)).is_some();
        let next = successor(&ledger, now + 60, &screen);
        if model == "Opus" {
            if !waits_out(first) || !waits_out(next) || !open {
                wrong.push(format!(
                    "{case}: first loop {first:?}, open wall {open}, successor {next:?}"
                ));
            }
            drop(s);
            let _ = std::fs::remove_dir_all(&dir);
            continue;
        }
        if !short_turn(first, now + 30) || open || !short_turn(next, now + 60) {
            wrong.push(format!(
                "{case}: first loop {first:?}, open wall {open}, successor {next:?}"
            ));
        }
        s.set_clock(now + 150, PDT);
        let p = point_at(&s, screen.clone(), 103);
        let a = s
            .turn_end_at_point(&p, None, &opts)
            .expect("the continuation");
        assert_eq!(a.rule_id(), Some(RULE_CONTINUE), "{case}");
        s.pass_wake_settle();
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("typed");
        let early = successor(&ledger, now + 160, &met);
        s.set_clock(now + 180, PDT);
        let again = decided(&mut s, &met, 104, None, &opts);
        drop(s);
        let after = successor(&ledger, now + 240, &met);
        let _ = std::fs::remove_dir_all(&dir);
        if !waits_out(early) || !waits_out(again) || !waits_out(after) {
            wrong.push(format!(
                "{case}, the continuation met the bucket: a successor before its read \
                 {early:?}, first loop {again:?}, a successor after {after:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A transcript of the session's turn `prompt` met by Fable's limit at `at`.
fn fable_transcript(prompt: &str, at: i64) -> String {
    let stamp = crate::harness::usage::rfc3339_utc(at);
    include_str!("fixtures/claude-2.1.294-transcript-session-limit.jsonl")
        .lines()
        .skip(1)
        .filter(|l| !l.contains("\"subtype\":\"informational\""))
        .map(|l| {
            let mut row = l
                .replace("seventh try", prompt)
                .replace(
                    "You've hit your session limit · resets 11:33am (America/Los_Angeles)",
                    "You've hit your Fable limit · resets Oct 17 at 4:20pm (America/Los_Angeles)",
                )
                .replace("\"five_hour\"", "\"seven_day_overage_included\"");
            for ms in ["921", "951", "952", "953"] {
                row = row.replace(
                    &format!("\"2026-10-08T16:34:58.{ms}Z\""),
                    &format!("\"{stamp}\""),
                );
            }
            row
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// A TURN ASKED ON THE FALLBACK IS WRITTEN FOR A LOOP THAT FOLLOWS: once a turn the person or
/// the host asked on Opus meets Fable's limit again, neither this loop nor a later one takes the
/// notice for the bucket the relaunch left; it is waited out. NEGATIVE CONTROL: the redraw with
/// nothing between is continued at once.
#[test]
fn a_turn_asked_on_the_fallback_is_written_for_a_loop_that_follows() {
    use crate::supervise::policy::turn_end::HostTurn;
    let now = ARMED_RESET - 2 * 86_400;
    let armed = fable_armed();
    let redrawn = armed_redrawn("Fable");
    let before = fable_transcript("carry on with the task", now - 600);
    let plain = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let successor = |ledger: &std::path::Path, at: i64, rows: &[String]| -> Decided {
        let mut m = Mock::new(vec![rows.to_vec()]);
        let mut s = later_loop(&mut m, ledger, at);
        s.set_mono_standing();
        decided(&mut s, rows, 101, None, &plain)
    };
    let waits_out = |d: Decided| d.is_some_and(|(until, _)| until >= ARMED_RESET);
    let mut wrong = Vec::new();
    for known in [false, true] {
        for (n, case) in [
            "a person's exchange, then their prompt met by the bucket",
            "the host's carry-on typed inside the relaunch, met by the bucket",
            "control: the redraw, nothing between",
            "the host's carry-on typed at its own step, met by the bucket",
        ]
        .into_iter()
        .enumerate()
        {
            let carried = n == 1;
            let (dir, ledger) = ledger_at(&format!("fallback-asked-{known}-{n}"));
            let host = FableHost::launched(
                LaunchModel::Unnamed,
                &[if carried { "continued" } else { "relaunched" }],
            );
            if known {
                *host.tails.lock().unwrap() = Some((before.clone(), before.clone()));
            }
            let opts = SuperviseOpts {
                idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
                ..plain.clone()
            };
            let mut m = Mock::new(vec![armed.clone()]);
            let mut s = loop_at(&mut m, &ledger, now);
            s.set_mono_standing();
            let p = point_at(&s, armed.clone(), 100);
            let a = s.turn_end_at_point(&p, None, &opts).expect("the fallback");
            assert!(moves_to_opus(&a), "{case}: {a:?}");
            s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
                .expect("relaunched");
            assert_eq!(host.words(), ["model"], "{case}");
            let (last, early) = match n {
                0 => {
                    s.set_clock(now + 30, PDT);
                    let _ = decided(&mut s, &redrawn, 102, None, &opts);
                    s.set_clock(now + 60, PDT);
                    let _ = decided(
                        &mut s,
                        &ended("Here is the summary you asked for."),
                        103,
                        Some(Duration::from_secs(20)),
                        &opts,
                    );
                    let last = met_again("now run the integration tests", &redrawn);
                    let early = successor(&ledger, now + 85, &last);
                    s.set_clock(now + 90, PDT);
                    (last, early)
                }
                1 | 3 => {
                    if n == 3 {
                        s.host_turn(HostTurn::CarryOn, 102);
                    }
                    let last = met_again("Continue from where you left off.", &redrawn);
                    let early = successor(&ledger, now + 25, &last);
                    s.set_clock(now + 30, PDT);
                    (last, early)
                }
                _ => {
                    let early = successor(&ledger, now + 25, &redrawn);
                    s.set_clock(now + 30, PDT);
                    (redrawn.clone(), early)
                }
            };
            let first = decided(
                &mut s,
                &last,
                104,
                (n == 0).then(|| Duration::from_secs(1)),
                &opts,
            );
            drop(s);
            let after = successor(&ledger, now + 120, &last);
            let third = successor(&ledger, now + 600, &last);
            let _ = std::fs::remove_dir_all(&dir);
            let ok = if n != 2 {
                [early, first, after, third].into_iter().all(waits_out)
            } else {
                early == Some((now + 25 + 120, Some(RULE_CONTINUE)))
                    && first == Some((now + 30 + 120, Some(RULE_CONTINUE)))
                    && after == Some((now + 120 + 120, Some(RULE_CONTINUE)))
            };
            if !ok {
                wrong.push(format!(
                    "{case}, known={known}: a successor before the first loop's read {early:?}, \
                     the first loop {first:?}, a successor after {after:?}, one after that \
                     {third:?}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A FALLBACK'S RELAUNCH IN FLIGHT WHEN ITS LOOP ENDS IS MADE AT ITS FIRST ASK: the deferral with
/// a claim taken is journaled as an open move, a later loop asks the same move again and, once
/// made, dates the switch by the first ask's second; a turn the person asked on Opus since that
/// met Fable again is waited out, a plain redraw continued.
#[test]
fn a_fallbacks_relaunch_in_flight_when_its_loop_ends_is_made_at_its_first_ask() {
    use super::super::turn_end_loop::journaled_open_moves;
    use crate::supervise::journal::JournalRecord;
    let now = ARMED_RESET - 2 * 86_400;
    let armed = fable_armed();
    let redrawn = armed_redrawn("Fable");
    let met = met_again("please run the integration tests", &redrawn);
    let before = fable_transcript("carry on with the task", now - 600);
    let journaled = |heard: &Heard| -> String {
        heard
            .noted
            .iter()
            .map(|line| {
                JournalRecord::of_line(now * 1000 + 400, Some(SID), line, None).to_json() + "\n"
            })
            .collect()
    };
    let hosted = |transcript: &str, answers: &[&'static str], claim: RestartClaim| {
        let host = FableHost::claiming(LaunchModel::Unnamed, answers, &[claim]);
        *host.tails.lock().unwrap() = Some((transcript.to_string(), transcript.to_string()));
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            policy: owners_policy(),
            ..auto(30)
        };
        (host, opts)
    };
    let mut wrong = Vec::new();
    for (n, answered) in [true, false].into_iter().enumerate() {
        let (dir, ledger) = ledger_at(&format!("relaunch-in-flight-{n}"));
        let (host, opts) = hosted(&before, &["wait:resume"], RestartClaim::Taken);
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = loop_at(&mut m, &ledger, now);
        let p = point_at(&s, armed.clone(), 100);
        let a = s.turn_end_at_point(&p, None, &opts).expect("the fallback");
        assert!(moves_to_opus(&a), "{a:?}");
        let mut heard = Heard::default();
        s.turn_end_execute(&p, a, &opts, &[], &mut heard)
            .expect("waits");
        assert_eq!(host.words(), ["model"]);
        drop(s);
        let journal = journaled(&heard);
        let run = journaled_open_moves(&journal, Some(SID))
            .0
            .and_then(|run| run.claimed.map(|claimed| (run.kind, run.at, claimed)));
        let (screen, transcript) = if answered {
            (
                met.clone(),
                fable_transcript("please run the integration tests", now + 55),
            )
        } else {
            (redrawn.clone(), before.clone())
        };
        let (_host2, opts2) = hosted(&transcript, &["relaunched"], RestartClaim::Held);
        let mut m2 = Mock::new(vec![screen.clone()]);
        let mut s2 = loop_at(&mut m2, &ledger, now + 61);
        s2.seed_model_switch(&ledger);
        s2.seed_open_move_in(&journal);
        s2.seed_wall_memory(open_wall(&ledger, Some(SID)));
        let p = point_at(&s2, screen.clone(), 101);
        let a = s2
            .turn_end_at_point(&p, None, &opts2)
            .expect("the relaunch");
        let asked = moves_to_opus(&a);
        s2.pass_wake_settle();
        let mut heard2 = Heard::default();
        s2.turn_end_execute(&p, a, &opts2, &[], &mut heard2)
            .expect("relaunched");
        let made = open_model_switch(&ledger, Some(SID)).map(|o| o.made_unix - now);
        let ended = journaled_open_moves(&(journal.clone() + &journaled(&heard2)), Some(SID))
            == (None, None);
        s2.set_clock(now + 70, PDT);
        s2.pass_wake_settle();
        let decided = decided(&mut s2, &screen, 102, None, &opts2);
        drop(s2);
        let _ = std::fs::remove_dir_all(&dir);
        let ok = run == Some((Restart::MODEL, now, now))
            && asked
            && made == Some(0)
            && ended
            && if answered {
                decided.is_some_and(|(at, _)| at >= ARMED_RESET)
            } else {
                decided.is_some_and(|(at, rule)| rule == Some(RULE_CONTINUE) && at < ARMED_RESET)
            };
        if !ok {
            wrong.push(format!(
                "answered {answered}: run {run:?}, asked again {asked}, made {made:?} after the \
                 first ask, the run ended {ended}, then {decided:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The agent process a test host names: its pid, start (unix seconds) and conversation.
type NamedAgent = (Option<u32>, Option<i64>, Option<&'static str>);

/// What one point decides of a model switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwitchAt {
    /// Its return is made (`model-restore@v1`).
    Restores,
    /// Its return waits for the point the process is named on a conversation; the switch stands.
    Holds,
    /// The switch is over: a closing row, and nothing restored.
    Closes,
}

/// A MODEL SWITCH IS ITS RELAUNCHED PROCESS, AND ANOTHER PROGRAM IN THE TAB CLOSES IT: the return
/// past the reset is made in the process the Fable move relaunched (and one that resumed its
/// conversation, or one the host's own relaunch started); a program the person started in the tab
/// on another conversation closes the switch with a row a later loop reads; a process the host
/// names no conversation of yet holds the return. The process may be named only at its point
/// ([`IdleHost::agent_resumed`]). NEGATIVE CONTROLS: a switch no loop scoped to a process is
/// returned wherever it is read, and the relaunched process itself returns.
#[test]
fn a_model_switch_is_its_relaunched_process_and_another_program_in_the_tab_closes_it() {
    use SwitchAt::{Closes, Holds, Restores};
    let now = NOW;
    let back_at = now - 600;
    let p1: NamedAgent = (Some(4100), Some(now - 7_100), Some("conv-a"));
    let b: NamedAgent = (Some(4200), Some(now - 300), Some("conv-b"));
    let named = |host: &FableHost, (pid, started, conversation): NamedAgent| {
        *host.pid.lock().unwrap() = pid;
        *host.started.lock().unwrap() = started;
        *host.conversation.lock().unwrap() = conversation.map(str::to_string);
    };
    let closed = |ledger: &std::path::Path| {
        std::fs::read_to_string(ledger)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(RULE_MODEL_RESTORE) && l.contains("\"decision\":\"waiting\""))
            .count()
    };
    let run = |ledger: &std::path::Path,
               agents: &[NamedAgent],
               owed: bool,
               step: Option<&'static str>,
               late: bool| {
        let host = FableHost::launched(LaunchModel::Unnamed, &[]);
        host.relaunch_owed.store(owed, Ordering::SeqCst);
        let opts = opts_for(&host);
        let mut m = Mock::new(vec![ended("Fixed the parser.")]);
        let mut s = loop_at(&mut m, ledger, now);
        s.seed_model_switch(ledger);
        if let Some(word) = step {
            named(&host, p1);
            *host.step.lock().unwrap() = Some(word);
            host.carry.store(true, Ordering::SeqCst);
            let rows = ended("Fixed the parser.");
            let mut point = point_at(&s, rows.clone(), 101);
            point.screen.cursor_row = rows
                .iter()
                .rposition(|r| r.starts_with('❯'))
                .expect("caret");
            point.screen.cursor_col = 2;
            s.host_steps_here(&point, &opts, &mut false, &mut Alone)
                .expect("the host's step");
            assert_eq!(
                host.carried.load(Ordering::SeqCst),
                1,
                "the step {word} was taken"
            );
        }
        let mut decided = Vec::new();
        for (n, agent) in agents.iter().enumerate() {
            if late {
                named(&host, (None, None, None));
                let (Some(pid), Some(started), conversation) = *agent else {
                    panic!("a process named at its point has a pid and a start: {agent:?}");
                };
                *host.late_agent.lock().unwrap() = Some((pid, started, conversation));
            } else {
                named(&host, *agent);
            }
            let before = closed(ledger);
            let point = point_at(&s, ended("Fixed the parser."), 102 + n as u64);
            let a = s
                .turn_end_at_point(&point, Some(Duration::from_secs(300)), &opts)
                .expect("a decision");
            decided.push(match (returns(&a).is_some(), closed(ledger) > before) {
                (true, false) => Restores,
                (false, false) => Holds,
                (false, true) => Closes,
                (true, true) => panic!("restored and closed at one point: {a:?}"),
            });
        }
        decided
    };
    // The ledger an earlier loop left: the Fable move onto Opus, back at a reset now past, and
    // (`stamped`) a point the relaunched process `p1` had since.
    let switched = |tag: &str, stamped: bool| {
        let (dir, ledger) = ledger_at(&format!("switch-process-{tag}"));
        let made = now - 7_200;
        let row = Row {
            rule_id: RULE_MODEL_FALLBACK,
            outcome: Outcome::Typed,
            command: "relaunch --model claude-opus-5-5",
            reason: &model_switch_reason(Some("fable"), None, Some(back_at), made),
            box_seq: 90,
        }
        .to_json(made * 1000, Some(SID));
        std::fs::write(&ledger, row + "\n").expect("the ledger");
        if stamped {
            let first = run(&ledger, &[p1], false, None, false);
            assert_eq!(first, [Restores], "its own process restores");
        }
        (dir, ledger)
    };
    let unnamed: NamedAgent = (Some(4300), Some(now - 200), None);
    let resumed_a: NamedAgent = (Some(4300), Some(now - 200), Some("conv-a"));
    let elsewhere: NamedAgent = (Some(4300), Some(now - 200), Some("conv-z"));
    let mut wrong = Vec::new();
    for (n, (case, agents, owed, step, late, want)) in [
        (
            "the person's fresh program, B",
            vec![b],
            false,
            None,
            false,
            vec![Closes],
        ),
        (
            "B, after a step of the host's that typed a line and started nothing",
            vec![b],
            false,
            Some("continued"),
            false,
            vec![Closes],
        ),
        (
            "control: the relaunched process",
            vec![p1],
            false,
            None,
            false,
            vec![Restores],
        ),
        (
            "control: the relaunched process after a /clear",
            vec![(p1.0, p1.1, Some("conv-a2"))],
            false,
            None,
            false,
            vec![Restores],
        ),
        (
            "control: a relaunch that resumed A",
            vec![resumed_a],
            false,
            None,
            false,
            vec![Restores],
        ),
        (
            "another process whose conversation the host does not name yet, then A's",
            vec![unnamed, resumed_a],
            false,
            None,
            false,
            vec![Holds, Restores],
        ),
        (
            "another process whose conversation the host does not name yet, then another's",
            vec![unnamed, (unnamed.0, unnamed.1, Some("conv-b"))],
            false,
            None,
            false,
            vec![Holds, Closes],
        ),
        (
            "a relaunch the host still owes, on a conversation the switch's process never named",
            vec![elsewhere],
            true,
            None,
            false,
            vec![Restores],
        ),
        (
            "a relaunch the host's own step started afresh, on a new conversation",
            vec![(Some(4300), Some(now - 200), Some("conv-n"))],
            false,
            Some("done:fresh"),
            false,
            vec![Restores],
        ),
        (
            "B, named only at its point",
            vec![b],
            false,
            None,
            true,
            vec![Closes],
        ),
        (
            "control: a relaunch that resumed A, named only at its point",
            vec![resumed_a],
            false,
            None,
            true,
            vec![Restores],
        ),
        (
            "another process with no conversation yet, then A's, each named only at its point",
            vec![unnamed, resumed_a],
            false,
            None,
            true,
            vec![Holds, Restores],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (dir, ledger) = switched(&format!("case-{n}"), true);
        let got = run(&ledger, &agents, owed, step, late);
        let keeps = !want.contains(&Closes);
        let rows = closed(&ledger);
        let open = open_model_switch(&ledger, Some(SID)).is_some();
        let _ = std::fs::remove_dir_all(&dir);
        if got != want || rows != usize::from(!keeps) || open != keeps {
            wrong.push(format!(
                "{case}: {got:?}, closing rows {rows}, open for the loop after {open}"
            ));
        }
    }
    let (dir, ledger) = switched("unscoped", false);
    let got = run(&ledger, &[b], false, None, false);
    if got != [Restores] || closed(&ledger) != 0 {
        wrong.push(format!(
            "control: a switch no loop scoped to a process: {got:?}"
        ));
    }
    let _ = std::fs::remove_dir_all(&dir);
    let (dir, ledger) = switched("same-loop", false);
    let got = run(&ledger, &[p1, b], false, None, false);
    if got != [Restores, Closes] {
        wrong.push(format!(
            "the loop that saw the relaunched process, at B's point: {got:?}"
        ));
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(wrong.is_empty(), "{wrong:#?}");
}
