// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::permission::Level;
use crate::ai::testing::{Box3, FakeFetcher, FakeRunner, machine, ran};

fn env_of(b: &Box3, config: &str, extra: &[(&str, &str)], tty: bool) -> Env {
    let cfg = b.dir.path().join("aterm.toml");
    std::fs::write(&cfg, config).expect("config");
    // The system's own directories, never the host's PATH: a CLI someone installed by hand
    // there (a `~/.grok/bin/grok`) would read as one aterm does not manage.
    let mut vars = vec![
        ("HOME".to_string(), b.home.display().to_string()),
        (
            "PATH".to_string(),
            "/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
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
        asker: Asker::Person,
    }
}

/// Codex alone: every other connection, Grok Build's included, is not used here.
const ONLY_CODEX: &str = "[ai]\npreferred = \"codex\"\n[ai.claude]\nuse = false\n[ai.agy]\nuse = \
                          false\n[ai.muse]\nuse = false\n[ai.grok]\nuse = false\n[ai.kimi]\nuse = \
                          false\n";

/// Codex and Kimi Code: the other connections are not used here.
const CODEX_AND_KIMI: &str = "[ai]\npreferred = \"codex\"\n[ai.claude]\nuse = false\n[ai.agy]\nuse \
                              = false\n[ai.muse]\nuse = false\n[ai.grok]\nuse = false\n";

/// Codex and Grok Build: the other connections are not used here.
const CODEX_AND_GROK: &str = "[ai]\npreferred = \"codex\"\n[ai.claude]\nuse = false\n[ai.agy]\nuse \
                              = false\n[ai.muse]\nuse = false\n[ai.kimi]\nuse = false\n";

fn run_cli(env: &Env, runner: &FakeRunner, args: &[&str]) -> (u8, String, String) {
    run_net(env, runner, &FakeFetcher::default(), args)
}

fn run_net(
    env: &Env,
    runner: &FakeRunner,
    net: &FakeFetcher,
    args: &[&str],
) -> (u8, String, String) {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(env, runner, net, &args, &mut out, &mut err);
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
        out.contains("grok:       off            not used on this machine"),
        "a program not used here is off, no need: {out}"
    );
    assert!(
        out.contains("reviews:    on · nobody: no other AI CLI is used here (when codex asks)"),
        "an off row is asked by no review: {out}"
    );
    let (_, json, _) = run_cli(&env, &runner, &["--json"]);
    let v: Value = aterm_json::from_str(json.trim()).expect("json");
    let codex = v["clis"]
        .as_array()
        .and_then(|clis| clis.iter().find(|c| c["id"] == Value::from("codex")))
        .expect("codex row");
    assert_eq!(
        (&codex["fix_by"], &codex["fix"]),
        (&Value::from("person"), &Value::from("aterm ai login codex")),
        "codex has no roster, so only its own sign-in signs it in: {json}"
    );
    assert!(v["health"]["needs_you"].as_str().is_some(), "{json}");
    assert_eq!(
        out.lines().filter(|l| l.starts_with("NEEDS YOU:")).count(),
        1,
        "{out}"
    );
    assert!(
        runner.calls().is_empty(),
        "the look runs no child: {:?}",
        runner.calls()
    );
}

