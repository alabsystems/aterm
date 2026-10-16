// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::testing::{Box3, FakeRunner, machine, ran};

fn env_of(b: &Box3, config: &str, extra: &[(&str, &str)], tty: bool) -> Env {
    let cfg = b.dir.path().join("aterm.toml");
    std::fs::write(&cfg, config).expect("config");
    let mut vars = vec![
        ("HOME".to_string(), b.home.display().to_string()),
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ),
    ];
    vars.extend(
        extra
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
    );
    Env {
        now: 1_800_000_000,
        home: Some(b.home.clone()),
        vars,
        config: Some(cfg),
        aterm_state: Some(b.state.clone()),
        harness_state: None,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        cwd: b.dir.path().to_path_buf(),
        stdin_tty: tty,
        exe: Some(PathBuf::from("/app/aterm")),
    }
}

const ONLY_CODEX: &str = "[ai]\npreferred = \"codex\"\n[ai.claude]\nuse = false\n[ai.agy]\nuse = \
                          false\n[ai.muse]\nuse = false\n";

fn run_cli(env: &Env, runner: &FakeRunner, args: &[&str]) -> (u8, String, String) {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(env, runner, &args, &mut out, &mut err);
    let code = if code == ExitCode::SUCCESS {
        0
    } else if code == ExitCode::from(1) {
        1
    } else {
        2
    };
    (
        code,
        String::from_utf8(out).expect("utf8"),
        String::from_utf8(err).expect("utf8"),
    )
}

#[test]
fn aterm_ai_looks_and_runs_nothing() {
    let b = machine(&["codex"]);
    let env = env_of(&b, ONLY_CODEX, &[], true);
    let runner = FakeRunner::quiet();
    let (code, out, _) = run_cli(&env, &runner, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("preferred:  codex (aterm.toml)\n"), "{out}");
    assert!(
        out.contains("codex:      signed-out     never signed in here"),
        "{out}"
    );
    assert!(out.contains("claude:     off "), "{out}");
    assert!(
        out.contains("reviews:    off: list in [ai] reviewers"),
        "{out}"
    );
    assert!(
        out.contains("\nhealth: FAIL — 1 CLI needs you (codex); next: aterm ai doctor\n"),
        "{out}"
    );
    assert!(
        runner.calls().is_empty(),
        "the look runs no child: {:?}",
        runner.calls()
    );
}

#[test]
fn the_doctor_signs_in_for_a_person_and_only_looks_for_an_agent() {
    let b = machine(&["codex"]);
    // An agent: read-only whatever the terminal, and exit 1 for the CLI that needs a person.
    let env = env_of(&b, ONLY_CODEX, &[("CLAUDECODE", "1")], true);
    let runner = FakeRunner::quiet();
    let (code, out, _) = run_cli(&env, &runner, &["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("next: ask a person to run `aterm ai doctor`"),
        "{out}"
    );
    assert!(runner.calls().is_empty());
    // `--read-only`: the same.
    let env = env_of(&b, ONLY_CODEX, &[], true);
    let (code, _, _) = run_cli(&env, &runner, &["doctor", "--read-only"]);
    assert_eq!(code, 1);
    assert!(runner.calls().is_empty());
    // A person: Codex's own sign-in, then its status, then the look again.
    let home = b.home.clone();
    let runner = FakeRunner::with(move |call| {
        if call.args == ["login", "status"] {
            std::fs::create_dir_all(home.join(".codex")).expect("dir");
            std::fs::write(home.join(".codex/auth.json"), "x").expect("auth");
            return Ok(ran(0, "", "Logged in using ChatGPT\n"));
        }
        Ok(ran(0, "", ""))
    });
    let (code, out, _) = run_cli(&env, &runner, &["doctor"]);
    let calls = runner.calls();
    assert_eq!(
        calls
            .iter()
            .map(|c| (c.terminal, c.args.clone()))
            .collect::<Vec<_>>(),
        [
            (true, vec!["login".to_string()]),
            (false, vec!["login".to_string(), "status".to_string()]),
        ]
    );
    assert!(out.contains("codex: signed in (chatgpt)"), "{out}");
    assert!(out.contains("\nhealth: OK"), "{out}");
    assert_eq!(code, 0);
}

#[test]
fn the_json_names_every_cli_by_its_closed_words() {
    let b = machine(&["codex"]);
    let env = env_of(&b, ONLY_CODEX, &[], false);
    let (code, out, _) = run_cli(&env, &FakeRunner::quiet(), &["doctor", "--json"]);
    assert_eq!(code, 1);
    let v: Value = aterm_json::from_str(out.trim()).expect("json");
    assert_eq!(v.get("kind").and_then(Value::as_str), Some("ai"));
    assert_eq!(v.get("preferred").and_then(Value::as_str), Some("codex"));
    let clis = v.get("clis").and_then(Value::as_array).expect("clis");
    let codex = clis
        .iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some("codex"))
        .expect("codex");
    assert_eq!(
        codex.get("state").and_then(Value::as_str),
        Some("signed-out")
    );
    assert_eq!(
        codex.get("fix").and_then(Value::as_str),
        Some("aterm ai login codex")
    );
    assert_eq!(
        v.get("health")
            .and_then(|h| h.get("word"))
            .and_then(Value::as_str),
        Some("FAIL")
    );
}

