// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `occupancy-wake-claude`: all of it but the run, which needs a real Claude Code.

use std::path::Path;
use std::time::Duration;

use aterm_agent::supervise::occupancy::door::{self, Told};
use aterm_agent::supervise::occupancy::wakes::{Atom, SourceAtom, Token, WakeReq};
use aterm_agent::supervise::{Autopilot, Occupancy, SupervisorConfig};

use super::*;
use crate::scn::{Plan, scenarios};
use crate::stub::{ToolTurn, tool_turn};

/// A screen fixture's rows, its `#` notes and its `==`/`exit=` capture lines left out.
fn screen(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.starts_with("# ") && !l.starts_with("== ") && !l.starts_with("exit="))
        .map(str::to_string)
        .collect()
}

fn workflow_screen() -> Vec<String> {
    screen(include_str!("../../aterm-phase/src/fixtures/wait_bg.out"))
}

fn done_screen() -> Vec<String> {
    workflow_screen()
        .into_iter()
        .filter(|l| !l.contains("dynamic workflow") && !l.contains("cache-lever-pick"))
        .collect()
}

/// A live turn: Claude Code mid-turn (`✳ …`, `esc to interrupt`).
fn busy_screen() -> Vec<String> {
    screen(include_str!(
        "../../aterm-phase/src/fixtures/claude-2.1.307-footer-busy.txt"
    ))
}

/// An idle point: Claude Code at rest with nothing of its own running.
fn idle_screen() -> Vec<String> {
    screen(include_str!(
        "../../aterm-phase/src/fixtures/claude-2.1.307-footer-auto-idle.txt"
    ))
}

#[test]
fn the_screens_read_as_the_judge_means() {
    assert!(workflow_waits(&workflow_screen()));
    assert!(background_wait(&workflow_screen()).is_some());
    assert!(!workflow_waits(&done_screen()));
    assert_eq!(background_wait(&busy_screen()), None);
    assert_ne!(worker_phase(&busy_screen()), Phase::Idle);
    assert_eq!(worker_phase(&idle_screen()), Phase::Idle);
}

#[test]
fn the_scenario_is_listed_with_its_lab() {
    let listed: Vec<_> = scenarios().into_iter().filter(|s| s.name == NAME).collect();
    let [s] = listed.as_slice() else {
        panic!("{} scenarios named {NAME}", listed.len());
    };
    assert_eq!(s.harness_keys, HARNESS_KEYS);
    assert_eq!(
        s.kw.instance_env, "",
        "no seam: the wake is told at its own pace"
    );
    assert!(matches!(s.plan, Plan::OccupancyWake), "{:?}", s.plan);
    assert!(
        scenarios()
            .iter()
            .filter(|s| s.name != NAME && s.name != STANDIN)
            .all(|s| !matches!(s.plan, Plan::OccupancyWake)),
        "no other scenario plays it"
    );
}

/// THE STAND-IN'S SCENARIO IS THE SAME PLAY IN THE SAME LAB with the lab's program as its agent,
/// and only it asks for that. NEGATIVE CONTROL: the real scenario's lab runs a real agent.
#[test]
fn the_stand_ins_scenario_is_the_same_lab_with_no_real_agent() {
    let all = scenarios();
    let named = |name| {
        all.iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} is listed"))
    };
    let (real, standin) = (named(NAME), named(STANDIN));
    assert!(standin.kw.standin);
    assert!(!real.kw.standin);
    assert_eq!(standin.harness_keys, real.harness_keys);
    assert!(matches!(standin.plan, Plan::OccupancyWake));
    let asks: Vec<_> = all
        .iter()
        .filter(|s| s.kw.standin)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(asks, [STANDIN]);
}

/// A STAND-IN'S RUN IS NO CAPTURE: however well it went, its tape is not promoted. NEGATIVE
/// CONTROL: the same summary without the stand-in's key is refused nothing.
#[test]
fn a_stand_ins_run_is_never_promoted() {
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let summary = dir.path().join("summary.json");
    let passed = J::obj([("occupancy_judged", J::s("pass"))]);
    std::fs::write(&summary, passed.dump(COMPACT)).expect("its summary");
    assert_eq!(crate::tape::summary_refusal(dir.path()), Ok(()));
    let mut standin = passed;
    standin.set(crate::occupancy_standin::SUMMARY_KEY, J::Bool(true));
    std::fs::write(&summary, standin.dump(COMPACT)).expect("its summary");
    assert!(crate::tape::summary_refusal(dir.path()).is_err());
}