/// A CLI'S OWN SIGN-IN IS A PERSON'S: under an agent the doctor starts no `codex login` and says
/// so, naming the verb a person starts it by, never handing the agent a command aterm runs
/// itself; `--read-only` runs nothing; for a person at a terminal it runs the login and its
/// status read.
#[test]
fn the_doctor_runs_a_clis_own_sign_in_only_for_a_person() {
    let b = machine(&["codex"]);
    let env = env_of(&b, ONLY_CODEX, &[("CLAUDECODE", "1")], true);
    let runner = FakeRunner::quiet();
    let (code, out, _) = run_cli(&env, &runner, &["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains(
            "codex: not started: CLAUDECODE is set (an agent runs this); no route of aterm's \
             signs codex in, so a person does, once, at this Mac (`aterm ai login codex`)"
        ),
        "{out}"
    );
    assert!(
        out.contains("NEEDS YOU: sign codex in once at this Mac: `aterm ai login codex`"),
        "{out}"
    );
    assert!(!out.contains("ask a person"), "{out}");
    assert!(runner.calls().is_empty());
    let env = env_of(&b, ONLY_CODEX, &[], true);
    let (code, _, _) = run_cli(&env, &runner, &["doctor", "--read-only"]);
    assert_eq!(code, 1);
    assert!(runner.calls().is_empty());
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
fn the_doctor_repairs_definite_unmeasured_vendor_failures_but_not_unknown_logins() {
    let only_agy = "[ai]\npreferred = \"agy\"\n[ai.claude]\nuse = false\n[ai.codex]\nuse = false\n[ai.muse]\nuse = false\n[ai.grok]\nuse = false\n[ai.kimi]\nuse = false\n";
    let missing = machine(&[]);
    let env = env_of(&missing, only_agy, &[], true);
    let runner = FakeRunner::quiet();
    let (code, out, _) = run_cli(&env, &runner, &["doctor"]);
    assert_eq!(code, 1, "the stand-in did not install anything: {out}");
    assert_eq!(runner.calls().len(), 1);
    assert_eq!(runner.calls()[0].args, ["pkg", "install", "agy"]);
    assert!(runner.calls()[0].terminal);

    let installed = machine(&["agy"]);
    let env = env_of(&installed, only_agy, &[], true);
    let runner = FakeRunner::quiet();
    let (code, out, _) = run_cli(&env, &runner, &["doctor"]);
    assert_eq!(
        code, 0,
        "an unread keyring is unknown, not signed out: {out}"
    );
    assert!(runner.calls().is_empty());
    env.store()
        .expect("store")
        .note(
            "agy",
            super::super::store::How::Review,
            super::super::store::Outcome::Ended(super::super::store::Ended::ReviewAuth),
            env.now,
        )
        .expect("recorded authentication failure");
    let (_, out, _) = run_cli(&env, &runner, &["doctor"]);
    let calls = runner.calls();
    assert_eq!(calls.len(), 1, "{out}");
    assert_eq!(calls[0].program, "agy");
    assert!(
        calls[0].args.is_empty(),
        "agy signs in on interactive startup"
    );
    assert!(calls[0].terminal);
    assert!(
        out.contains("nothing reads whether it worked"),
        "no unsupported status probe: {out}"
    );
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
fn a_review_is_refused_when_off_and_never_from_a_sandbox_or_a_reviewer() {
    let b = machine(&["codex"]);
    let runner = FakeRunner::quiet();
    let off = ONLY_CODEX.replacen("[ai]\n", "[ai]\nreviews = false\n", 1);
    let env = env_of(&b, &off, &[], false);
    let (code, _, err) = run_cli(&env, &runner, &["review"]);
    assert_eq!(code, 2);
    assert!(err.contains("reviews are off"), "{err}");
    for (k, why) in [
        ("CODEX_SANDBOX", "sandbox"),
        ("ATERM_AI_REVIEW", "a reviewer may not"),
    ] {
        let env = env_of(&b, ONLY_CODEX, &[(k, "1")], false);
        let (code, _, err) = run_cli(&env, &runner, &["review"]);
        assert_eq!(code, 2, "{k}");
        assert!(err.contains(why), "{k}: {err}");
    }
    let env = env_of(&b, ONLY_CODEX, &[], false);
    let (code, _, err) = run_cli(&env, &runner, &["review", "--with", "gpt"]);
    assert_eq!(code, 2);
    assert!(err.contains("is no AI CLI"), "{err}");
    let (code, _, err) = run_cli(&env, &runner, &["review", "--permissions", "root"]);
    assert_eq!(code, 2);
    assert!(err.contains("--permissions takes"), "{err}");
    assert!(
        runner.calls().is_empty(),
        "nothing ran: {:?}",
        runner.calls()
    );
}

#[test]
fn reviews_are_on_for_every_other_cli_at_the_askers_level() {
    let b = machine(&["claude", "codex"]);
    let both = "[ai]\n[ai.agy]\nuse = false\n[ai.muse]\nuse = false\n[ai.grok]\nuse = false\n[ai.kimi]\nuse = false\n";
    let mut env = env_of(&b, both, &[], false);
    env.asker = Asker::Agent("claude", Level::Yolo);
    let (_, out, _) = run_cli(&env, &FakeRunner::quiet(), &[]);
    assert!(
        out.contains("reviews:    on · codex (when claude asks) · up to 4 per review · 600 s · yolo (inherited from claude)"),
        "{out}"
    );
    env.asker = Asker::Person;
    let (_, out, _) = run_cli(&env, &FakeRunner::quiet(), &[]);
    assert!(out.contains("yolo (a person asks)"), "{out}");
    let pinned = both.replacen("[ai]\n", "[ai]\nreview_permissions = \"plan\"\n", 1);
    let mut env = env_of(&b, &pinned, &[], false);
    env.asker = Asker::Agent("codex", Level::Yolo);
    let (_, out, _) = run_cli(&env, &FakeRunner::quiet(), &[]);
    assert!(
        out.contains("claude (when codex asks)") && out.contains("plan (set in aterm.toml)"),
        "{out}"
    );
    let (_, json, _) = run_cli(&env, &FakeRunner::quiet(), &["--json"]);
    let v: Value = aterm_json::from_str(json.trim()).expect("json");
    assert_eq!(v["reviews"]["level"], Value::from("plan"));
    assert_eq!(v["reviews"]["asker"], Value::from("codex"));
    assert_eq!(v["reviews"]["on"], Value::Bool(true));
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

const FLEET_URL: &str = "https://fleet.example/ai-fleet.toml";

fn fleet_machine(b: &Box3) -> (Env, PathBuf, String) {
    let secret = b.dir.path().join("fleet.key");
    let recording = aterm_durable::seam::Recording::start();
    let (code, out, err) = run_cli(
        &env_of(b, "", &[], false),
        &FakeRunner::quiet(),
        &["fleet", "keygen", secret.to_str().expect("utf8")],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Durable],
        "the secret key the person is told to keep is durable"
    );
    let public = out
        .lines()
        .find_map(|l| l.strip_prefix("public key: "))
        .expect("public key")
        .to_string();
    let config = ONLY_CODEX.replacen(
        "[ai]\n",
        &format!("[ai]\nfleet = {FLEET_URL:?}\nfleet_key = {public:?}\n"),
        1,
    );
    (env_of(b, &config, &[], false), secret, public)
}

fn signed(b: &Box3, env: &Env, secret: &Path, text: &str) -> (Vec<u8>, Vec<u8>) {
    let doc = b.dir.path().join("served.toml");
    std::fs::write(&doc, text).expect("doc");
    let recording = aterm_durable::seam::Recording::start();
    let (code, out, err) = run_cli(
        env,
        &FakeRunner::quiet(),
        &[
            "fleet",
            "sign",
            doc.to_str().expect("utf8"),
            "--key",
            secret.to_str().expect("utf8"),
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered],
        "a signature, made again from the document and the key, is only ordered"
    );
    assert!(out.starts_with("signed "), "{out}");
    let sig = std::fs::read(b.dir.path().join("served.toml.sig")).expect("sig");
    (text.as_bytes().to_vec(), sig)
}

#[cfg(unix)]
#[test]
fn keygen_makes_a_private_key_once() {
    use std::os::unix::fs::PermissionsExt as _;
    let b = machine(&["codex"]);
    let (env, secret, public) = fleet_machine(&b);
    let mode = std::fs::metadata(&secret)
        .expect("key")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(crate::ai::fleet::key_bytes(&public).is_some());
    let (code, _, err) = run_cli(
        &env,
        &FakeRunner::quiet(),
        &["fleet", "keygen", secret.to_str().expect("utf8")],
    );
    assert_eq!(code, 1);
    assert!(err.contains("never overwritten"), "{err}");
}

#[test]
fn a_signed_document_verifies_and_an_edited_one_does_not() {
    let b = machine(&["codex"]);
    let (env, secret, _) = fleet_machine(&b);
    let _ = signed(
        &b,
        &env,
        &secret,
        "schema = 1\nseq = 3\npreferred = \"codex\"\n",
    );
    let doc = b.dir.path().join("served.toml");
    let doc_s = doc.to_str().expect("utf8");
    let (code, out, err) = run_cli(&env, &FakeRunner::quiet(), &["fleet", "verify", doc_s]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("seq 3"), "{out}");
    std::fs::write(&doc, "schema = 1\nseq = 3\npreferred = \"muse\"\n").expect("edit");
    let (code, _, err) = run_cli(&env, &FakeRunner::quiet(), &["fleet", "verify", doc_s]);
    assert_eq!(code, 1);
    assert!(err.contains("does not verify"), "{err}");
}

#[test]
fn the_doctor_fetches_a_signed_document_and_the_look_applies_it() {
    let b = machine(&["codex"]);
    let (env, secret, public) = fleet_machine(&b);
    let net = FakeFetcher::default();
    let (doc, sig) = signed(
        &b,
        &env,
        &secret,
        "schema = 1\nseq = 1\nreview_timeout_s = 120\n",
    );
    net.serve(FLEET_URL, &doc);
    net.serve(&format!("{FLEET_URL}.sig"), &sig);
    let (_, out, _) = run_net(&env, &FakeRunner::quiet(), &net, &[]);
    assert!(out.contains("refused (not fetched yet)"), "{out}");
    assert!(net.asked().is_empty(), "a look fetches nothing");
    let (_, out, _) = run_net(&env, &FakeRunner::quiet(), &net, &["doctor", "--read-only"]);
    assert_eq!(net.asked().len(), 2, "{:?}", net.asked());
    assert!(out.contains("120 s"), "{out}");
    assert!(out.contains("seq 1 · fetched "), "{out}");
    assert!(
        out.contains(&format!(
            "signer {}…",
            crate::ai::fleet::fingerprint(&public)
        )),
        "{out}"
    );
    let _ = run_net(&env, &FakeRunner::quiet(), &net, &["doctor", "--read-only"]);
    assert_eq!(net.asked().len(), 2);
    let (code, out, _) = run_net(&env, &FakeRunner::quiet(), &net, &["fleet", "fetch"]);
    assert_eq!((code, out.trim()), (0, "unchanged (seq 1)"));
    let (other_doc, _) = (
        b"schema = 1\nseq = 9\nreview_timeout_s = 3600\n".to_vec(),
        (),
    );
    let (other_secret, _) = crate::ai::fleet::keygen().expect("keygen");
    let other_sig = crate::ai::fleet::sign(&other_secret, &other_doc).expect("sign");
    net.serve(FLEET_URL, &other_doc);
    net.serve(&format!("{FLEET_URL}.sig"), other_sig.as_bytes());
    let (code, out, _) = run_net(&env, &FakeRunner::quiet(), &net, &["fleet", "fetch"]);
    assert_eq!(code, 1);
    assert!(out.contains("does not verify"), "{out}");
    let (_, out, _) = run_net(&env, &FakeRunner::quiet(), &net, &[]);
    assert!(out.contains("120 s"), "the held document stands: {out}");
    assert!(out.contains("last fetch failed"), "{out}");
    let (_, status, _) = run_net(&env, &FakeRunner::quiet(), &net, &["fleet"]);
    assert!(status.contains("held:       seq 1"), "{status}");
}

#[test]
fn unreadable_ancestry_never_becomes_a_persons_yolo_review() {
    struct Tree {
        missing_args: bool,
        missing_parent: bool,
    }
    impl permission::Procs for Tree {
        fn first(&self) -> Option<u32> {
            Some(40)
        }
        fn parent(&self, _: u32) -> Option<u32> {
            if self.missing_parent { None } else { Some(1) }
        }
        fn args(&self, _: u32) -> Option<(String, Vec<String>)> {
            (!self.missing_args).then(|| ("/bin/zsh".to_string(), vec!["zsh".to_string()]))
        }
    }
    let (local, notes) = config::read("");
    let settings = config::resolve(&local, None, None, notes);
    let person = Tree {
        missing_args: false,
        missing_parent: false,
    };
    let observed = permission::asker(&person, &|| None);
    assert_eq!(
        review_level(&settings, observed).expect("proven person").0,
        Level::Yolo
    );
    let bad: Vec<_> = [(true, false), (false, true)]
        .into_iter()
        .filter(|&(missing_args, missing_parent)| {
            let observed = permission::asker(
                &Tree {
                    missing_args,
                    missing_parent,
                },
                &|| None,
            );
            review_level(&settings, observed).is_ok()
        })
        .collect();
    assert!(bad.is_empty(), "unknown ancestry received Yolo: {bad:?}");
}

#[test]
fn unknown_inheritance_stops_before_the_real_cli_collects_or_runs() {
    let b = machine(&["codex"]);
    let mut env = env_of(&b, ONLY_CODEX, &[], false);
    env.asker = Asker::Unknown;
    let runner = FakeRunner::quiet();
    let (code, _, err) = run_cli(&env, &runner, &["review"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("permissions are unknown"), "{err}");
    assert!(
        runner.calls().is_empty(),
        "no git collection or reviewer: {:?}",
        runner.calls()
    );
    let pinned = ONLY_CODEX.replacen("[ai]\n", "[ai]\nreview_permissions = \"plan\"\n", 1);
    let settings = config::read(&pinned);
    let settings = config::resolve(&settings.0, None, None, settings.1);
    assert_eq!(
        review_level(&settings, Asker::Unknown)
            .expect("explicit fixed choice")
            .0,
        Level::Plan
    );
}

/// `aterm ai login kimi` IS THE CONNECTION'S SIGN-IN: with this machine keeping
/// `accounts-kimi.toml` it is the pool's renewal of its next row, aterm's own, which runs with
/// no terminal and under an agent too; with no roster it is kimi's own `kimi login`, a person's,
/// which needs a terminal and is refused under an agent. A word that names no CLI is refused,
/// kimi among those it lists.
#[test]
fn login_kimi_renews_its_pool_or_runs_its_own_login_for_a_person() {
    let b = machine(&["codex", "kimi"]);
    let mut env = env_of(&b, CODEX_AND_KIMI, &[], true);
    env.harness_state = Some(b.state.clone());
    let runner = FakeRunner::quiet();
    let (code, out, err) = run_cli(&env, &runner, &["login", "kimi"]);
    assert_eq!(code, 0, "{out}{err}");
    let calls = runner.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].terminal && calls[0].program == "kimi" && calls[0].args == ["login"],
        "no roster: kimi's own login: {:?}",
        calls[0]
    );
    let runner = FakeRunner::quiet();
    let (code, _, err) = run_cli(
        &env_of(&b, CODEX_AND_KIMI, &[], false),
        &runner,
        &["login", "kimi"],
    );
    assert_eq!(code, 2);
    assert!(err.contains("not a terminal"), "{err}");
    let agent = env_of(&b, CODEX_AND_KIMI, &[("CLAUDECODE", "1")], true);
    let (code, out, _) = run_cli(&agent, &runner, &["login", "kimi"]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("kimi: not started: CLAUDECODE is set"),
        "{out}"
    );
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());

    std::fs::write(
        b.dir.path().join("accounts-kimi.toml"),
        "[[account]]\nlabel = \"kimi-one\"\nclass = \"dedicated\"\n\
         email = \"kimi-one@example.com\"\nregion = \"global\"\n",
    )
    .expect("roster");
    for (who, extra, tty) in [
        ("a person", &[][..], true),
        (
            "an agent with no terminal",
            &[("CLAUDECODE", "1")][..],
            false,
        ),
    ] {
        let mut env = env_of(&b, CODEX_AND_KIMI, extra, tty);
        env.harness_state = Some(b.state.clone());
        let runner = FakeRunner::quiet();
        let (code, out, err) = run_cli(&env, &runner, &["login", "kimi"]);
        assert_eq!(code, 0, "{who}: {out}{err}");
        let calls = runner.calls();
        assert_eq!(calls.len(), 1, "{who}: {calls:?}");
        assert!(
            calls[0].program == "aterm"
                && calls[0].args == ["harness", "accounts", "renew", "kimi-one", "--cli", "kimi"],
            "{who}: {:?}",
            calls[0]
        );
        assert!(out.contains("kimi: signed in"), "{who}: {out}");
    }

    let (code, _, err) = run_cli(&env, &runner, &["login", "gpt"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("is no AI CLI") && err.contains("kimi"),
        "{err}"
    );
}

/// UNDER AN AGENT, WITH NO TERMINAL, THE DOCTOR MAKES ATERM'S OWN FIXES AND HOLDS ONLY A
/// PERSON'S (owner ruling R-13: no human user): it installs a CLI that is not installed (`aterm
/// pkg install muse`) and renews a pool row with an email (`aterm harness accounts renew
/// kimi-one --cli kimi`), starts no CLI's own sign-in (codex has no roster), and says what only
/// a person can give on one `NEEDS YOU:` line. CONTROL: `--read-only` runs nothing.
#[test]
fn the_doctor_makes_aterms_own_fixes_under_an_agent_and_holds_only_a_persons() {
    let b = machine(&["codex", "kimi"]);
    let config = "[ai]\npreferred = \"codex\"\n[ai.claude]\nuse = false\n[ai.agy]\nuse = false\n\
                  [ai.grok]\nuse = false\n";
    std::fs::write(
        b.dir.path().join("accounts-kimi.toml"),
        "[[account]]\nlabel = \"kimi-one\"\nclass = \"dedicated\"\n\
         email = \"kimi-one@example.com\"\nregion = \"global\"\n",
    )
    .expect("roster");
    let mut env = env_of(&b, config, &[("CLAUDECODE", "1")], false);
    env.harness_state = Some(b.state.clone());
    let runner = FakeRunner::quiet();
    let (code, out, err) = run_cli(&env, &runner, &["doctor", "--read-only"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(runner.calls().is_empty(), "CONTROL: {:?}", runner.calls());
    let h = state::health(&look(&env).rows);
    assert_eq!(
        (h.by_aterm.clone(), h.by_person.clone()),
        (vec!["muse", "kimi"], vec!["codex"]),
        "{out}"
    );

    let (code, out, err) = run_cli(&env, &runner, &["doctor"]);
    assert_eq!(code, 1, "the stand-ins changed nothing: {out}{err}");
    let calls: Vec<(String, Vec<String>)> = runner
        .calls()
        .into_iter()
        .map(|c| (c.program, c.args))
        .collect();
    let args = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
    assert!(
        calls.contains(&("aterm".to_string(), args(&["pkg", "install", "muse"])))
            && calls.contains(&(
                "aterm".to_string(),
                args(&["harness", "accounts", "renew", "kimi-one", "--cli", "kimi"])
            )),
        "{calls:?}"
    );
    assert!(
        calls.iter().all(|(program, _)| program == "aterm"),
        "no CLI's own sign-in ran: {calls:?}"
    );
    assert!(
        runner
            .calls()
            .iter()
            .filter(|c| c.args.get(2).is_some_and(|verb| verb == "renew"))
            .all(|c| !c.terminal),
        "with nobody here aterm's own renewal runs off the terminal: {:?}",
        runner.calls()
    );
    assert!(out.contains("codex: not started: "), "{out}");
    let needs: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("NEEDS YOU:"))
        .collect();
    assert!(
        needs.len() == 1 && needs[0].contains("`aterm ai login codex`"),
        "said once, after the fixes: {out}"
    );
    assert!(!out.contains("ask a person"), "{out}");
}

/// A PERSON'S OFF SWITCH HOLDS EVERY AUTOMATED SIGN-IN (`[harness] logins = "off"` in the
/// caller's config, the switch the window's upkeep honours): the pool row with an email is then
/// a person's, its `fix:` the person's own `aterm ai login kimi`, and the doctor under an agent
/// starts no renewal. CONTROL: without the switch the same doctor renews the row.
#[test]
fn the_logins_off_switch_holds_the_doctors_renewal_under_an_agent() {
    let b = machine(&["codex", "kimi"]);
    std::fs::write(
        b.dir.path().join("accounts-kimi.toml"),
        "[[account]]\nlabel = \"kimi-one\"\nclass = \"dedicated\"\n\
         email = \"kimi-one@example.com\"\nregion = \"global\"\n",
    )
    .expect("roster");
    let renewed = |runner: &FakeRunner| {
        runner
            .calls()
            .iter()
            .any(|c| c.args.get(2).is_some_and(|verb| verb == "renew"))
    };
    for (config, renews) in [
        (
            format!("[harness]\nlogins = \"off\"\n{CODEX_AND_KIMI}"),
            false,
        ),
        (CODEX_AND_KIMI.to_string(), true),
    ] {
        let mut env = env_of(&b, &config, &[("CLAUDECODE", "1")], false);
        env.harness_state = Some(b.state.clone());
        let kimi = look(&env)
            .rows
            .into_iter()
            .find(|r| r.connector.id == "kimi")
            .expect("kimi row");
        assert_eq!(
            kimi.actor,
            Some(if renews { Actor::Aterm } else { Actor::Person }),
            "{config}"
        );
        let runner = FakeRunner::quiet();
        let (_, out, err) = run_cli(&env, &runner, &["doctor"]);
        assert_eq!(renewed(&runner), renews, "{config}: {out}{err}");
        if !renews {
            assert_eq!(kimi.fix.as_deref(), Some("aterm ai login kimi"));
        }
    }
}

/// A BARE `aterm ai login` LISTS EVERY NAME THE VERB TAKES: its usage names the same CLIs, in the
/// same order, as the refusal of a word that names none, the pool-only ones (kimi) among them,
/// and runs nothing.
#[test]
fn a_bare_login_lists_every_name_the_verb_takes() {
    let b = machine(&["codex"]);
    let env = env_of(&b, ONLY_CODEX, &[], true);
    let runner = FakeRunner::quiet();
    let listed = |err: &str| -> Vec<String> {
        let (_, tail) = err.trim_end().rsplit_once('(').expect("a list");
        let list = tail.strip_suffix(')').expect("a closed list");
        list.split(", ").map(str::to_owned).collect()
    };
    let (code, _, usage) = run_cli(&env, &runner, &["login"]);
    assert_eq!(code, 2, "{usage}");
    let (code, _, unknown) = run_cli(&env, &runner, &["login", "gpt"]);
    assert_eq!(code, 2, "{unknown}");
    assert!(usage.starts_with("usage: aterm ai login <cli>"), "{usage}");
    assert_eq!(listed(&usage), listed(&unknown), "{usage}{unknown}");
    assert!(listed(&usage).iter().any(|id| id == "kimi"), "{usage}");
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

/// `aterm ai login grok` WHERE NOBODY INSTALLED IT INSTALLS IT FIRST, AS THE DOCTOR DOES: Grok
/// Build, in atpkg's default set, is an install owed, which is aterm's own act, so the login runs
/// `aterm pkg install grok` and no vendor login; the stand-in installs nothing, so the sign-in is
/// then refused naming that (exit 2). CONTROL: excluded, it is off and refused, and nothing runs.
#[test]
fn logging_in_grok_where_nobody_installed_it_installs_it_first() {
    if !atpkg::vendor_direct::served_here("grok") {
        return;
    }
    let b = machine(&["codex"]);
    let env = env_of(&b, CODEX_AND_GROK, &[], true);
    let runner = FakeRunner::quiet();
    let (code, out, err) = run_cli(&env, &runner, &["login", "grok"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(out.contains("grok: not started: not installed"), "{out}");
    let calls = runner.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].args, ["pkg", "install", "grok"], "{calls:?}");
    let excluded = Env {
        exclude: vec!["grok".to_string()],
        ..env
    };
    let runner = FakeRunner::quiet();
    let (code, out, err) = run_cli(&excluded, &runner, &["login", "grok"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

/// OVER SSH, `aterm ai login grok` AND `aterm ai doctor` (which `doctor --on`/`--fleet` run on
/// each machine over `ssh -t`) RUN THE CLI'S OWN SIGN-IN UNDER THE NO-DESK SANDBOX: Grok
/// Build's `grok login --device-auth` and Codex's `codex login` open the default browser
/// through LaunchServices whatever their `PATH`, which over SSH is a desk nobody is at.
/// CONTROL: at the desk the same verbs run the CLI plainly.
#[cfg(target_os = "macos")]
#[test]
fn over_ssh_the_doctor_and_login_run_the_cli_under_the_no_desk_sandbox() {
    use crate::harness::login_drive::NO_DESK;
    let ssh = [("SSH_CONNECTION", "10.0.0.2 52000 10.0.0.3 22")];
    let terminal = |runner: &FakeRunner| -> Vec<(String, Vec<String>)> {
        runner
            .calls()
            .into_iter()
            .filter(|c| c.terminal)
            .map(|c| (c.program, c.args))
            .collect()
    };
    let b = machine(&["codex"]);
    let shim = b.pkg.agents_dir().join("codex").display().to_string();
    let runner = FakeRunner::quiet();
    let _ = run_cli(&env_of(&b, ONLY_CODEX, &ssh, true), &runner, &["doctor"]);
    assert_eq!(
        terminal(&runner),
        [(
            "sandbox-exec".to_string(),
            vec![
                "-p".to_string(),
                NO_DESK.to_string(),
                shim.clone(),
                "login".to_string()
            ]
        )]
    );
    let runner = FakeRunner::quiet();
    let _ = run_cli(&env_of(&b, ONLY_CODEX, &[], true), &runner, &["doctor"]);
    assert_eq!(
        terminal(&runner),
        [("codex".to_string(), vec!["login".to_string()])],
        "CONTROL"
    );
    if !atpkg::vendor_direct::served_here("grok") {
        return;
    }
    let b = machine(&["grok"]);
    let shim = b.pkg.agents_dir().join("grok").display().to_string();
    let runner = FakeRunner::quiet();
    let (_, out, err) = run_cli(
        &env_of(&b, CODEX_AND_GROK, &ssh, true),
        &runner,
        &["login", "grok"],
    );
    assert_eq!(
        terminal(&runner),
        [(
            "sandbox-exec".to_string(),
            vec![
                "-p".to_string(),
                NO_DESK.to_string(),
                shim,
                "login".to_string(),
                "--device-auth".to_string()
            ]
        )],
        "{out}{err}"
    );
    let runner = FakeRunner::quiet();
    let (_, out, err) = run_cli(
        &env_of(&b, CODEX_AND_GROK, &[], true),
        &runner,
        &["login", "grok"],
    );
    assert_eq!(
        terminal(&runner),
        [("grok".to_string(), vec!["login".to_string()])],
        "CONTROL: {out}{err}"
    );
}

#[test]
fn look_honours_harness_accounts_off_from_a_custom_config_file_name() {
    use crate::harness::accounts::{Cli, Store};

    let b = machine(&["codex"]);
    let config_dir = b.dir.path().join("custom-cfg-dir");
    let harness_state = b.state.join("harness/v1");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    std::fs::create_dir_all(&harness_state).expect("harness state");
    let custom_cfg = config_dir.join("custom-profile.toml");

    let staging = b.home.join(".aterm/logins/codex/work-1");
    std::fs::create_dir_all(&staging).expect("staging");
    std::fs::write(staging.join("auth.json"), "{}").expect("auth");
    std::fs::write(config_dir.join("accounts.toml"), "").expect("claude roster");
    std::fs::write(
        config_dir.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"work-1\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"w1@example.test\"\n",
            staging.display().to_string()
        ),
    )
    .expect("codex roster");

    let mut note = None;
    let store = Store::at_for(&harness_state, Cli::Codex, &b.home);
    store
        .transact(1_800_000_000, &mut note, |state| {
            state.active = Some("work-1".into());
            state.pinned = false;
            Ok(Vec::new())
        })
        .expect("init codex store");

    let make_env = |body: &str| {
        std::fs::write(&custom_cfg, body).expect("write custom config");
        let mut env = env_of(&b, "", &[], true);
        env.config = Some(custom_cfg.clone());
        env.harness_state = Some(harness_state.clone());
        env
    };

    // With accounts = "early" in custom-profile.toml, unpinned work-1 serves Codex.
    let early = look(&make_env(
        "[harness]\naccounts = \"early\"\n[ai]\npreferred = \"codex\"\n",
    ));
    let codex_early = early
        .facts
        .iter()
        .find(|f| f.connector.id == "codex")
        .expect("codex facts");
    assert_eq!(
        codex_early
            .pool
            .as_ref()
            .and_then(|p| p.serving_label.as_deref()),
        Some("work-1")
    );

    // With accounts = "off" in custom-profile.toml (no aterm.toml present), unpinned work-1 does
    // not serve; pinning work-1 serves it even under Mode::Off.
    let off = look(&make_env(
        "[harness]\naccounts = \"off\"\n[ai]\npreferred = \"codex\"\n",
    ));
    let codex_off = off
        .facts
        .iter()
        .find(|f| f.connector.id == "codex")
        .expect("codex facts");
    assert_eq!(
        codex_off
            .pool
            .as_ref()
            .and_then(|p| p.serving_label.as_deref()),
        None,
        "custom config file name must govern Mode::Off"
    );

    store
        .transact(1_800_000_000, &mut note, |state| {
            state.pinned = true;
            Ok(Vec::new())
        })
        .expect("pin work-1");
    let pinned_off = look(&make_env(
        "[harness]\naccounts = \"off\"\n[ai]\npreferred = \"codex\"\n",
    ));
    let codex_pinned = pinned_off
        .facts
        .iter()
        .find(|f| f.connector.id == "codex")
        .expect("codex facts");
    assert_eq!(
        codex_pinned
            .pool
            .as_ref()
            .and_then(|p| p.serving_label.as_deref()),
        Some("work-1"),
        "a pinned row still serves under Mode::Off"
    );
}

/// A Codex pool of `main` (this machine's own login, in the default home) and `work` (a row of
/// its own, signed in), run by a Codex build that keeps no row's login its own, so only `main`
/// serves; `held` is a line of `main`'s roster row (`held_by = "m2"`).
fn codex_default_only(held: &str) -> Box3 {
    let b = machine(&["codex"]);
    let exe = crate::harness::codex_alab::lay_package(&b.dir.path().join("store/old"), false);
    std::fs::write(
        b.pkg.agents_dir().join("codex"),
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", exe.display()),
    )
    .expect("launcher");
    std::fs::create_dir_all(b.home.join(".codex")).expect("default home");
    std::fs::write(b.home.join(".codex/auth.json"), "{}").expect("default login");
    let work = b.home.join(".aterm/logins/codex/work");
    std::fs::create_dir_all(&work).expect("work");
    std::fs::write(work.join("auth.json"), "{}").expect("work login");
    std::fs::write(
        b.dir.path().join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"own@example.com\"\n\
             {held}\n[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {:?}\n\
             email = \"work@example.com\"\n",
            work.display().to_string()
        ),
    )
    .expect("codex roster");
    b
}

fn codex_pool_store(b: &Box3) -> crate::harness::accounts::Store {
    crate::harness::accounts::Store::at_for(&b.state, crate::harness::accounts::Cli::Codex, &b.home)
}

fn sign_main_out(b: &Box3, now: i64) {
    codex_pool_store(b)
        .transact_logins(now, &mut None, |state, _| {
            Ok(crate::harness::accounts::note_auth(
                state,
                "main",
                crate::harness::accounts::Auth::SignedOut,
                now,
            ))
        })
        .expect("sign `main` out");
}

fn pooled_env(b: &Box3, extra: &[(&str, &str)], tty: bool) -> Env {
    let mut env = env_of(b, ONLY_CODEX, extra, tty);
    env.harness_state = Some(b.state.clone());
    env
}

/// The Codex row and facts of `env`'s look, and the verdict over every row.
fn codex_look(env: &Env) -> (Row, Facts, state::Health) {
    let l = look(env);
    let row = l.rows.iter().find(|r| r.connector.id == "codex").cloned();
    let f = l.facts.iter().find(|f| f.connector.id == "codex").cloned();
    (
        row.expect("codex row"),
        f.expect("codex facts"),
        state::health(&l.rows),
    )
}

/// ON A CODEX BUILD THAT KEEPS NO ROW'S LOGIN ITS OWN, THE SIGNED-OUT ROW ON THIS MACHINE'S OWN
/// LOGIN SIGNS IN BY CODEX'S OWN LOGIN, A PERSON'S: the pool names no verb for it
/// ([`crate::ai::facts::CliAccounts::sign_in_target`]), so its fix is a person's, and with
/// nobody here (no terminal and no agent's marker, as cron or a script runs it, or an agent at a
/// pty) neither `doctor` nor `login codex` starts `codex login`. CONTROL: a person at a terminal
/// runs the step the row names, and it starts Codex's own login.
#[test]
fn codexs_own_login_of_the_default_row_runs_only_for_a_person_at_a_terminal() {
    let b = codex_default_only("");
    sign_main_out(&b, 1_800_000_000);
    for (who, extra, tty) in [
        ("no terminal and no marker", &[][..], false),
        ("an agent at a pty", &[("CLAUDECODE", "1")][..], true),
    ] {
        let env = pooled_env(&b, extra, tty);
        let (row, f, h) = codex_look(&env);
        let pool = f.pool.as_ref().expect("codex pool");
        assert!(
            crate::ai::login::default_home_sign_in(pool),
            "{who}: {pool:?}"
        );
        assert_eq!(row.actor, Some(Actor::Person), "{who}: {}", row.fact);
        assert!(h.by_aterm.is_empty(), "{who}: {:?}", h.by_aterm);
        assert_eq!(h.by_person, ["codex"], "{who}");
        for verb in [&["doctor"][..], &["login", "codex"][..]] {
            let runner = FakeRunner::quiet();
            let (_, out, err) = run_cli(&env, &runner, verb);
            assert!(
                runner.calls().is_empty(),
                "{who}: {verb:?} ran {:?}: {out}{err}",
                runner.calls()
            );
        }
    }
    let env = pooled_env(&b, &[], true);
    let (_, _, h) = codex_look(&env);
    assert_eq!(h.person_steps, ["aterm ai login codex"], "CONTROL");
    let runner = FakeRunner::quiet();
    let _ = run_cli(&env, &runner, &["login", "codex"]);
    let calls = runner.calls();
    assert!(
        calls
            .first()
            .is_some_and(|c| c.terminal && c.program == "codex" && c.args == ["login"]),
        "CONTROL: {calls:?}"
    );
}

/// THE CODEX ROW ON THIS MACHINE'S OWN LOGIN THAT NOTHING HERE SIGNS IN HANDS NOBODY A STEP: on a
/// Codex build that keeps no row's login its own, while another Mac's warm set holds it (signed
/// out) or while it serves with its renewal due (Codex's own login would revoke the login that
/// still works), the row offers no fix and no actor, the verdict names no person's step and no
/// `NEEDS YOU:` line, and `aterm ai login codex` and `aterm ai doctor` run nothing. With no
/// terminal, `login codex` refuses with the same reason a terminal's run gives
/// ([`crate::ai::login::refusal`]), never sending a person to a terminal where it refuses, and
/// every run prints that reason whole.
#[test]
fn a_codex_default_row_nothing_here_signs_in_hands_nobody_a_step() {
    let now = 1_800_000_000_i64;
    let held = codex_default_only("held_by = \"m2\"\n");
    sign_main_out(&held, now);
    let due = codex_default_only("");
    codex_pool_store(&due)
        .transact_logins(now - 10, &mut None, |_, logins| {
            crate::harness::accounts::note_login_notice(logins, "main", 2, now - 10);
            Ok(Vec::new())
        })
        .expect("a notice that `main`'s login ends in two days");
    for (case, b, signed_out) in [("held", &held, true), ("due", &due, false)] {
        let env = pooled_env(b, &[], true);
        let (row, f, h) = codex_look(&env);
        assert_eq!(
            matches!(row.state, State::SignedOut),
            signed_out,
            "{case}: {:?}: {}",
            row.state,
            row.fact
        );
        assert!(row.state.needs_act(row.wanted), "{case}: {:?}", row.state);
        assert_eq!((&row.fix, row.actor), (&None, None), "{case}: {}", row.fact);
        assert!(
            h.by_person.is_empty() && h.person_steps.is_empty() && h.by_aterm.is_empty(),
            "{case}: {h:?}"
        );
        assert_eq!(h.needs_you(), None, "{case}");
        let why = crate::ai::login::refusal(&f, &row).expect("a refusal");
        let line = format!("codex: not started: {why}");
        let at_tty = {
            let runner = FakeRunner::quiet();
            let (code, out, err) = run_cli(&env, &runner, &["login", "codex"]);
            assert!(runner.calls().is_empty(), "{case}: {:?}", runner.calls());
            assert_eq!(code, 2, "{case}: {out}{err}");
            (code, out, err)
        };
        assert!(at_tty.1.lines().any(|l| l == line), "{case}: {}", at_tty.1);
        let quiet = pooled_env(b, &[], false);
        let runner = FakeRunner::quiet();
        let none = run_cli(&quiet, &runner, &["login", "codex"]);
        assert!(runner.calls().is_empty(), "{case}: {:?}", runner.calls());
        assert_eq!(
            none, at_tty,
            "{case}: no terminal refuses as a terminal does"
        );
        let (_, out, err) = run_cli(&quiet, &runner, &["doctor"]);
        assert!(runner.calls().is_empty(), "{case}: {:?}", runner.calls());
        assert!(
            out.lines().all(|l| !l.starts_with("NEEDS YOU:")),
            "{case}: {out}{err}"
        );
        assert!(out.lines().any(|l| l == line), "{case}: {out}{err}");
    }
}

/// The owner's pin on `label` in `b`'s Codex pool, as `accounts use` sets it.
fn pin_codex(b: &Box3, label: &str, now: i64) {
    use crate::harness::accounts::{self, Cli, Roster};
    let roster = Roster::load_for(Cli::Codex, &b.dir.path().join("accounts-codex.toml"))
        .expect("codex roster");
    let enrolled = accounts::enrolled_in(Cli::Codex, &roster, &b.home);
    codex_pool_store(b)
        .transact_within(&accounts::LOCK_LADDER, now, &mut None, |state| {
            accounts::pin(state, &roster, &enrolled, label, now).map(|event| vec![event])
        })
        .expect("a pin");
}

/// WITH NO TERMINAL, `aterm ai login codex` UNDER A PIN THAT REFUSES EVERY LAUNCH NAMES THE
/// PERSON'S MEND, NOT A SIGN-IN: on a Codex build that keeps no row's login its own, under the
/// owner's pin on `work` (a row of its own), with `work`'s login gone (the pool's next sign-in is
/// `work`'s, which that build never runs) or with `main` signed out (its sign-in is needed but
/// does not move the pin), the row's step is its mend, and `login codex` names that mend and the
/// launch's refusal, runs nothing and exits 2, though a terminal's run would start a sign-in.
/// CONTROL: with no pin and `main` signed out, the row has no mend, and `login codex` sends a
/// person to `aterm ai login codex` at a terminal.
#[test]
fn with_no_terminal_a_login_under_a_refusing_pin_names_the_mend() {
    use crate::ai::facts::LaunchEnd;
    let now = 1_800_000_000;
    let gone = codex_default_only("");
    pin_codex(&gone, "work", now);
    std::fs::remove_file(gone.home.join(".aterm/logins/codex/work/auth.json"))
        .expect("work's login");
    let main_out = codex_default_only("");
    pin_codex(&main_out, "work", now);
    sign_main_out(&main_out, now);
    for (case, b) in [("work's login gone", &gone), ("main signed out", &main_out)] {
        let env = pooled_env(b, &[], false);
        let (row, f, _) = codex_look(&env);
        let pool = f.pool.as_ref().expect("codex pool");
        assert_eq!(pool.launch_end, Some(LaunchEnd::Owner), "{case}: {pool:?}");
        let mend = row
            .mend
            .clone()
            .unwrap_or_else(|| panic!("{case}: a person's mend: {}", row.fact));
        assert_eq!(
            crate::ai::login::refusal(&f, &row),
            None,
            "{case}: PRECONDITION: a terminal's run starts a sign-in"
        );
        let refused = pool.launch_refused.as_deref().expect("a refused launch");
        let refused = refused
            .strip_suffix(crate::harness::accounts_pool::NO_WORKER)
            .unwrap_or(refused);
        let runner = FakeRunner::quiet();
        let (code, out, err) = run_cli(&env, &runner, &["login", "codex"]);
        assert!(runner.calls().is_empty(), "{case}: {:?}", runner.calls());
        assert_eq!(code, 2, "{case}: {out}{err}");
        assert!(
            err.contains(&mend) && err.contains(refused),
            "{case}: {out}{err}"
        );
    }
    let b = codex_default_only("");
    sign_main_out(&b, now);
    let env = pooled_env(&b, &[], false);
    let (row, _, _) = codex_look(&env);
    assert_eq!(row.mend, None, "CONTROL: {}", row.fact);
    let runner = FakeRunner::quiet();
    let (code, out, err) = run_cli(&env, &runner, &["login", "codex"]);
    assert!(runner.calls().is_empty(), "CONTROL: {:?}", runner.calls());
    assert_eq!(code, 2, "CONTROL: {out}{err}");
    assert!(err.contains("`aterm ai login codex`"), "CONTROL: {err}");
}

/// ON A CODEX BUILD THAT KEEPS NO ROW'S LOGIN ITS OWN, UNDER THE OWNER'S PIN ON `work` (A ROW OF
/// ITS OWN), WITH `main` (THE ONLY ROW IT RUNS) SIGNED OUT, CODEX IS SIGNED OUT AS ATERM RUNS IT:
/// the doctor's row reads `signed-out`, whatever `work`'s records and login file say, and its
/// step is the two that end the refusal, `main`'s sign-in at a terminal and then its pin, in that
/// order, as the verdict's person's step too; `aterm ai login codex` with no terminal names that
/// step and says the sign-in alone does not end the refusal. CONTROL: with `main` signed in and
/// `work`'s login gone, the row is not signed out, its step is the pin alone, and the login names
/// that step with no "alone".
#[test]
fn under_a_pin_on_a_row_of_its_own_a_signed_out_default_row_signs_codex_out() {
    use crate::harness::codex_alab::{DEFAULT_SIGN_IN, unread_fix};
    let now = 1_800_000_000;
    let b = codex_default_only("");
    pin_codex(&b, "work", now);
    sign_main_out(&b, now);
    let env = pooled_env(&b, &[], false);
    let (row, f, health) = codex_look(&env);
    assert_eq!(row.state, State::SignedOut, "{}", row.fact);
    assert_eq!(row.fix, None, "{}", row.fact);
    assert_eq!(state::sign_in_then_pin(&f), Some(("main", true)));
    let mend = row.mend.clone().expect("a person's step");
    let sign_in = mend
        .find(&format!("`{DEFAULT_SIGN_IN}`"))
        .unwrap_or_else(|| panic!("the sign-in: {mend}"));
    let pin = mend
        .find(&format!("`{}`", unread_fix(Some("main"))))
        .unwrap_or_else(|| panic!("the pin: {mend}"));
    assert!(sign_in < pin, "{mend}");
    assert!(health.mends.contains(&mend), "{health:?}");
    let runner = FakeRunner::quiet();
    let (code, out, err) = run_cli(&env, &runner, &["login", "codex"]);
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains(&mend) && err.contains(" alone "), "{err}");

    let b = codex_default_only("");
    pin_codex(&b, "work", now);
    std::fs::remove_file(b.home.join(".aterm/logins/codex/work/auth.json")).expect("work's login");
    let env = pooled_env(&b, &[], false);
    let (row, f, _) = codex_look(&env);
    assert_ne!(row.state, State::SignedOut, "CONTROL: {}", row.fact);
    assert_eq!(state::sign_in_then_pin(&f), None, "CONTROL");
    let mend = row.mend.clone().expect("CONTROL: the re-pin");
    assert!(!mend.contains(DEFAULT_SIGN_IN), "CONTROL: {mend}");
    let (code, out, err) = run_cli(&env, &FakeRunner::quiet(), &["login", "codex"]);
    assert_eq!(code, 2, "CONTROL: {out}{err}");
    assert!(
        err.contains(&mend) && !err.contains(" alone "),
        "CONTROL: {err}"
    );
}

/// ON A CODEX BUILD THAT KEEPS NO ROW'S LOGIN ITS OWN, THE ROW ON THIS MACHINE'S OWN LOGIN WITH
/// NO LOGIN STORED HERE IS NOT SIGNED IN "AGAIN": whether none was ever made or one the pool
/// recorded signed out was removed since, the doctor's pool words say no login of it is stored
/// here and its sign-in signs it in. CONTROL: recorded signed out with its login file still here,
/// its sign-in signs it in again.
#[test]
fn a_default_row_with_no_login_stored_here_is_not_signed_in_again() {
    let (said, _) = crate::harness::codex_alab::signed_out_says(false);
    for recorded in [false, true] {
        let b = codex_default_only("");
        if recorded {
            sign_main_out(&b, 1_800_000_000);
        }
        std::fs::remove_file(b.home.join(".codex/auth.json")).expect("the default login");
        let env = pooled_env(&b, &[], false);
        let (row, _, _) = codex_look(&env);
        assert_eq!(row.state, State::SignedOut, "{recorded}: {}", row.fact);
        assert!(!row.fact.contains("in again"), "{recorded}: {}", row.fact);
        assert!(row.fact.contains(said), "{recorded}: {}", row.fact);
    }

    let b = codex_default_only("");
    sign_main_out(&b, 1_800_000_000);
    let env = pooled_env(&b, &[], false);
    let (row, _, _) = codex_look(&env);
    assert!(row.fact.contains("in again"), "CONTROL: {}", row.fact);
}

/// A FACT LONGER THAN THE TEXT ROW HOLDS IS CUT AFTER A WHOLE WORD AND MARKED: under the owner's
/// pin on `work` on a Codex build that keeps no row's login its own, the row's fact (its launch's
/// refusal appended) runs past [`FACT_MAX`], and the text row ends in [`FACT_MORE`] after a
/// whole word of the fact `--json` carries whole. CONTROL: a fact that fits is said whole, and a
/// first word longer than the room is cut at a character, marked too.
#[test]
fn a_long_fact_is_cut_after_a_whole_word_and_marked() {
    let b = codex_default_only("");
    pin_codex(&b, "work", 1_800_000_000);
    let env = pooled_env(&b, &[], false);
    let (row, _, _) = codex_look(&env);
    assert!(row.fact.len() > FACT_MAX, "PRECONDITION: {}", row.fact);
    let runner = FakeRunner::quiet();
    let (_, out, _) = run_cli(&env, &runner, &["doctor", "--read-only"]);
    let lead = format!(
        "{}{}",
        pad("codex:", LABEL_W),
        pad(row.state.word(), STATE_W)
    );
    let said = out
        .lines()
        .find_map(|l| l.strip_prefix(lead.as_str()))
        .unwrap_or_else(|| panic!("a codex row: {out}"));
    let head = said
        .strip_suffix(FACT_MORE)
        .unwrap_or_else(|| panic!("a marked cut: {said}"));
    assert!(said.len() <= FACT_MAX, "{said}");
    let (_, json, _) = run_cli(&env, &runner, &["--json"]);
    let v: Value = aterm_json::from_str(json.trim()).expect("json");
    let whole = v["clis"]
        .as_array()
        .and_then(|clis| clis.iter().find(|c| c["id"] == Value::from("codex")))
        .and_then(|c| c["fact"].as_str())
        .expect("codex's fact")
        .to_owned();
    let rest = whole
        .strip_prefix(head)
        .unwrap_or_else(|| panic!("the text says a head of `--json`'s fact: {head}"));
    assert!(
        !head.ends_with(char::is_alphanumeric) || !rest.starts_with(char::is_alphanumeric),
        "cut inside a word: {head}|{rest}"
    );
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    assert_eq!(
        fact_line("signed in \u{b7} used 5 min ago"),
        "signed in \u{b7} used 5 min ago",
        "CONTROL"
    );
    let word = "x".repeat(FACT_MAX * 2);
    let cut = fact_line(&word);
    assert!(
        cut.len() <= FACT_MAX && cut.ends_with(FACT_MORE) && cut.starts_with('x'),
        "CONTROL: {cut}"
    );
    // A fact of whole words whose room ends inside one, whatever the home's path: no cut word.
    let words = "word ".repeat(FACT_MAX / 4);
    let cut = fact_line(&words);
    let head = cut
        .strip_suffix(FACT_MORE)
        .unwrap_or_else(|| panic!("a marked cut: {cut}"));
    assert!(
        cut.len() <= FACT_MAX && !head.is_empty() && head.split(' ').all(|w| w == "word"),
        "a word cut short: {cut}"
    );
}

/// Claude Code alone: every other connection is not used here.
const ONLY_CLAUDE: &str = "[ai]\npreferred = \"claude\"\n[ai.codex]\nuse = false\n[ai.agy]\nuse = \
                           false\n[ai.muse]\nuse = false\n[ai.grok]\nuse = false\n[ai.kimi]\nuse \
                           = false\n";

/// A Claude-only machine whose accounts.toml beside its config holds `roster`, read through the
/// harness state root, with `extra` in its environment and a terminal at stdin or none.
fn claude_env_with(b: &Box3, roster: &str, extra: &[(&str, &str)], tty: bool) -> Env {
    std::fs::write(b.dir.path().join("accounts.toml"), roster).expect("claude roster");
    let mut env = env_of(b, ONLY_CLAUDE, extra, tty);
    env.harness_state = Some(b.state.clone());
    env
}

/// [`claude_env_with`] with nothing else in its environment.
fn claude_env(b: &Box3, roster: &str, tty: bool) -> Env {
    claude_env_with(b, roster, &[], tty)
}

/// The lines of the doctor's last look that hand a person a step: its rows' `fix:` lines, the
/// closing `health:` line and the `NEEDS YOU:` line.
fn closing_steps(out: &str) -> Vec<&str> {
    let last = out.rfind("\npreferred:").map_or(out, |at| &out[at..]);
    last.lines()
        .map(str::trim_start)
        .filter(|l| {
            l.starts_with("fix:") || l.starts_with("health:") || l.starts_with("NEEDS YOU:")
        })
        .collect()
}

/// A CLAUDE SIGN-IN ACCOUNTS V2 HAS NOTHING TO RUN FOR IS REFUSED WITH ITS REASON, WITH A TERMINAL
/// OR NONE, AND NOTHING HANDS A PERSON THE COMMAND THAT REFUSES: with accounts.toml refused,
/// accounts v2's records unreadable, accounts.toml listing this machine's own login with no
/// email, or `CLAUDE_CONFIG_DIR` naming a home of its own, a person's run at a terminal starts
/// nothing, so `aterm ai login claude` with no terminal prints the refusal a terminal's run
/// prints ([`crate::ai::login::refusal`]), never sending a person to a terminal, and `aterm ai
/// doctor` with no terminal prints it too. The row offers no command ([`Row::fix`]) but what a
/// person does instead ([`Row::mend`]), which its `fix:` line, the closing `health:` line, the
/// `NEEDS YOU:` line and `--json`'s `mend` say, never `aterm ai login claude`, naming a home of
/// its own whole; unreadable records are never called a refused accounts.toml, and the doctor's
/// note says they cannot be read. CONTROL: with an accounts.toml that lists no row, a person's
/// run at a terminal asks the account's email, so no terminal still sends the person to one, and
/// the `NEEDS YOU:` line names `aterm ai login claude`.
#[test]
fn a_claude_sign_in_accounts_v2_refuses_says_why_with_a_terminal_or_none() {
    for (case, roster, says) in [
        (
            "refused",
            "[[account]\nlabel = \"work\"\n",
            "accounts.toml is refused (",
        ),
        ("records", "", "accounts v2's records cannot be read ("),
        (
            "no email",
            "[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\n",
            "with no email",
        ),
        ("home", "", "CLAUDE_CONFIG_DIR is set ("),
    ] {
        let b = machine(&["claude"]);
        if case == "records" {
            let store = crate::harness::accounts::Store::at(&b.state);
            store.ensure().expect("store");
            std::fs::write(store.state_file(), "{not json").expect("damaged state");
        }
        let own = b.dir.path().join("claude-home").display().to_string();
        let extra: &[(&str, &str)] = if case == "home" {
            &[("CLAUDE_CONFIG_DIR", own.as_str())]
        } else {
            &[]
        };
        let env = claude_env_with(&b, roster, extra, true);
        let l = look(&env);
        let (Some(row), Some(f)) = (
            l.rows.iter().find(|r| r.connector.id == "claude"),
            l.facts.iter().find(|f| f.connector.id == "claude"),
        ) else {
            panic!("{case}: no claude row");
        };
        assert!(row.state.needs_act(row.wanted), "{case}: {:?}", row.state);
        assert_eq!(row.fix, None, "{case}: no command that refuses");
        assert_eq!(row.actor, Some(Actor::Person), "{case}");
        let mend = row.mend.clone().expect("what a person does instead");
        let why = crate::ai::login::refusal(f, row).expect("a refusal");
        assert!(why.contains(says), "{case}: {why}");
        if case == "records" {
            assert!(!why.contains("accounts.toml is refused"), "{case}: {why}");
            for said in [&why, &mend] {
                let points = said.contains("`aterm harness accounts` says why");
                assert!(points, "{case}: {said}");
            }
        }
        if case == "home" {
            assert!(mend.contains(&format!("({own})")), "{case}: {mend}");
        }
        let line = format!("claude: not started: {why}");
        let at_tty = {
            let runner = FakeRunner::quiet();
            let ran = run_cli(&env, &runner, &["login", "claude"]);
            assert!(runner.calls().is_empty(), "{case}: {:?}", runner.calls());
            ran
        };
        assert_eq!(at_tty.0, 2, "{case}: {}{}", at_tty.1, at_tty.2);
        assert!(at_tty.1.lines().any(|l| l == line), "{case}: {}", at_tty.1);
        let quiet = claude_env_with(&b, roster, extra, false);
        let runner = FakeRunner::quiet();
        let none = run_cli(&quiet, &runner, &["login", "claude"]);
        assert!(runner.calls().is_empty(), "{case}: {:?}", runner.calls());
        assert_eq!(
            none, at_tty,
            "{case}: no terminal refuses as a terminal does"
        );
        let (_, out, err) = run_cli(&quiet, &runner, &["doctor"]);
        assert!(runner.calls().is_empty(), "{case}: {:?}", runner.calls());
        assert!(out.lines().any(|l| l == line), "{case}: {out}{err}");
        assert!(
            !out.contains("so a person does"),
            "{case}: the doctor sends nobody to a sign-in that refuses: {out}"
        );
        assert_eq!(
            case == "records",
            out.lines()
                .any(|l| l.starts_with("note:")
                    && l.contains("accounts v2's records cannot be read (")),
            "{case}: {out}"
        );
        let steps = closing_steps(&out);
        assert!(
            steps.contains(&format!("fix: {mend}").as_str()),
            "{case}: {steps:?}"
        );
        assert!(
            steps.contains(&format!("NEEDS YOU: {mend}").as_str()),
            "{case}: {steps:?}"
        );
        assert!(
            steps
                .iter()
                .any(|l| l.starts_with("health:") && l.ends_with(&mend)),
            "{case}: {steps:?}"
        );
        assert!(
            steps.iter().all(|l| !l.contains("aterm ai login claude")),
            "{case}: no step names the command that refuses: {steps:?}"
        );
        let (_, json, _) = run_cli(&quiet, &runner, &["--json"]);
        let v: Value = aterm_json::from_str(json.trim()).expect("json");
        let claude = v["clis"]
            .as_array()
            .and_then(|clis| clis.iter().find(|c| c["id"] == Value::from("claude")))
            .expect("claude row");
        assert_eq!(
            (&claude["fix"], &claude["mend"]),
            (&Value::Null, &Value::from(mend.as_str())),
            "{case}: {json}"
        );
    }
    let b = machine(&["claude"]);
    let quiet = claude_env(&b, "", false);
    let runner = FakeRunner::quiet();
    let (code, out, err) = run_cli(&quiet, &runner, &["login", "claude"]);
    assert!(runner.calls().is_empty(), "CONTROL: {:?}", runner.calls());
    assert_eq!(code, 2, "CONTROL: {out}{err}");
    assert!(
        err.contains("stdin is not a terminal, and no route of aterm's signs claude in"),
        "CONTROL: {out}{err}"
    );
    let (_, out, _) = run_cli(&quiet, &runner, &["doctor", "--read-only"]);
    assert!(
        closing_steps(&out)
            .iter()
            .any(|l| l.starts_with("NEEDS YOU:") && l.contains("`aterm ai login claude`")),
        "CONTROL: {out}"
    );
}