#[test]
fn a_review_is_refused_until_reviewers_are_listed_and_never_from_a_sandbox_or_a_reviewer() {
    let b = machine(&["codex"]);
    let runner = FakeRunner::quiet();
    let env = env_of(&b, ONLY_CODEX, &[], false);
    let (code, _, err) = run_cli(&env, &runner, &["review"]);
    assert_eq!(code, 2);
    assert!(err.contains("no reviewers"), "{err}");
    let listed = ONLY_CODEX.replacen("[ai]\n", "[ai]\nreviewers = [\"codex\"]\n", 1);
    for (k, why) in [
        ("CODEX_SANDBOX", "sandbox"),
        ("ATERM_AI_REVIEW", "a reviewer may not"),
    ] {
        let env = env_of(&b, &listed, &[(k, "1")], false);
        let (code, _, err) = run_cli(&env, &runner, &["review"]);
        assert_eq!(code, 2, "{k}");
        assert!(err.contains(why), "{k}: {err}");
    }
    let env = env_of(&b, &listed, &[], false);
    let (code, _, err) = run_cli(&env, &runner, &["review", "--with", "claude"]);
    assert_eq!(code, 2);
    assert!(err.contains("is not in [ai] reviewers"), "{err}");
    assert!(
        runner.calls().is_empty(),
        "nothing ran: {:?}",
        runner.calls()
    );
}

#[test]
fn login_refuses_with_no_terminal_and_names_a_cli_it_does_not_know() {
    let b = machine(&["codex"]);
    let runner = FakeRunner::quiet();
    let (code, _, err) = run_cli(
        &env_of(&b, ONLY_CODEX, &[], false),
        &runner,
        &["login", "codex"],
    );
    assert_eq!(code, 2);
    assert!(err.contains("not a terminal"), "{err}");
    let (code, _, err) = run_cli(
        &env_of(&b, ONLY_CODEX, &[], true),
        &runner,
        &["login", "gpt"],
    );
    assert_eq!(code, 2);
    assert!(err.contains("is no AI CLI"), "{err}");
    assert!(runner.calls().is_empty());
}

#[test]
fn doctor_on_refuses_a_machine_that_is_an_ssh_option() {
    let b = machine(&["codex"]);
    let runner = FakeRunner::quiet();
    let env = env_of(&b, ONLY_CODEX, &[], false);
    for bad in ["-oProxyCommand=x", "m3;rm", "m3:relative/aterm"] {
        let (code, _, err) = run_cli(&env, &runner, &["doctor", "--on", bad]);
        assert_eq!(code, 2, "{bad}");
        assert!(err.contains("is not a machine"), "{bad}: {err}");
    }
    assert!(runner.calls().is_empty());
}

#[test]
fn a_fleet_poll_signs_in_nowhere_and_survives_any_answer() {
    let b = machine(&["codex"]);
    // A person's terminal, but --json: a poll, read-only, whatever this machine needs.
    let env = env_of(&b, ONLY_CODEX, &[], true);
    let big = "é".repeat(40 * 1024);
    let runner = FakeRunner::with(move |call| {
        Ok(if call.args.iter().any(|a| a == "m3") {
            ran(0, &big, "")
        } else if call.args.iter().any(|a| a == "m5") {
            ran(
                0,
                "{\"clis\":[{\"id\":\"codex\",\"state\":\"ok\"},{\"id\":\"codex\",\"state\":\"ok\"}]}",
                "",
            )
        } else if call.args.iter().any(|a| a == "m6") {
            ran(
                0,
                "{\"clis\":[{\"id\":\"codex\",\"state\":\"ok\"},{\"id\":\"codex\",\"state\":\"signed-out\"}]}",
                "",
            )
        } else {
            ran(
                0,
                "{\"clis\":[{\"id\":\"codex\",\"state\":\"ok\"},{\"id\":\"gpt\",\"state\":\"ok\"},{\"id\":\"agy\",\"state\":\"\\u001b]52;c;x\\u0007\"}]}",
                "",
            )
        })
    });
    let (code, out, _) = run_cli(&env, &runner, &["doctor", "--json", "--on", "m3,m4,m5,m6"]);
    assert!(
        runner.calls().iter().all(|c| !c.terminal),
        "nothing ran on the terminal"
    );
    let v: Value = aterm_json::from_str(out.trim()).expect("one json document");
    assert_eq!(v.get("kind").and_then(Value::as_str), Some("ai-fleet"));
    let machines = v
        .get("machines")
        .and_then(Value::as_array)
        .expect("machines");
    assert_eq!(machines.len(), 5);
    assert_eq!(
        machines[1].get("clis"),
        Some(&Value::Null),
        "an answer over 64 KiB is not read"
    );
    let m4 = machines[2]
        .get("clis")
        .and_then(Value::as_object)
        .expect("m4");
    assert_eq!(m4.len(), 1, "unknown CLIs and words are dropped: {m4:?}");
    for machine in &machines[3..] {
        assert_eq!(
            machine.get("clis"),
            Some(&Value::Null),
            "duplicate CLI rows cannot count a machine twice or contradict it: {machine:?}"
        );
    }
    let cov = v
        .get("coverage")
        .and_then(|c| c.get("codex"))
        .expect("codex coverage");
    assert_eq!(cov.get("working").and_then(Value::as_i64), Some(1));
    assert_eq!(cov.get("of").and_then(Value::as_i64), Some(2));
    assert_eq!(code, 1, "this machine's codex is signed out");
}
