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

const ONLY_CODEX: &str = "[ai]\npreferred = \"codex\"\n[ai.claude]\nuse = false\n[ai.agy]\nuse = \
                          false\n[ai.muse]\nuse = false\n";

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
        out.contains("grok:       off            not installed: an opt-in program"),
        "an opt-in program nobody installed is off, no need: {out}"
    );
    assert!(
        out.contains("reviews:    on · nobody: no other AI CLI is used here (when codex asks)"),
        "an off row is asked by no review: {out}"
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
    let env = env_of(&b, ONLY_CODEX, &[("CLAUDECODE", "1")], true);
    let runner = FakeRunner::quiet();
    let (code, out, _) = run_cli(&env, &runner, &["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("next: ask a person to run `aterm ai doctor`"),
        "{out}"
    );
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
    let only_agy = "[ai]\npreferred = \"agy\"\n[ai.claude]\nuse = false\n[ai.codex]\nuse = false\n[ai.muse]\nuse = false\n";
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
    let both = "[ai]\n[ai.agy]\nuse = false\n[ai.muse]\nuse = false\n";
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

/// Grok Build and Kimi Code are no connections of `aterm ai`'s yet, but their pools are: `aterm
/// ai login kimi` runs the pool's renewal of its next row where this machine keeps
/// `accounts-kimi.toml`, and says how to add one, running nothing, where it does not.
#[test]
fn login_of_a_pool_cli_that_is_no_connection_runs_its_pools_renewal() {
    let b = machine(&["codex"]);
    let mut env = env_of(&b, ONLY_CODEX, &[], true);
    env.harness_state = Some(b.state.clone());
    let runner = FakeRunner::quiet();
    let (code, out, err) = run_cli(&env, &runner, &["login", "kimi"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(out.contains("keeps no `accounts-kimi.toml`"), "{out}");
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());

    std::fs::write(
        b.dir.path().join("accounts-kimi.toml"),
        "[[account]]\nlabel = \"kimi-one\"\nclass = \"dedicated\"\n\
         email = \"kimi-one@example.com\"\nregion = \"global\"\n",
    )
    .expect("roster");
    let mut unavailable = env.clone();
    unavailable.harness_state = None;
    let (code, out, err) = run_cli(&unavailable, &runner, &["login", "kimi"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(runner.calls().is_empty(), "unknown store starts no login");
    let (code, out, err) = run_cli(&env, &runner, &["login", "kimi"]);
    assert_eq!(code, 0, "{out}{err}");
    let calls = runner.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].terminal && calls[0].program == "aterm",
        "{:?}",
        calls[0]
    );
    assert_eq!(
        calls[0].args,
        ["harness", "accounts", "renew", "kimi-one", "--cli", "kimi"]
    );
    assert!(out.contains("kimi: signed in"), "{out}");

    // Negative: an agent never signs a pool in, nor a person with no terminal, and a word that
    // names no CLI is still refused, the pool CLIs among those it lists.
    let runner = FakeRunner::quiet();
    let agent = env_of(&b, ONLY_CODEX, &[("CLAUDECODE", "1")], true);
    let (code, out, _) = run_cli(&agent, &runner, &["login", "kimi"]);
    assert_eq!(code, 2, "{out}");
    let (code, _, err) = run_cli(
        &env_of(&b, ONLY_CODEX, &[], false),
        &runner,
        &["login", "grok"],
    );
    assert_eq!(code, 2);
    assert!(err.contains("not a terminal"), "{err}");
    let (code, _, err) = run_cli(&env, &runner, &["login", "gpt"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("is no AI CLI") && err.contains("kimi"),
        "{err}"
    );
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

/// `aterm ai login grok` WHERE NOBODY INSTALLED IT RUNS NOTHING: Grok Build is off as an opt-in
/// program, so its sign-in is refused with that fact (exit 2), no install and no vendor login.
#[test]
fn logging_in_an_opt_in_program_nobody_installed_runs_nothing() {
    if !atpkg::vendor_direct::served_here("grok") {
        return;
    }
    let b = machine(&["codex"]);
    let env = env_of(&b, ONLY_CODEX, &[], true);
    let runner = FakeRunner::quiet();
    let (code, out, err) = run_cli(&env, &runner, &["login", "grok"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(out.contains("an opt-in program"), "{out}");
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
        &env_of(&b, ONLY_CODEX, &ssh, true),
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
        &env_of(&b, ONLY_CODEX, &[], true),
        &runner,
        &["login", "grok"],
    );
    assert_eq!(
        terminal(&runner),
        [("grok".to_string(), vec!["login".to_string()])],
        "CONTROL: {out}{err}"
    );
}
