// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `occupancy-break-claude`: all of it but the run, which needs a real Claude Code (its
//! listing, its lab's `aterm.toml` and seam, the stub's script, its promotion, and its judge over
//! a synthetic recording).

use std::path::Path;

use aterm_agent::supervise::occupancy::rules::NUDGE_SEAM;
use aterm_agent::supervise::occupancy::{templates, waiting_declared};
use aterm_agent::supervise::{Autopilot, Occupancy, SupervisorConfig};

use super::*;
use crate::scn::{Plan, scenarios};
use crate::stub::{ToolTurn, tool_turn};

/// The rows Claude Code draws waiting on one workflow (`aterm-phase`'s capture), the capture's
/// own header and exit lines left out.
fn workflow_screen() -> Vec<String> {
    include_str!("../../aterm-phase/src/fixtures/wait_bg.out")
        .lines()
        .filter(|l| !l.starts_with("== ") && !l.starts_with("exit="))
        .map(str::to_string)
        .collect()
}

/// [`workflow_screen`] with its workflow gone: no status row and no footer row for it.
fn done_screen() -> Vec<String> {
    workflow_screen()
        .into_iter()
        .filter(|l| !l.contains("dynamic workflow") && !l.contains("cache-lever-pick"))
        .collect()
}

/// THE SCENARIO IS LISTED ONCE, with its `[harness]` lines, its instance's seam and its plan, as
/// `lab scn run occupancy-break-claude` finds it.
#[test]
fn the_scenario_is_listed_with_its_lab() {
    let listed: Vec<_> = scenarios().into_iter().filter(|s| s.name == NAME).collect();
    let [s] = listed.as_slice() else {
        panic!("{} scenarios named {NAME}", listed.len());
    };
    assert_eq!(s.harness_keys, HARNESS_KEYS);
    assert_eq!(s.kw.instance_env, INSTANCE_ENV);
    assert!(matches!(s.plan, Plan::OccupancyBreak), "{:?}", s.plan);
    assert!(
        scenarios()
            .iter()
            .filter(|s| s.name != NAME)
            .all(|s| s.kw.instance_env.is_empty()),
        "no other scenario's instance gets a seam"
    );
}

/// THE LAB'S SCRATCH `aterm.toml` GRANTS PROMPT: its `[harness]` lines land under the root
/// `[harness]` table, where the owner's key reaches `prompt`, under `cruise` with a 5 s grace.
/// NEGATIVE CONTROL: the same lab without the scenario's lines reads `observe`, and no grant.
#[test]
fn its_scratch_aterm_toml_grants_prompt_under_cruise() {
    let (cfg, notes) =
        SupervisorConfig::from_aterm_toml(&crate::lab::scratch_aterm_toml("", HARNESS_KEYS));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(cfg.occupancy, Occupancy::Prompt);
    assert_eq!(cfg.autopilot, Autopilot::Cruise);
    assert_eq!(cfg.human_grace_s, 5);
    let (plain, _) = SupervisorConfig::from_aterm_toml(&crate::lab::scratch_aterm_toml("", ""));
    assert_ne!(plain.occupancy, Occupancy::Prompt);
}

/// THE INSTANCE GETS THE NUDGE'S SEAM AT A MINUTE: `LAB_INSTANCE_ENV` takes the scenario's words
/// as the seam's name and [`NUDGE_S`].
#[test]
fn its_instance_gets_the_nudge_seam_at_a_minute() {
    assert_eq!(
        crate::lab::instance_env(INSTANCE_ENV),
        Ok(vec![(NUDGE_SEAM.to_string(), NUDGE_S.to_string())])
    );
}

/// A request body of `messages`.
fn body(messages: Vec<J>) -> Vec<u8> {
    J::obj([("messages", J::Arr(messages))])
        .dump(COMPACT)
        .into_bytes()
}

fn user(text: &str) -> J {
    J::obj([("role", J::s("user")), ("content", J::s(text))])
}

fn said(text: &str) -> J {
    J::obj([
        ("role", J::s("assistant")),
        (
            "content",
            J::Arr(vec![J::obj([("type", J::s("text")), ("text", J::s(text))])]),
        ),
    ])
}

