// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_ledgers_rows_are_read_by_their_rule() {
    let text = concat!(
        "{\"kind\":\"removed\",\"tier\":4}\n",
        "{\"ts\":\"t1\",\"kind\":\"free-ask\",\"said\":\"elected\",\"sid\":\"s-1\",\"free_bytes\":5}\n",
        "{\"ts\":\"t2\",\"kind\":\"free-ask\",\"said\":\"step\",\"sid\":\"s-1\",\"step\":\"typed\"}\n",
        "{\"ts\":\"t3\",\"kind\":\"free-ask\",\"said\":\"handoff\",\"sid\":\"s-1\",\"why\":\"nothing-freed\"}\n",
    );
    let rows = ask_rows(text);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        (rows[0].said.as_str(), rows[0].sid.as_str(), rows[0].free),
        ("elected", "s-1", Some(5))
    );
    assert_eq!(
        (rows[1].said.as_str(), rows[1].step.as_str()),
        ("step", "typed")
    );
    assert_eq!(
        (rows[2].said.as_str(), rows[2].why.as_str()),
        ("handoff", Miss::NothingFreed.word())
    );
    assert_eq!(count(&rows, "step", "typed"), 1);
    assert_eq!(count(&rows, "handoff", ""), 1);
    assert_eq!(count(&rows, "step", ""), 0);
}

fn tab(name: &'static str) -> Tab {
    Tab {
        name,
        gib: 1,
        token: format!("LABDISK-{name}-1"),
        dir: PathBuf::from(format!("/w/{name}")),
        sid: String::new(),
        silent: false,
        draft: None,
    }
}

fn user(text: &str) -> crate::workers::Row {
    crate::workers::Row {
        kind: "user".to_owned(),
        text: text.to_owned(),
        ..Default::default()
    }
}

/// What each tab received is read off every transcript: a line in a conversation no tab's
/// token names counts, with its file, and a tab with no transcript is unread, never a tab
/// that received nothing. Before this a line typed into a conversation the lane could not
/// match to its tab (a new session file, a first row that is not the task) went uncounted,
/// and "exactly one tab received the line" held although two had.
#[test]
fn every_transcripts_lines_are_counted_and_an_unmatched_tab_is_unread() {
    let tabs = [tab("alpha"), tab("beta"), tab("gamma")];
    let line = "[aterm harness] aterm: ... Only this tab was asked. (rule disk-free@v1)";
    let result = crate::workers::Row {
        kind: "user".to_owned(),
        text: "removed (rule disk-free@v1)".to_owned(),
        result: Some("removed (rule disk-free@v1)".to_owned()),
        ..Default::default()
    };
    let all = vec![
        (
            PathBuf::from("/p/a.jsonl"),
            vec![
                user("LABDISK-alpha-1: keep this checkout building"),
                user("continue"),
                user(line),
                result,
            ],
        ),
        (
            PathBuf::from("/p/b.jsonl"),
            vec![user("LABDISK-beta-1: keep this checkout building")],
        ),
    ];
    let got = received_in(&all, &tabs);
    assert_eq!(got.total(), 1, "a tool's result is no line: {got:?}");
    assert_eq!(got.counts(), [1, 0, 0]);
    assert_eq!(got.unread(), ["gamma"], "no transcript of gamma's");
    assert_eq!(got.first(), Some(line));
    assert!(
        !got.exactly(&[1, 0, 0]),
        "an unread tab is not one that received nothing"
    );
    let mut all = all;
    all.push((
        PathBuf::from("/p/g.jsonl"),
        vec![user("LABDISK-gamma-1: keep this checkout building")],
    ));
    let got = received_in(&all, &tabs);
    assert!(got.exactly(&[1, 0, 0]), "{got:?}");
    assert_eq!(got.first_of(0), Some(line));
    assert_eq!(got.first_of(1), None);
    all.push((
        PathBuf::from("/p/c.jsonl"),
        vec![user("resumed"), user(line)],
    ));
    let got = received_in(&all, &tabs);
    assert_eq!(got.total(), 2, "{got:?}");
    assert_eq!(got.counts(), [1, 0, 0]);
    assert_eq!(got.stray, [(PathBuf::from("/p/c.jsonl"), line.to_owned())]);
    assert!(!got.exactly(&[1, 0, 0]), "a stray line fails: {got:?}");
    // A tab's second transcript counts for it.
    all.push((
        PathBuf::from("/p/a2.jsonl"),
        vec![user("LABDISK-alpha-1: again"), user(line)],
    ));
    let got = received_in(&all, &tabs);
    assert_eq!(got.tabs[0].1.as_ref().map(Vec::len), Some(2), "{got:?}");
    assert_eq!(got.counts(), [2, 0, 0]);
}