/// ITS LAB GRANTS `prompt` UNDER CRUISE WITH THE POLICY'S CONTINUATIONS OFF, in the scratch
/// home's `aterm.toml` alone. NEGATIVE CONTROL: the lab's plain toml grants nothing.
#[test]
fn its_scratch_aterm_toml_grants_prompt_under_cruise_with_continue_off() {
    let (cfg, notes) = SupervisorConfig::from_aterm_toml(&crate::lab::scratch_aterm_toml(
        "",
        HARNESS_KEYS,
        None,
        false,
    ));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(cfg.occupancy, Occupancy::Prompt);
    assert_eq!(cfg.autopilot, Autopilot::Cruise);
    assert!(!cfg.continue_policy);
    assert_eq!(cfg.human_grace_s, 5);
    let (plain, _) =
        SupervisorConfig::from_aterm_toml(&crate::lab::scratch_aterm_toml("", "", None, false));
    assert_ne!(plain.occupancy, Occupancy::Prompt);
    assert!(plain.continue_policy);
}

/// THE AGENT'S DECLARATION IS A WAKE THE BOOK TAKES: once its shell strips the quotes, its words
/// parse as one file atom under the home's `scratch.noindex`, the run's flag, labelled with the
/// plant. NEGATIVE CONTROL: the flag's path unquoted with a space in it is no declaration.
#[test]
fn the_agents_declaration_is_one_file_atom_on_the_runs_flag() {
    let run = format!("{NAME}-01");
    let command = declare_command(&run);
    let words = command
        .strip_prefix("aterm ctl @self wake ")
        .expect("the agent's own wake")
        .replace('\'', "");
    let req = WakeReq::parse(&words).expect("a declaration");
    assert_eq!(Some(req.label), Token::parse(LABEL));
    let [
        Atom::Source(SourceAtom::File {
            path,
            pattern: None,
        }),
    ] = req.when.as_slice()
    else {
        panic!("{:?}", req.when);
    };
    assert!(path.home_relative());
    assert_eq!(path.as_str(), format!("{}/{FLAG}", flag_dir(&run)));
    assert!(path.as_str().starts_with("scratch.noindex/"));
    assert_eq!(req.need, None);
    assert!(req.unless.is_empty());
    assert!(WakeReq::parse(&words.replace(FLAG, "a flag")).is_err());
}

/// THE LAB TOUCHES ITS FLAG THROUGH NO LINK (the 2.10 safety review's F1): the agent under test
/// can write in the flag's folder, and the lab writes there as its owner. A link the agent left at
/// the flag's name is replaced by the flag and its target kept; a link on the way to the folder
/// refuses both the laying and the touch, and nothing is made or emptied where it points.
/// NEGATIVE CONTROL: a plain write through the same link empties its target.
#[test]
fn the_flag_is_touched_through_no_link() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch folder");
    let root = std::fs::canonicalize(tmp.path()).expect("its real path");
    let dir = root.join("home/scratch.noindex/run");
    clear_flag(&dir).expect("laid");
    assert!(std::fs::symlink_metadata(&dir).expect("made").is_dir());
    let flag = dir.join(FLAG);
    let kept = root.join("kept");
    std::fs::write(&kept, "the owner's").expect("a file the owner keeps");
    std::os::unix::fs::symlink(&kept, &flag).expect("a planted link");
    touch_flag(&dir).expect("touched");
    assert_eq!(std::fs::read_to_string(&kept).expect("kept"), "the owner's");
    let meta = std::fs::symlink_metadata(&flag).expect("the flag");
    assert!(meta.is_file() && meta.len() == 0, "{meta:?}");
    std::fs::remove_file(&flag).expect("the flag taken away");
    std::os::unix::fs::symlink(&kept, &flag).expect("planted again");
    clear_flag(&dir).expect("cleared");
    assert!(
        std::fs::symlink_metadata(&flag).is_err(),
        "the link is gone"
    );
    assert_eq!(std::fs::read_to_string(&kept).expect("kept"), "the owner's");

    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("a folder the owner keeps");
    std::fs::write(elsewhere.join(FLAG), "the owner's").expect("its file");
    let via = root.join("home/scratch.noindex/via");
    std::os::unix::fs::symlink(&elsewhere, &via).expect("a planted folder link");
    assert!(clear_flag(&via).is_err());
    assert!(touch_flag(&via).is_err());
    assert!(clear_flag(&via.join("deeper")).is_err());
    assert_eq!(
        std::fs::read_to_string(elsewhere.join(FLAG)).expect("kept"),
        "the owner's"
    );
    assert!(!elsewhere.join("deeper").exists());

    std::fs::write(via.join(FLAG), b"").expect("a plain write follows the link");
    assert_eq!(
        std::fs::read_to_string(elsewhere.join(FLAG)).expect("emptied"),
        ""
    );
}