fn called(name: &str) -> J {
    J::obj([
        ("role", J::s("assistant")),
        (
            "content",
            J::Arr(vec![J::obj([
                ("type", J::s("tool_use")),
                ("id", J::s("t1")),
                ("name", J::s(name)),
                ("input", J::obj([])),
            ])]),
        ),
    ])
}

fn result(text: &str) -> J {
    J::obj([
        ("role", J::s("user")),
        (
            "content",
            J::Arr(vec![J::obj([
                ("type", J::s("tool_result")),
                ("tool_use_id", J::s("t1")),
                ("content", J::s(text)),
            ])]),
        ),
    ])
}

/// THE STUB PLAYS THE AGENT FROM THE SCRIPT, as its `tool` mode reads it from the lab's file:
/// the main thread calls `Workflow` with the script, says it started, and answers the nudge with
/// a declaration the `WAITING:` reader takes; a step's subagent, its first message under the
/// mark, sleeps and says so. NEGATIVE CONTROL: a main thread with no `toolstart` is not the
/// script's, and the task never carries the steps' mark (so the main thread is never answered as
/// a step).
#[test]
fn the_stub_plays_the_agent_from_the_script() {
    let script = J::parse(&script().dump(COMPACT)).expect("the script is JSON");
    let turn = |messages: Vec<J>| tool_turn(&body(messages), &script);
    let Some(ToolTurn::Call(name, input, 0)) = turn(vec![user(TURN)]) else {
        panic!("{:?}", turn(vec![user(TURN)]));
    };
    assert_eq!(name, "Workflow");
    assert_eq!(input.get("script").and_then(J::as_str), Some(WORKFLOW));
    let started = vec![user(TURN), called("Workflow"), result("Workflow started")];
    assert_eq!(
        turn(started.clone()),
        Some(ToolTurn::Say(STARTED.into(), 1))
    );
    let nudge = templates::wait_nudge(61_000);
    let mut nudged = started;
    nudged.extend([said(STARTED), user(nudge.text())]);
    assert_eq!(turn(nudged), Some(ToolTurn::Say(WAITING.into(), 2)));
    assert!(waiting_declared(WAITING));

    let step = format!("{STEP_MARK} 1: run sleep 900 in Bash, then say slept.");
    let Some(ToolTurn::Call(name, input, 0)) = turn(vec![user(&step)]) else {
        panic!("{:?}", turn(vec![user(&step)]));
    };
    assert_eq!(name, "Bash");
    assert_eq!(input.get("command").and_then(J::as_str), Some("sleep 900"));
    assert_eq!(
        turn(vec![user(&step), called("Bash"), result("")]),
        Some(ToolTurn::Say("slept".into(), 1))
    );

    assert_eq!(turn(vec![user("say hello")]), None);
    assert!(!TURN.contains(STEP_MARK));
    assert!(WORKFLOW.starts_with("export const meta = {"), "{WORKFLOW}");
    assert!(
        WORKFLOW.contains(&format!("`{STEP_MARK} ${{n}}: ")),
        "{WORKFLOW}"
    );
}

/// THE RUN PROMOTES UNDER THE SCENARIO'S NAME: `lab tape promote occupancy-break-claude-01` names
/// the corpus tape after the scenario, a corpus name, and refuses a run whose summary carries the
/// judge's `error`. NEGATIVE CONTROL: a run the judge passed is not refused.
#[test]
fn its_run_promotes_under_its_name_unless_the_judge_failed() {
    let run = Path::new("/results").join(format!("{NAME}-01"));
    assert_eq!(crate::tape::scenario_of(&run).as_deref(), Some(NAME));
    assert!(crate::tape::corpus_name(NAME));
    let passed = J::obj([("occupancy_judged", J::s("pass"))]);
    assert_eq!(crate::tape::run_failed(&passed), None);
    let failed = J::obj([
        ("occupancy_judged", J::Arr(vec![J::s("no hold declared")])),
        ("error", J::s("no hold declared")),
    ]);
    assert!(crate::tape::run_failed(&failed).is_some());
}