/// aterm's disk worker is no tab: its task ends `(rule disk-free@v1 worker)`, which neither
/// counts as a line nor draws the stub's cleanup. Before this the lane matched the rule's bare
/// name, so a worker that started would have read as a line in a conversation of no tab's.
#[test]
fn the_disk_workers_task_is_no_line() {
    let tabs = [tab("alpha"), tab("beta"), tab("gamma")];
    let task =
        "Free this volume ... no NEEDS YOU line: aterm tells the owner. (rule disk-free@v1 worker)";
    let all = vec![
        (
            PathBuf::from("/p/a.jsonl"),
            vec![user("LABDISK-alpha-1: keep this checkout building")],
        ),
        (
            PathBuf::from("/p/b.jsonl"),
            vec![user("LABDISK-beta-1: keep this checkout building")],
        ),
        (
            PathBuf::from("/p/g.jsonl"),
            vec![user("LABDISK-gamma-1: keep this checkout building")],
        ),
        (PathBuf::from("/p/w.jsonl"), vec![user(task)]),
    ];
    let got = received_in(&all, &tabs);
    assert!(
        got.exactly(&[0, 0, 0]),
        "the worker's own conversation: {got:?}"
    );
    // The worker's task typed into a tab's conversation is that tab asked again, though it is
    // no line: before this the lane matched only the line's tag and passed it.
    let mut spammed = all.clone();
    spammed[0].1.push(user(task));
    let got = received_in(&spammed, &tabs);
    assert_eq!(got.counts(), [0, 0, 0], "no line: {got:?}");
    assert_eq!(
        got.worker_in_tab,
        [("alpha".to_owned(), PathBuf::from("/p/a.jsonl"))]
    );
    assert!(!got.exactly(&[0, 0, 0]), "{got:?}");
    let picked = crate::stub::with_reply(stub_mode(&tabs, None), &users(&[task]));
    assert_eq!(picked.get("text").and_then(J::as_str), Some(WORKING));
    assert!(crate::stub::scripted_call(&picked, false).is_none());
    let rows = ask_rows(concat!(
        "{\"ts\":\"t1\",\"kind\":\"free-ask\",\"said\":\"worker\",\"sid\":\"\"}\n",
        "{\"ts\":\"t2\",\"kind\":\"free-ask\",\"said\":\"worker-started\",\"sid\":\"s-w\"}\n",
        "{\"ts\":\"t3\",\"kind\":\"free-ask\",\"said\":\"worker-over\",\"sid\":\"s-w\",\"why\":\"failed:could-not-finish\"}\n",
    ));
    assert_eq!(count(&rows, "worker", ""), 1, "only the claim");
    assert_eq!(
        (rows[2].said.as_str(), rows[2].why.as_str()),
        ("worker-over", "failed:could-not-finish")
    );
    assert_eq!(worker_sids(&rows, 0), ["s-w"]);
    assert!(worker_sids(&rows, 3).is_empty());
}