/// THE HANG DETECTOR STANDS PAST THE CONDITION, K LOOKS AND A MINUTE: a look reads the file's
/// level, K looks are how long an untold wake may wait for a door, and the minute is the hang
/// detector's own.
#[test]
fn the_line_is_waited_for_past_k_looks_and_a_minute() {
    let look_s = Duration::from_millis(u64::try_from(LOOK_EVERY_MS).expect("a look")).as_secs_f64();
    let k = f64::from(u32::try_from(K_LOOKS).expect("K looks"));
    assert!(told_within_s() >= look_s * (k + 1.0) + 60.0);
    assert!(told_within_s() > WAITS_WITHIN_S);
}

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

#[test]
fn the_stub_plays_the_agent_from_the_script() {
    let run = format!("{NAME}-03");
    let script = J::parse(&script(&run).dump(COMPACT)).expect("the script is JSON");
    let turn = |messages: Vec<J>| tool_turn(&body(messages), &script);
    let Some(ToolTurn::Call(name, input, 0)) = turn(vec![user(TURN)]) else {
        panic!("{:?}", turn(vec![user(TURN)]));
    };
    assert_eq!(name, "Bash");
    assert_eq!(
        input.get("command").and_then(J::as_str),
        Some(declare_command(&run).as_str())
    );
    let declared = vec![user(TURN), called("Bash"), result("OK wake #1")];
    let Some(ToolTurn::Call(name, input, 1)) = turn(declared.clone()) else {
        panic!("{:?}", turn(declared));
    };
    assert_eq!(name, "Workflow");
    assert_eq!(input.get("script").and_then(J::as_str), Some(WORKFLOW));
    let mut started = declared;
    started.extend([called("Workflow"), result("Workflow started")]);
    assert_eq!(
        turn(started.clone()),
        Some(ToolTurn::Say(STARTED.into(), 2))
    );
    let told = templates::wake(1, Declarer::Agent, &[AtomKind::File], Some((23, 40)), None);
    let mut answered = started;
    answered.extend([said(STARTED), user(told.text())]);
    assert_eq!(turn(answered), Some(ToolTurn::Say(ANSWERED.into(), 3)));

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
    assert!(
        !WORKFLOW.contains(crate::occupancy_break::STEP_MARK),
        "its steps are not the break scenario's"
    );
}

#[test]
fn its_run_promotes_under_its_name_unless_the_judge_failed() {
    let run = Path::new("/results").join(format!("{NAME}-01"));
    assert_eq!(crate::tape::scenario_of(&run).as_deref(), Some(NAME));
    assert!(crate::tape::corpus_name(NAME));
    let passed = J::obj([("occupancy_judged", J::s("pass"))]);
    assert_eq!(crate::tape::run_failed(&passed), None);
    let failed = J::obj([
        (
            "occupancy_judged",
            J::Arr(vec![J::s("wake #1 is not told")]),
        ),
        ("error", J::s("wake #1 is not told")),
    ]);
    assert!(crate::tape::run_failed(&failed).is_some());
}

fn frame(wall: f64, rows: Vec<String>, i: usize) -> Frame {
    Frame {
        wall,
        t: None,
        seq: i64::try_from(i).expect("a seq"),
        rows,
        i,
    }
}

/// THE SCREEN THE LINE WAS TYPED INTO is the last read before the typed line was seen that draws
/// no harness line yet, however long ago it came. NEGATIVE CONTROL: before any read there is none.
#[test]
fn the_screen_at_the_write_is_the_last_before_the_line_drew() {
    let mut drawn = workflow_screen();
    drawn.push(format!("❯ {HARNESS_MARK} aterm: wake #1 (yours): its file"));
    let frames = [
        frame(1.0, idle_screen(), 0),
        frame(2.0, workflow_screen(), 1),
        frame(3.0, busy_screen(), 2),
        frame(4.0, drawn, 3),
        frame(6.0, idle_screen(), 4),
    ];
    assert_eq!(before_line(&frames, 5.0), Some(busy_screen()));
    assert_eq!(before_line(&frames, 3.0), Some(workflow_screen()));
    assert_eq!(before_line(&frames, 0.5), None);
}

const SID: &str = "s-0123456789abcdef0123";
const EPISODE: u64 = 0xab;
const HELD_MS: i64 = 75_000;