/// The reader's workflow wait: the capture of one workflow is one; the same screen with its
/// workflow gone is not.
#[test]
fn a_workflow_wait_is_read_off_the_screen() {
    assert!(workflow_waits(&workflow_screen()));
    assert!(!workflow_waits(&done_screen()));
}

const SID: &str = "s-0123456789abcdef0123";
const EPISODE: &str = "00000000000000ab";

/// A ledger row of `sid` as the loop writes it.
fn row(sid: &str, rule: &str, decision: &str, command: &str, reason: &str) -> String {
    J::obj([
        ("ts", J::Int(1_000)),
        ("sid", J::s(sid)),
        ("rule_id", J::s(rule)),
        ("decision", J::s(decision)),
        ("cmd_sha256", J::s("")),
        ("command", J::s(command)),
        ("reason", J::s(reason)),
        ("box_seq", J::Int(0)),
    ])
    .dump(COMPACT)
}

/// A `messages` row keyed `key` titled `title`, as the wire writes it (each byte but a letter or
/// a digit `%XX`).
fn message(id: u32, key: &str, title: &str) -> String {
    let pct = |s: &str| {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>()
    };
    format!(
        "message {id} at=1000 ago_ms=5 tag=harness sev=info origin=host state=live glass=- rep=1 \
         key={} title={} detail= actions=-",
        pct(key),
        pct(title)
    )
}

/// A recording that passes, in parts the negative controls change one at a time.
#[derive(Clone)]
struct Parts {
    status: String,
    journal: Vec<String>,
    ledger: Vec<String>,
    messages: Vec<String>,
    after: Vec<Vec<String>>,
}

fn passing() -> Parts {
    let reason = format!("rule=wait-nudge@v1 episode={EPISODE} held_ms=61000");
    let line = templates::wait_nudge(61_000);
    Parts {
        status: "OK 1 agent=claude occupancy=prompt occupancy_hold=declared ready=0 slots=- \
                 idle_ready_min=0"
            .to_string(),
        journal: [
            "EVENT idle",
            "OCCUPANCY seq=4 hold open",
            &format!(
                "OCCUPANCY seq=9 rule=wait-nudge@v1 rev=builtin episode={EPISODE} door=break typed"
            ),
            "OCCUPANCY seq=11 hold declared held_s=75",
        ]
        .map(str::to_string)
        .to_vec(),
        ledger: vec![
            row(
                SID,
                "continue@v1",
                "typed",
                "continue",
                "a policy's own row",
            ),
            row(
                "s-another",
                "fact-would@v1",
                "waiting",
                line.text(),
                &reason,
            ),
            row(SID, "fact-intent@v1", "waiting", "", &reason),
            row(SID, "fact-hold@v1", "typed", line.text(), &reason),
        ],
        messages: vec![
            "OK 3".to_string(),
            message(
                1,
                &format!("harness.supervisor.{SID}"),
                "Tab 1: an earlier note",
            ),
            message(
                2,
                "harness.supervisor.s-another",
                "Tab 2: another tab's note",
            ),
            message(
                3,
                &format!("harness.supervisor.{SID}"),
                &format!("Tab 1: {}", line.text()),
            ),
        ],
        after: vec![workflow_screen()],
    }
}

fn judged(p: &Parts) -> Vec<String> {
    judge(&Recording {
        sid: SID,
        status: &p.status,
        journal: &p.journal,
        ledger: &p.ledger.join("\n"),
        messages: &p.messages.join("\n"),
        after: &p.after,
    })
}

/// THE JUDGE PASSES THE RUN THE SCENARIO MEANS ([module docs](super)): the line typed once at the
/// break, fenced, past the nudge's wait, its intent before it in one episode, its record in
/// Messages, the hold declared after it, the workflow still drawn; other sessions' rows and
/// records and the policy's own rows are not the door's.
#[test]
fn the_judge_passes_the_run_the_scenario_means() {
    assert_eq!(judged(&passing()), Vec::<String>::new());
}