/// The disk worker runs in a session of its own: one the ledger's worker rows name that is a
/// tab's is found, whichever worker row names it.
#[test]
fn a_worker_in_a_tabs_session_is_found() {
    let mut tabs = [tab("alpha"), tab("beta"), tab("gamma")];
    for (t, sid) in tabs.iter_mut().zip(["s-a", "s-b", "s-g"]) {
        t.sid = sid.to_owned();
    }
    let own = ask_rows(concat!(
        "{\"kind\":\"free-ask\",\"said\":\"elected\",\"sid\":\"s-a\"}\n",
        "{\"kind\":\"free-ask\",\"said\":\"worker\",\"sid\":\"\"}\n",
        "{\"kind\":\"free-ask\",\"said\":\"worker-started\",\"sid\":\"s-w\"}\n",
        "{\"kind\":\"free-ask\",\"said\":\"worker-runs\",\"sid\":\"s-w\"}\n",
        "{\"kind\":\"free-ask\",\"said\":\"worker-over\",\"sid\":\"s-w\"}\n",
    ));
    assert_eq!(worker_sids(&own, 0), ["s-w"], "an elected tab is no worker");
    assert!(tabs_among(&worker_sids(&own, 0), &tabs).is_empty());
    for said in ["worker-started", "worker-runs", "worker-over"] {
        let rows = ask_rows(&format!(
            "{{\"kind\":\"free-ask\",\"said\":\"{said}\",\"sid\":\"s-b\"}}\n"
        ));
        assert_eq!(
            tabs_among(&worker_sids(&rows, 0), &tabs),
            [("beta", "s-b")],
            "{said}"
        );
    }
}

#[test]
fn the_asked_tabs_own_build_is_read_by_its_mark() {
    let own = "Largest left: ~/work/alpha's builds 7.0 GiB (this tab's). Free what";
    assert!(marks_own(own, "alpha"));
    assert!(!marks_own(own, "beta"));
    let other =
        "Largest left: ~/work/alpha's builds 7.0 GiB; ~/work/beta's builds 2.0 GiB (this tab's).";
    assert!(!marks_own(other, "alpha"), "the mark is beta's");
    assert!(marks_own(other, "beta"));
    let shared = "Largest left: ~/work/alpha's builds 7.0 GiB (this tab's checkout, shared).";
    assert!(!marks_own(shared, "alpha"), "shared is not its own alone");
    let pointer = "`aterm harness disk` lists the largest holders left.";
    assert!(!marks_own(pointer, "alpha"));
}