fn run() -> String {
    format!("{NAME}-02")
}

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

fn told() -> templates::FactLine {
    templates::wake(1, Declarer::Agent, &[AtomKind::File], Some((23, 40)), None)
}

/// A recording that passes, in parts the negative controls change one at a time.
#[derive(Clone)]
struct Parts {
    status: String,
    waits_before: String,
    waits_after: String,
    journal: Vec<String>,
    ledger: Vec<String>,
    messages: Vec<String>,
    at_write: Option<Vec<String>>,
    after: Vec<Vec<String>>,
}

fn waits(state: &str) -> String {
    format!(
        "#1 {state} prio=normal kinds=file declared=1000 by=86401000 since={}",
        if state == "pending" { "-" } else { "61000" }
    )
}

/// The run at a break: the wake's intent, its line typed alone, the record and the book's `told`.
fn passing() -> Parts {
    let typed = door::wake_outcome_reason(1, EPISODE, HELD_MS, WakeDoor::Break, Told::Typed);
    let intent = door::wake_intent_reason(1, EPISODE, HELD_MS, WakeDoor::Break);
    Parts {
        status: "OK 1 agent=claude occupancy=prompt occupancy_hold=open waits=1 waits_ready=0"
            .to_string(),
        waits_before: waits("pending"),
        waits_after: waits("told"),
        journal: [
            "EVENT idle".to_string(),
            "OCCUPANCY seq=4 hold open".to_string(),
            format!(
                "OCCUPANCY seq=9 rule=wake@v1 rev=builtin episode={EPISODE:016x} door=break typed"
            ),
        ]
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
                SID,
                "approve@v1",
                "approved",
                &declare_command(&run()),
                "an approval's own row",
            ),
            row("s-another", WOULD_RULE, "waiting", told().text(), &typed),
            row(SID, INTENT_RULE, "waiting", "", &intent),
            row(SID, FactKind::Wake.rule(), "typed", told().text(), &typed),
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
                &format!("Tab 1: {}", told().text()),
            ),
        ],
        at_write: Some(workflow_screen()),
        after: vec![workflow_screen()],
    }
}

/// The run at an idle point: the wake's fact rode the policy's act, typed into an idle screen.
fn passing_at_a_point() -> Parts {
    let mut p = passing();
    let typed = door::wake_outcome_reason(1, EPISODE, HELD_MS, WakeDoor::Point, Told::Typed);
    let intent = door::wake_intent_reason(1, EPISODE, HELD_MS, WakeDoor::Point);
    p.ledger[3] = row(SID, INTENT_RULE, "waiting", "", &intent);
    p.ledger[4] = row(SID, FactKind::Wake.rule(), "typed", told().fact(), &typed);
    p.journal = vec![
        "EVENT idle".to_string(),
        format!("FACT seq=9 kind=wake episode={EPISODE:016x} ord=1 door=point typed"),
    ];
    p.at_write = Some(idle_screen());
    p.after = vec![idle_screen()];
    p
}

fn judged(p: &Parts) -> Vec<String> {
    judge(&Recording {
        sid: SID,
        run: &run(),
        status: &p.status,
        waits_before: &p.waits_before,
        waits_after: &p.waits_after,
        journal: &p.journal,
        ledger: &p.ledger.join("\n"),
        messages: &p.messages.join("\n"),
        at_write: p.at_write.as_deref(),
        after: &p.after,
    })
}

#[test]
fn the_judge_passes_the_run_the_scenario_means() {
    assert_eq!(judged(&passing()), Vec::<String>::new());
    assert_eq!(judged(&passing_at_a_point()), Vec::<String>::new());
    let mut headed = passing();
    headed.waits_after = format!("OK 1\n{}", waits("told"));
    assert_eq!(judged(&headed), Vec::<String>::new());
}

/// THE LINE TYPED INTO A LIVE TURN FAILS THE RUN, at either door: the agent was mid-turn
/// (`✳ …`, `esc to interrupt`), neither at its background break nor at rest.
#[test]
fn a_line_typed_into_a_live_turn_fails_the_run() {
    for mut p in [passing(), passing_at_a_point()] {
        p.at_write = Some(busy_screen());
        let fail = judged(&p);
        assert!(
            matches!(fail.as_slice(), [one] if one.contains("live turn")),
            "{fail:#?}"
        );
    }
}