/// One change of the passing recording, named.
type Case = (&'static str, Box<dyn Fn(&mut Parts)>);

fn case(what: &'static str, change: impl Fn(&mut Parts) + 'static) -> Case {
    (what, Box::new(change))
}

/// EACH WAY THE RUN CAN FALL SHORT FAILS IT, alone: NEGATIVE CONTROLS of the passing recording,
/// one part changed at a time, each a failure of its own.
#[test]
fn each_way_the_run_falls_short_fails_it_alone() {
    let reason = format!("rule=wait-nudge@v1 episode={EPISODE} held_ms=61000");
    let typed = row(
        SID,
        "fact-hold@v1",
        "typed",
        templates::wait_nudge(61_000).text(),
        &reason,
    );
    let again = typed.clone();
    let cases = [
        case("observe", |p| {
            p.status = p.status.replace("occupancy=prompt", "occupancy=observe");
        }),
        case("a would-be row", |p| {
            p.ledger.push(row(
                SID,
                "fact-would@v1",
                "waiting",
                "",
                "rule=wait-nudge@v1",
            ));
        }),
        case("a skip", |p| {
            let skip = row(
                SID,
                "fact-hold@v1",
                "skipped",
                "",
                "rule=wait-nudge@v1 why=x",
            );
            p.ledger.push(skip);
        }),
        case("no typed row", |p| {
            p.ledger.pop();
        }),
        case("a second typed row", move |p| p.ledger.push(again.clone())),
        case("the intent after the line", move |p| {
            p.ledger.pop();
            p.ledger.insert(2, typed.clone());
        }),
        case("two episodes", |p| {
            p.ledger[2] = p.ledger[2].replace(EPISODE, "00000000000000cd");
        }),
        case("no episode identity", |p| {
            let token = format!("episode={EPISODE} ");
            for row in p.ledger.iter_mut().chain(&mut p.journal) {
                *row = row.replace(&token, "");
            }
        }),
        case("empty episode identity", |p| {
            for row in p.ledger.iter_mut().chain(&mut p.journal) {
                *row = row.replace(EPISODE, "");
            }
        }),
        case("malformed episode identity", |p| {
            for row in p.ledger.iter_mut().chain(&mut p.journal) {
                *row = row.replace(EPISODE, "00000000000000gg");
            }
        }),
        case("another line typed", |p| {
            p.ledger[3] = p.ledger[3].replace("1 min", "2 min");
        }),
        case("two typed journal lines", |p| {
            let again = p.journal[2].clone();
            p.journal.push(again);
        }),
        case("a would-be journal line", |p| {
            let would = p.journal[2].replace("door=break typed", "door=break would");
            p.journal.insert(1, would);
        }),
        case("the journal's other episode", |p| {
            p.journal[2] = p.journal[2].replace(EPISODE, "00000000000000cd");
        }),
        case("no hold open", |p| {
            p.journal.remove(1);
        }),
        case("no hold declared", |p| {
            p.journal.pop();
        }),
        case("the hold closed", |p| {
            p.journal
                .push("OCCUPANCY seq=12 hold closed why=gone held_s=80".into());
        }),
        case("no workflow drawn", |p| p.after = vec![done_screen()]),
        case("held under the nudge's wait", |p| {
            let short = templates::wait_nudge(59_000);
            p.ledger[3] = row(
                SID,
                "fact-hold@v1",
                "typed",
                short.text(),
                &format!("rule=wait-nudge@v1 episode={EPISODE} held_ms=59000"),
            );
            p.messages[3] = message(
                3,
                &format!("harness.supervisor.{SID}"),
                &format!("Tab 1: {}", short.text()),
            );
        }),
        case("no record of the line", |p| {
            p.messages.pop();
        }),
        case("only another tab's record of the line", |p| {
            let line = templates::wait_nudge(61_000);
            p.messages[3] = message(
                3,
                "harness.supervisor.s-another",
                &format!("Tab 2: {}", line.text()),
            );
        }),
        case("no screen after", |p| p.after.clear()),
    ];
    for (what, change) in cases {
        let mut p = passing();
        change(&mut p);
        let fail = judged(&p);
        assert_eq!(fail.len(), 1, "{what}: {fail:#?}");
    }
}