fn users(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn the_stub_answers_only_the_disk_line_with_the_cleanup() {
    let spec = stub_mode(&[tab("alpha"), tab("beta"), tab("gamma")], None);
    let task = users(&["LABDISK-alpha-1: keep this checkout building"]);
    let picked = crate::stub::with_reply(spec.clone(), &task);
    assert_eq!(picked.get("text").and_then(J::as_str), Some(WORKING));
    assert!(crate::stub::scripted_call(&picked, false).is_none());
    let asked = users(&[
        "LABDISK-alpha-1: keep this checkout building",
        "continue",
        "[aterm] aterm: ... Only this tab was asked. (rule disk-free@v1)",
    ]);
    let picked = crate::stub::with_reply(spec.clone(), &asked);
    let (name, input) = crate::stub::scripted_call(&picked, false).expect("the call");
    assert_eq!(name, "Bash");
    assert_eq!(input.get("command").and_then(J::as_str), Some(CLEANUP));
    assert!(
        crate::stub::scripted_call(&picked, true).is_none(),
        "its result back"
    );
    assert_eq!(picked.get("text").and_then(J::as_str), Some(REMOVED));
    let after = users(&[
        "LABDISK-alpha-1: keep this checkout building",
        "[aterm] aterm: ... (rule disk-free@v1)",
        "continue",
    ]);
    let picked = crate::stub::with_reply(spec, &after);
    assert_eq!(picked.get("text").and_then(J::as_str), Some(WORKING));
}

/// A silent tab, told apart by its token in its conversation's prompts, answers the disk line
/// with words alone; every other tab answers it with the cleanup.
#[test]
fn a_silent_tab_answers_the_disk_line_with_words_alone() {
    let mut alpha = tab("alpha");
    alpha.silent = true;
    let spec = stub_mode(&[alpha, tab("beta"), tab("gamma")], None);
    let line = "[aterm] aterm: ... Only this tab was asked. (rule disk-free@v1)";
    let silent = users(&["LABDISK-alpha-1: keep this checkout building", line]);
    let picked = crate::stub::with_reply(spec.clone(), &silent);
    assert_eq!(picked.get("text").and_then(J::as_str), Some(DECLINED));
    assert!(crate::stub::scripted_call(&picked, false).is_none());
    let asked = users(&["LABDISK-beta-1: keep this checkout building", line]);
    let picked = crate::stub::with_reply(spec.clone(), &asked);
    let (_, input) = crate::stub::scripted_call(&picked, false).expect("beta's call");
    assert_eq!(input.get("command").and_then(J::as_str), Some(CLEANUP));
    let task = users(&["LABDISK-alpha-1: keep this checkout building", "continue"]);
    let picked = crate::stub::with_reply(spec, &task);
    assert_eq!(picked.get("text").and_then(J::as_str), Some(WORKING));
}

#[test]
fn a_scenario_is_named_by_its_word() {
    for sc in Scenario::ALL {
        assert_eq!(Scenario::named(sc.word()), Some(sc));
    }
    assert_eq!(Scenario::named("ended"), None);
    assert_eq!(
        UNANSWERED.disk_toml(),
        "[disk]\nwarn_free_gib = 9\nauto_free_gib = 8\nidle_build_dirs = true\n"
    );
    assert_eq!(PARTIAL.image(), "10g");
    assert_eq!(EXHAUSTED.disk_toml(), UNANSWERED.disk_toml());
    assert_eq!(silent(&EXHAUSTED), 2);
    assert_eq!(silent(&UNANSWERED), 1);
    assert_eq!(Scenario::named("worker"), Some(Scenario::Worker));
    assert_eq!(WORKER.image(), "11g");
    assert_eq!(WORKER.tabs.len(), 2);
    assert!(Scenario::Worker.drafted() && !Scenario::Clear.drafted());
    assert!(!Scenario::Partial.claims_worker());
    for sc in [
        Scenario::Clear,
        Scenario::Unanswered,
        Scenario::Exhausted,
        Scenario::Worker,
    ] {
        assert!(sc.claims_worker(), "{sc:?}");
    }
}

/// Each scenario's `[disk]` makes the lines the lane's sizes stand on, as the window computes
/// them on the image: the floor written, the emergency line 4 GiB, the home laid `short`.
#[test]
fn each_scenarios_disk_makes_the_lines_its_sizes_stand_on() {
    for sc in Scenario::ALL {
        let g = sc.geometry();
        let knobs = aterm_capacity::floor::Knobs::from_toml(&g.disk_toml());
        let lines = aterm_capacity::floor::lines(knobs, Some(g.image_gib * GIB), None);
        assert_eq!(lines.floor, g.floor(), "{sc:?}");
        assert_eq!(lines.emergency, EMERGENCY_FREE_GIB * GIB, "{sc:?}");
        let t = aterm_agent::harness::disk::Thresholds {
            warn: lines.warn,
            floor: lines.floor,
            target: lines.target,
            emergency: lines.emergency,
            files_emergency: lines.files_emergency,
        };
        assert_eq!(
            t.level(Some(g.laid_free), None),
            aterm_agent::harness::disk::Level::Short,
            "{sc:?}"
        );
        assert_eq!(
            t.level(Some(g.laid_free + g.cache()), None),
            aterm_agent::harness::disk::Level::Short,
            "{sc:?}: the cache freed leaves the volume short"
        );
    }
}

/// A request body of `msgs` (each a message's JSON).
fn body(msgs: &[String]) -> Vec<u8> {
    format!("{{\"messages\":[{}]}}", msgs.join(",")).into_bytes()
}

fn user_msg(text: &str) -> String {
    msg("user", text)
}

fn reply_msg(text: &str) -> String {
    msg("assistant", text)
}

fn msg(role: &str, text: &str) -> String {
    J::obj([("role", J::s(role)), ("content", J::s(text))]).dump(crate::pyjson::Style {
        indent: None,
        compact: true,
        ascii: false,
    })
}

/// A planted cache as the lane lays it under home `/h`, its outside paths the ballast and
/// beta's build.
fn planted() -> Planted {
    Planted {
        home: PathBuf::from("/h"),
        cache: PathBuf::from("/h/Library/Caches/labtool"),
        bytes: 2 * GIB,
        outside: vec![
            PathBuf::from("/h/ballast"),
            PathBuf::from("/h/work/beta/target"),
        ],
    }
}

/// The commands the stub drives aterm's disk worker through under `script`, each answered
/// with a result, and the report it then says.
fn driven(script: &J, task: &str) -> (Vec<String>, String, Vec<String>) {
    use crate::stub::{ToolTurn, keyed_tool_turn};
    let used =
        r#"{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}"#;
    let result =
        r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}"#;
    let mut msgs = vec![user_msg(task)];
    let mut commands = Vec::new();
    loop {
        match keyed_tool_turn(&body(&msgs), script) {
            Some(ToolTurn::Call(name, input, step)) => {
                assert_eq!((name.as_str(), step), ("Bash", commands.len()));
                commands.push(
                    input
                        .get("command")
                        .and_then(J::as_str)
                        .unwrap_or("")
                        .to_owned(),
                );
                msgs.extend([used.to_owned(), result.to_owned()]);
            }
            Some(ToolTurn::Say(text, _)) => return (commands, text, msgs),
            other => panic!("{other:?} after {commands:?}"),
        }
    }
}