type Case = (&'static str, Box<dyn Fn(&mut Parts)>);

fn case(what: &'static str, change: impl Fn(&mut Parts) + 'static) -> Case {
    (what, Box::new(change))
}

#[test]
fn each_way_the_run_falls_short_fails_it_alone() {
    let ep = format!("{EPISODE:016x}");
    let cases = [
        case("observe", |p| {
            p.status = p.status.replace("occupancy=prompt", "occupancy=observe");
        }),
        case("not pending before the flag", |p| {
            p.waits_before = waits("ready")
        }),
        case("no wake before the flag", |p| p.waits_before.clear()),
        case("not told after", |p| p.waits_after = waits("answering")),
        case("a would-be row", |p| {
            let would = row(SID, WOULD_RULE, "waiting", "", "rule=wake@v1");
            p.ledger.push(would);
        }),
        case("a skip", |p| {
            let skip = row(
                SID,
                FactKind::Wake.rule(),
                "skipped",
                "",
                "rule=wake@v1 why=x",
            );
            p.ledger.push(skip);
        }),
        case("no typed row", |p| {
            p.ledger.pop();
        }),
        case("a second typed row", |p| {
            let again = p.ledger[4].clone();
            p.ledger.push(again);
        }),
        case("the intent after the line", |p| p.ledger.swap(3, 4)),
        case("two episodes", |p| {
            p.ledger[3] =
                p.ledger[3].replace("episode=00000000000000ab", "episode=00000000000000cd");
        }),
        case("malformed episode identity", move |p| {
            for row in p.ledger.iter_mut().chain(&mut p.journal) {
                *row = row.replace("00000000000000ab", "00000000000000gg");
            }
        }),
        case("another wake's line", |p| {
            let other =
                templates::wake(2, Declarer::Agent, &[AtomKind::File], Some((23, 40)), None);
            p.ledger[4] = p.ledger[4].replace(told().text(), other.text());
        }),
        case("another ordinal", |p| {
            p.ledger[4] = p.ledger[4].replace(" ord=1 ", " ord=2 ");
        }),
        case("two doors", |p| {
            p.ledger[3] = p.ledger[3].replace("door=break", "door=point");
        }),
        case("the plant in a typed row", |p| {
            p.ledger[0] = p.ledger[0].replace("\"continue\"", &format!("\"continue {LABEL}\""));
        }),
        case("the flag's path in a record", |p| {
            let path = flag_dir(&run());
            p.messages.push(message(
                4,
                &format!("harness.supervisor.{SID}"),
                &format!("Tab 1: {path}"),
            ));
        }),
        case("no record of the line", |p| {
            p.messages.pop();
        }),
        case("only another tab's record of the line", |p| {
            p.messages[3] = message(
                3,
                "harness.supervisor.s-another",
                &format!("Tab 2: {}", told().text()),
            );
        }),
        case("two typed journal lines", |p| {
            let again = p.journal[2].clone();
            p.journal.push(again);
        }),
        case("no typed journal line", |p| {
            p.journal.pop();
        }),
        case("the journal's other episode", move |p| {
            p.journal[2] = p.journal[2].replace(&ep, "00000000000000cd");
        }),
        case("the journal's other door", |p| {
            p.journal[2] =
                format!("FACT seq=9 kind=wake episode={EPISODE:016x} ord=1 door=point typed");
        }),
        case("no hold open", |p| {
            p.journal.remove(1);
        }),
        case("no screen at the write", |p| p.at_write = None),
        case("typed into a live turn", |p| {
            p.at_write = Some(busy_screen())
        }),
        case("no workflow drawn after", |p| p.after = vec![done_screen()]),
        case("no screen after", |p| p.after.clear()),
    ];
    for (what, change) in cases {
        let mut p = passing();
        change(&mut p);
        let fail = judged(&p);
        assert_eq!(fail.len(), 1, "{what}: {fail:#?}");
    }
}

/// AT A POINT THE FACT RIDES AN ACT: its row books the fact alone, never the whole break line, and
/// a break's line booked whole is never its fact alone. NEGATIVE CONTROL: each door's own
/// recording passes ([`the_judge_passes_the_run_the_scenario_means`]).
#[test]
fn each_door_books_its_own_text() {
    let mut whole = passing_at_a_point();
    whole.ledger[4] = whole.ledger[4].replace(told().fact(), told().text());
    let mut bare = passing();
    bare.ledger[4] = bare.ledger[4].replace(told().text(), told().fact());
    for p in [whole, bare] {
        let fail = judged(&p);
        assert!(
            matches!(fail.as_slice(), [one] if one.contains("closed line")),
            "{fail:#?}"
        );
    }
}