/// aterm's disk worker is driven by its task's tag alone, and through its fence's steps. With a
/// cache planted, as a worker that skipped its task's first step: the dry run before any
/// command (which its fence must decline, no shell snapshot standing), `pwd`, the `rm -rf` of
/// the cache (which its fence must decline), the dry run again, the verb of the paths it must
/// refuse by their whole paths (which its fence must decline, drawn on two rows), then under
/// `~`, the verb's removal of the cache, then its report, said again to Autopilot Land's
/// prompt as its defense. With none, as its task asks: `pwd`, the dry run and its report. A
/// tab's conversation, the disk line in it or not, takes no step of it: it is the replies'
/// (the line's cleanup), and a tab runs no command but that.
#[test]
fn the_stub_drives_the_disk_worker_through_its_steps_and_no_tab() {
    use crate::stub::{ToolTurn, keyed_tool_turn};
    let tabs = [tab("alpha"), tab("beta")];
    let p = planted();
    let task = format!("Free this volume. {}", worker_tag());
    let land = user_msg(
        "[aterm harness] Autopilot is Land: ... defend that in a few lines starting Nothing left:",
    );
    let spec = stub_mode(&tabs, Some(&p));
    let script = spec.get("script").cloned().expect("the script");
    let (commands, report, mut msgs) = driven(&script, &task);
    assert_eq!(
        commands,
        [
            FREE_DRY,
            FIRST_BASH,
            "rm -rf /h/Library/Caches/labtool",
            FREE_DRY,
            "aterm harness disk free /h/ballast /h/work/beta/target",
            "aterm harness disk free ~/ballast ~/work/beta/target",
            "aterm harness disk free /h/Library/Caches/labtool",
        ]
    );
    assert_eq!(commands[2], rm_cache(&p.cache));
    assert_eq!(commands[5], free_in_home(&p));
    assert!(is_report(&report), "{report}");
    assert!(
        report.contains("/h/Library/Caches/labtool (2.0 GiB)"),
        "{report}"
    );
    msgs.extend([reply_msg(&report), land.clone()]);
    let defense = keyed_tool_turn(&body(&msgs), &script);
    assert!(
        matches!(&defense, Some(ToolTurn::Say(t, 8)) if *t == report),
        "Land's prompt is answered with the report: {defense:?}"
    );
    msgs.extend([reply_msg(&report), land]);
    assert_eq!(keyed_tool_turn(&body(&msgs), &script), Some(ToolTurn::Done));
    let line = user_msg("[aterm harness] ... Only this tab was asked. (rule disk-free@v1)");
    for first in [
        "LABDISK-alpha-1: keep this checkout building",
        "LABDISK-beta-1: keep this checkout building",
    ] {
        assert_eq!(
            keyed_tool_turn(&body(&[user_msg(first)]), &script),
            None,
            "a tab's task is the replies'"
        );
        assert_eq!(
            keyed_tool_turn(&body(&[user_msg(first), line.clone()]), &script),
            None,
            "a tab's line is the replies'"
        );
    }
    let picked = crate::stub::with_reply(
        spec,
        &users(&["LABDISK-alpha-1: keep", "x (rule disk-free@v1)"]),
    );
    let (_, input) = crate::stub::scripted_call(&picked, false).expect("the tab's cleanup");
    assert_eq!(input.get("command").and_then(J::as_str), Some(CLEANUP));
    // No cache: `pwd`, the dry run, then the report, which freed nothing.
    let (commands, report, _) = driven(&worker_script(None), &worker_tag());
    assert_eq!(commands, [FIRST_BASH, FREE_DRY]);
    assert!(
        is_report(&report) && report.contains("freed nothing"),
        "{report}"
    );
}

/// An approval-ledger row is read as the box of the command its subject names, alone or after
/// the box's kind, a row the box wrapped included; a command that only ends in its words is
/// not it.
#[test]
fn a_ledger_row_is_read_as_the_box_of_its_command() {
    let row = |command: &str| Decided {
        command: command.to_owned(),
        ..Default::default()
    };
    assert!(boxed(&row("bash box: rm -rf /h/c"), "rm -rf /h/c"));
    assert!(boxed(&row(FIRST_BASH), FIRST_BASH));
    assert!(
        boxed(
            &row("aterm harness disk free /h/ballast /h/work/beta/tar get"),
            "aterm harness disk free /h/ballast /h/work/beta/target"
        ),
        "a command the box wrapped"
    );
    assert!(!boxed(&row("echo pwd"), FIRST_BASH));
    assert!(!boxed(
        &row("bash box: aterm harness disk free --dry-run"),
        FIRST_BASH
    ));
    assert!(!boxed(
        &row("aterm harness disk free /h/Library/Caches/labtool"),
        FREE_DRY
    ));
}

/// The worker's report is a defense Autopilot Land lands: a `Nothing left:` reply with no
/// NEEDS YOU line and nothing Land reads as open work, freed a cache or nothing. CONTROL: a
/// report that leaves work for later is none.
#[test]
fn the_workers_report_is_a_defense_land_lands() {
    use aterm_agent::supervise::policy::turn_end::{clean_defense, land_open_found};
    for report in [worker_report(Some(&planted())), worker_report(None)] {
        assert!(
            clean_defense(Some(&report)),
            "{report}: {:?}",
            land_open_found(Some(&report))
        );
        assert!(land_open_found(Some(&report)).is_empty(), "{report}");
    }
    let open = format!("{NOTHING_LEFT} freed nothing yet; I will free the cache later.");
    assert!(!clean_defense(Some(&open)), "{open}");
}

/// A worker's report is its last reply with words, read with whatever was typed into it
/// after; a reply that lacks a line its task asks for is no report.
#[test]
fn a_workers_report_is_its_last_reply_and_what_follows_it_is_read() {
    let reply = |text: &str| crate::workers::Row {
        kind: "assistant".to_owned(),
        text: text.to_owned(),
        ts: Some(5),
        ..Default::default()
    };
    let call = crate::workers::Row {
        kind: "assistant".to_owned(),
        tool_use: Some(("Bash".to_owned(), J::obj([("command", J::s(FREE_DRY))]))),
        tool_id: "t1".to_owned(),
        ..Default::default()
    };
    let result = crate::workers::Row {
        kind: "user".to_owned(),
        result: Some("would remove /h/c (2.0 GiB)".to_owned()),
        tool_id: "t1".to_owned(),
        ..Default::default()
    };
    let said = worker_report(Some(&planted()));
    let rows = vec![user("the task"), call, result, reply(&said)];
    let (report, after) = report_of(&rows);
    assert_eq!(report.map(|r| r.text.as_str()), Some(said.as_str()));
    assert!(after.is_empty());
    assert_eq!(results_of(&rows, FREE_DRY), ["would remove /h/c (2.0 GiB)"]);
    assert!(results_of(&rows, "rm -rf /h/c").is_empty());
    let mut typed = rows.clone();
    typed.push(user("[aterm harness] Autopilot is Land: ..."));
    let (report, after) = report_of(&typed);
    assert!(report.is_some());
    assert_eq!(after.len(), 1, "the prompt typed after the report");
    assert!(is_report(&said));
    assert!(
        !is_report("Nothing left: freed it.\ndisk-free filling: ballast"),
        "no owner-only line"
    );
    assert!(
        !is_report("I freed it.\ndisk-free filling: a\ndisk-free owner-only: b"),
        "no Nothing left:"
    );
}

/// The ledger's `disk-free` rows and a session's approval ledger are read by their own fields.
#[test]
fn the_verbs_rows_and_the_approval_ledger_are_read() {
    let ledger = concat!(
        "{\"ts\":\"t1\",\"kind\":\"free-ask\",\"said\":\"worker-over\",\"sid\":\"s-w\",\"why\":\"finished\",\"rose_bytes\":2147483648}\n",
        "{\"ts\":\"t2\",\"sid\":\"s-w\",\"kind\":\"disk-free\",\"action\":\"removing\",\"path\":\"/h/c\",\"bytes\":2147483648,\"text\":\"a cache\"}\n",
        "{\"ts\":\"t3\",\"sid\":\"s-w\",\"kind\":\"disk-free\",\"action\":\"removed\",\"path\":\"/h/c\",\"bytes\":2147483648,\"text\":\"a cache\"}\n",
    );
    let rows = free_rows(ledger);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (
            rows[0].action.as_str(),
            rows[0].sid.as_str(),
            rows[0].path.as_str(),
            rows[0].bytes
        ),
        ("removing", "s-w", "/h/c", Some(2 * GIB))
    );
    assert_eq!(rows[1].action, "removed");
    let asked = ask_rows(ledger);
    assert_eq!(asked.len(), 1, "a disk-free row is no free-ask row");
    assert_eq!(asked[0].rose, Some(2 * GIB));
    let drive = concat!(
        "{\"ts\":10,\"sid\":\"s-w\",\"rule_id\":\"disk-worker@v1\",\"decision\":\"declined\",\"command\":\"rm -rf /h/c\",\"reason\":\"r\"}\n",
        "{\"ts\":20,\"sid\":\"s-w\",\"rule_id\":\"continue@v1\",\"decision\":\"typed\",\"command\":\"[aterm harness] go on\"}\n",
    );
    let d = decisions(drive);
    assert_eq!(
        d[0],
        Decided {
            ts: 10,
            rule: "disk-worker@v1".to_owned(),
            decision: "declined".to_owned(),
            command: "rm -rf /h/c".to_owned(),
            reason: "r".to_owned(),
        }
    );
    assert_eq!((d[1].ts, d[1].decision.as_str()), (20, "typed"));
    assert_eq!(squashed("a b\n c"), "abc");
}
