// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Vault adoption (design §6.10): with no agh.toml, the vault's two token files route as if
//! provisioned, each login learned once from agh-fake's loopback GitHub. Every file is under a
//! scratch root (its `home/.secrets` and `config/aterm/agh`), never the real ones.

use std::path::{Path, PathBuf};

use agh::cli::{Captured, Ctx};
use agh::config::Dirs;
use agh::identity::{self, vault};
use agh_fake::{FakeGithub, Fault, Match, RepoSpec};
use aterm_json::Value;

use crate::common;

/// The dev token's own login, which names the dev owner (a made-up one).
const DEV_LOGIN: &str = "fixture-dev";
/// The release token's login.
const RELEASE_LOGIN: &str = "fixture-release-bot";

fn dirs(root: &Path) -> Dirs {
    Dirs::under(root)
}

/// `token` written as the vault writes `slot`'s file: 0600 in a 0700 `~/.secrets`.
fn vault_file(root: &Path, slot: vault::Slot, token: &str) -> PathBuf {
    let path = slot.path(&dirs(root).home);
    let dir = path.parent().expect("the vault directory");
    std::fs::create_dir_all(dir.parent().expect("the home")).expect("the home");
    let name = path.file_name().and_then(|n| n.to_str()).expect("a name");
    if dir.exists() {
        aterm_uds::private_file::write_private(dir, name, format!("{token}\n").as_bytes())
            .expect("the vault file");
        path
    } else {
        common::write_token(dir, name, token)
    }
}

/// Both vault files, their tokens registered with `fake` as `DEV_LOGIN` and `RELEASE_LOGIN`.
fn both(root: &Path, fake: &FakeGithub) -> (String, String) {
    let dev = agh_fake::token("vault-dev");
    let release = agh_fake::token("vault-release");
    fake.add_token(&dev, DEV_LOGIN);
    fake.add_token(&release, RELEASE_LOGIN);
    vault_file(root, vault::Slot::Dev, &dev);
    vault_file(root, vault::Slot::Release, &release);
    (dev, release)
}

fn run(ctx: &mut Ctx, args: &[&str]) -> u8 {
    common::run_cli(ctx, args)
}

fn rows(captured: &Captured) -> Vec<Value> {
    common::stdout_json(captured)
        .as_array()
        .expect("rows")
        .clone()
}

fn row<'a>(rows: &'a [Value], profile: &str) -> &'a Value {
    rows.iter()
        .find(|r| r["profile"] == Value::from(profile))
        .unwrap_or_else(|| panic!("no row {profile}: {rows:?}"))
}

/// The `GET /user` requests `fake` saw.
fn user_gets(fake: &FakeGithub) -> usize {
    fake.seen()
        .iter()
        .filter(|s| s.path_and_query == "/user")
        .count()
}

/// The `next=` value of a note line, unquoted (the steps it carries hold no quote to escape).
fn next_of(line: &str) -> &str {
    let (_, next) = line.rsplit_once(" next=").expect("a next= value");
    next.strip_prefix('"')
        .and_then(|next| next.strip_suffix('"'))
        .unwrap_or(next)
}

fn record(root: &Path) -> Option<String> {
    std::fs::read_to_string(dirs(root).config.join(vault::RECORD_FILE)).ok()
}

/// `whoami --for TARGET --op OP`'s decision: `("send", profile)`, `("anonymous", why)` or
/// `("refuse", code)`.
fn decision(root: &Path, fake: &FakeGithub, target: &str, op: &str) -> (String, String) {
    let (mut ctx, captured) = common::ctx(root, fake, common::env());
    assert_eq!(
        run(&mut ctx, &["whoami", "--for", target, "--op", op]),
        0,
        "{}",
        captured.stderr_text()
    );
    let decision = common::stdout_json(&captured)["decision"].clone();
    let object = decision.as_object().expect("a decision");
    let (key, inner) = object.iter().next().expect("one decision");
    let detail = ["profile", "code", "why"]
        .iter()
        .find_map(|k| inner.get(k).and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();
    (key.clone(), detail)
}

#[test]
fn a_first_call_adopts_both_tokens_learning_each_login_once() {
    let root = common::scratch("vault-first");
    let fake = FakeGithub::start();
    let (dev, release) = both(root.path(), &fake);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["whoami", "--meta"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let stderr = captured.stderr_text();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "a call that adopts prints only its receipt: {stderr}"
    );
    let pairs: Vec<&str> = lines[0].split(' ').collect();
    assert!(
        pairs.contains(&"reqs=2"),
        "the receipt counts the two GET /user adoption sent: {stderr}"
    );
    let listed = rows(&captured);
    let dev_row = row(&listed, "dev");
    assert_eq!(dev_row["from"], Value::from("vault"));
    assert_eq!(dev_row["login"], Value::from(DEV_LOGIN));
    assert_eq!(dev_row["tier"], Value::from("dev"));
    assert_eq!(dev_row["presence"], Value::from("present"));
    assert_eq!(
        dev_row["source"],
        Value::from("file:~/.secrets/gh_access_token")
    );
    assert_eq!(
        dev_row["fingerprint"],
        Value::from(identity::fingerprint(dev.as_bytes()))
    );
    let release_row = row(&listed, "release");
    assert_eq!(release_row["from"], Value::from("vault"));
    assert_eq!(release_row["login"], Value::from(RELEASE_LOGIN));
    assert_eq!(release_row["tier"], Value::from("release"));
    assert_eq!(
        release_row["fingerprint"],
        Value::from(identity::fingerprint(release.as_bytes()))
    );
    assert_eq!(user_gets(&fake), 2, "one GET /user per token");
    let logins: Vec<Option<String>> = fake.seen().iter().map(|s| s.as_login.clone()).collect();
    assert!(logins.contains(&Some(DEV_LOGIN.to_string())), "{logins:?}");
    assert!(
        logins.contains(&Some(RELEASE_LOGIN.to_string())),
        "{logins:?}"
    );

    let dirs = dirs(root.path());
    assert!(!dirs.config_file().exists(), "adoption writes no agh.toml");
    let text = record(root.path()).expect("the record");
    assert!(
        text.contains(DEV_LOGIN) && text.contains(RELEASE_LOGIN),
        "{text}"
    );
    assert!(
        !text.contains(&dev) && !text.contains(&release),
        "no token in the record"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&dirs.config.join(vault::RECORD_FILE)), 0o600);
    assert_eq!(mode(&dirs.config), 0o700);

    fake.clear_seen();
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    assert!(
        fake.seen().is_empty(),
        "a recorded login is not asked again"
    );
    assert_eq!(
        row(&rows(&captured), "dev")["login"],
        Value::from(DEV_LOGIN)
    );
}

#[test]
fn adopted_tokens_keep_the_tier_wall() {
    let root = common::scratch("vault-wall");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    fake.add_repo(
        "fixture-dev/app",
        RepoSpec::private().pull(DEV_LOGIN).push(DEV_LOGIN),
    );
    fake.add_repo("alabsystems/aterm", RepoSpec::public().push(RELEASE_LOGIN));
    fake.add_repo("fixture-ext/lib", RepoSpec::public());

    let send = |profile: &str| ("send".to_string(), profile.to_string());
    let rp = root.path();
    assert_eq!(decision(rp, &fake, "fixture-dev/app", "read"), send("dev"));
    assert_eq!(
        decision(rp, &fake, "fixture-dev/app", "writer-read"),
        send("dev")
    );
    assert_eq!(
        decision(rp, &fake, "alabsystems/aterm", "read"),
        send("dev"),
        "a plain read of the public owner is the dev token's, as the canonical rows route"
    );
    assert_eq!(
        decision(rp, &fake, "alabsystems/aterm", "writer-read"),
        send("release")
    );
    assert_eq!(
        decision(rp, &fake, "fixture-ext/lib", "read"),
        send("dev"),
        "another owner's read is the dev token's first, never anonymous's alone"
    );
    let (kind, _) = decision(rp, &fake, "fixture-ext/lib", "writer-read");
    assert_eq!(kind, "refuse");
    let (kind, _) = decision(rp, &fake, "fixture-ext/lib", "write");
    assert_eq!(kind, "refuse");

    fake.clear_seen();
    for slug in ["fixture-dev/app", "alabsystems/aterm", "fixture-ext/lib"] {
        let (mut ctx, captured) = common::ctx(rp, &fake, common::env());
        assert_eq!(
            run(&mut ctx, &["repo", "view", slug]),
            0,
            "{slug}: {}",
            captured.stderr_text()
        );
    }
    for seen in fake.seen() {
        let path = &seen.path_and_query;
        match seen.as_login.as_deref() {
            Some(DEV_LOGIN) => {}
            Some(RELEASE_LOGIN) => panic!("the release token on a plain read at {path}"),
            None => panic!("an anonymous read at {path}, with the dev token there"),
            Some(other) => panic!("{other} at {path}"),
        }
    }
}

#[test]
fn a_rotation_to_the_same_login_relearns_and_refreshes_the_fingerprint() {
    let root = common::scratch("vault-rotate");
    let fake = FakeGithub::start();
    let first = agh_fake::fine_grained_token("rotate-first");
    fake.add_token(&first, DEV_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &first);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let gh = ctx.github().expect("the client");
    assert!(gh.drafts_vouched("dev"), "{}", captured.stderr_text());

    let second = agh_fake::fine_grained_token("rotate-second");
    fake.add_token(&second, DEV_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &second);
    fake.clear_seen();
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    assert_eq!(captured.stderr_text(), "");
    assert_eq!(
        user_gets(&fake),
        1,
        "the rotated token's login is learned again"
    );
    let rows = rows(&captured);
    let fingerprint = identity::fingerprint(second.as_bytes());
    assert_eq!(
        row(&rows, "dev")["fingerprint"],
        Value::from(fingerprint.as_str())
    );
    assert_eq!(row(&rows, "dev")["login"], Value::from(DEV_LOGIN));
    let text = record(root.path()).expect("the record");
    assert!(text.contains(&fingerprint), "{text}");
    assert!(
        !text.contains(&identity::fingerprint(first.as_bytes())),
        "{text}"
    );
    let gh = ctx.github().expect("the client");
    assert!(
        gh.drafts_vouched("dev"),
        "drafts_fp follows the rotated token"
    );
    assert_eq!(
        decision(root.path(), &fake, "fixture-dev/app", "read"),
        ("send".to_string(), "dev".to_string())
    );
}

#[test]
fn a_rotation_to_another_login_is_refused_until_forgotten() {
    let root = common::scratch("vault-moved");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let before = record(root.path()).expect("the record");

    let other = agh_fake::token("vault-other");
    fake.add_token(&other, "fixture-other");
    vault_file(root.path(), vault::Slot::Dev, &other);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 1);
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(error["kind"], Value::from("refused"));
    let why = error["why"].as_str().expect("why");
    assert!(
        why.contains("fixture-other") && why.contains(DEV_LOGIN),
        "{why}"
    );
    let next = error["next"].as_str().expect("next");
    assert!(next.contains("agh auth remove dev"), "{next}");
    assert_eq!(record(root.path()).as_deref(), Some(before.as_str()));
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    run(&mut ctx, &["policy", "check"]);
    let report = common::stdout_json(&captured);
    let adopted = report["adopted"].as_array().expect("adopted");
    assert_eq!(
        row(adopted, "dev")["state"],
        Value::from("rotated"),
        "policy check, which asks GitHub nothing, shows the rotation, never pending: {report}"
    );
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["repo", "view", "fixture-dev/app"]), 1);
    assert_eq!(
        common::error_json(&captured.stderr_text())["kind"],
        Value::from("refused"),
        "every routed call refuses, never re-routes"
    );

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["auth", "status"]), 1);
    let report = common::stdout_json(&captured);
    let dev = row(report["profiles"].as_array().expect("profiles"), "dev");
    assert!(
        dev["why"]
            .as_str()
            .is_some_and(|w| w.starts_with("login_changed:")),
        "{dev}"
    );

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "dev"]),
        0,
        "{}",
        captured.stderr_text()
    );
    assert!(!dirs(root.path()).config_file().exists());
    assert_eq!(
        decision(root.path(), &fake, "fixture-other/x", "writer-read"),
        ("send".to_string(), "dev".to_string()),
        "forgotten, the token is adopted as its new login"
    );
    let (kind, _) = decision(root.path(), &fake, "fixture-dev/app", "writer-read");
    assert_eq!(
        kind, "refuse",
        "the old owner is no longer the token's to write for"
    );
}

#[test]
fn an_offline_first_call_reads_anonymously_writes_nothing_and_the_next_call_adopts() {
    let root = common::scratch("vault-offline");
    let offline = FakeGithub::start();
    offline.fault_always(Match::api("/user"), Fault::Close);
    let online = FakeGithub::start();
    both(root.path(), &online);
    online.add_repo("fixture-ext/lib", RepoSpec::public());
    offline.add_repo("fixture-ext/lib", RepoSpec::public());

    let (mut ctx, captured) = common::ctx(root.path(), &offline, common::env());
    assert_eq!(
        run(&mut ctx, &["repo", "view", "fixture-ext/lib"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let stderr = captured.stderr_text();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(
        lines[0].starts_with("agh: note vault=dev,release why="),
        "{stderr}"
    );
    assert!(lines[0].contains(" next="), "{stderr}");
    assert!(
        offline
            .seen()
            .iter()
            .filter(|s| s.path_and_query != "/user")
            .all(|s| s.as_login.is_none() && !s.had_authorization),
        "the call reads anonymously"
    );
    assert_eq!(record(root.path()), None, "nothing is recorded");
    assert!(!dirs(root.path()).config_file().exists());

    let (mut ctx, captured) = common::ctx(root.path(), &offline, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let dev = rows(&captured);
    let dev = row(&dev, "dev");
    assert_eq!(dev["presence"], Value::from("present"));
    assert!(
        dev["why"]
            .as_str()
            .is_some_and(|w| w.starts_with("not_adopted:")),
        "{dev}"
    );

    let (mut ctx, captured) = common::ctx(root.path(), &online, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    assert_eq!(captured.stderr_text(), "");
    assert_eq!(
        row(&rows(&captured), "dev")["login"],
        Value::from(DEV_LOGIN)
    );
    assert!(
        record(root.path()).is_some(),
        "the next call adopts and records"
    );
}

/// `--anonymous` sends no token, so it sends none to learn a login either: the call reads as
/// the record stands, writes nothing and prints no note, and the next call adopts as usual.
#[test]
fn an_anonymous_call_sends_no_vault_token_to_learn_its_login() {
    let root = common::scratch("vault-anonymous");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    fake.add_repo("fixture-dev/app", RepoSpec::public());
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(
            &mut ctx,
            &["repo", "view", "fixture-dev/app", "--anonymous"]
        ),
        0,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(captured.stderr_text(), "");
    let seen = fake.seen();
    assert!(!seen.is_empty());
    assert!(
        seen.iter()
            .all(|s| !s.had_authorization && s.as_login.is_none()),
        "{seen:?}"
    );
    assert_eq!(user_gets(&fake), 0);
    assert_eq!(record(root.path()), None);

    assert_eq!(
        decision(root.path(), &fake, "fixture-dev/app", "read"),
        ("send".to_string(), "dev".to_string())
    );
    assert!(record(root.path()).is_some());
}

/// A taped call reads no agh file, the vault's included: it records anonymously beside both
/// vault files, learns nothing and writes no record.
#[test]
fn a_taped_call_adopts_nothing() {
    let root = common::scratch("vault-taped");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    fake.add_repo("fixture-ext/lib", RepoSpec::public());
    let tape = common::tape_path(root.path(), "vault");
    let tape_arg = tape.to_str().expect("a UTF-8 path");
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(
            &mut ctx,
            &["--record", tape_arg, "repo", "view", "fixture-ext/lib"]
        ),
        0,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(captured.stderr_text(), "");
    assert_eq!(user_gets(&fake), 0);
    assert!(fake.seen().iter().all(|s| !s.had_authorization));
    assert_eq!(record(root.path()), None);
}

#[test]
fn a_token_github_refuses_is_not_adopted_and_is_asked_again() {
    let root = common::scratch("vault-401");
    let fake = FakeGithub::start();
    vault_file(
        root.path(),
        vault::Slot::Dev,
        &agh_fake::token("vault-unknown"),
    );
    for _ in 0..2 {
        let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
        assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
        let stderr = captured.stderr_text();
        let lines: Vec<&str> = stderr.lines().collect();
        assert_eq!(lines.len(), 1, "{stderr}");
        assert!(lines[0].starts_with("agh: note vault=dev "), "{stderr}");
        assert!(lines[0].contains("unauthenticated"), "{stderr}");
        assert_eq!(
            next_of(lines[0]),
            vault::REFRESH_STEP,
            "a refused vault token's step is the vault's refresh: {stderr}"
        );
        assert_eq!(
            row(&rows(&captured), "dev")["remedy"],
            Value::from(vault::REFRESH_STEP)
        );
    }
    assert_eq!(user_gets(&fake), 2, "each call asks again");
    assert_eq!(record(root.path()), None);
}

/// A 401 for an adopted token on a real request names the vault's refresh as its step, never
/// an agh.toml edit or a profile to add.
#[test]
fn a_401_for_an_adopted_token_names_the_vault_refresh() {
    let root = common::scratch("vault-401-routed");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    fake.add_repo(
        "fixture-dev/app",
        RepoSpec::private().pull(DEV_LOGIN).push(DEV_LOGIN),
    );
    fake.fault_always(
        Match::api("/repos/fixture-dev/app"),
        Fault::Status {
            status: 401,
            body: "{\"message\":\"Bad credentials\"}".to_string(),
            headers: Vec::new(),
        },
    );
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["repo", "view", "fixture-dev/app"]), 4);
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(error["kind"], Value::from("unauthenticated"), "{error}");
    assert_eq!(error["next"], Value::from(vault::REFRESH_STEP), "{error}");
}

#[test]
fn a_loose_vault_file_is_reported_with_its_remedy_and_never_used() {
    let root = common::scratch("vault-loose");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    let dev = vault::Slot::Dev.path(&dirs(root.path()).home);
    common::chmod(&dev, 0o644);
    fake.add_repo("fixture-dev/app", RepoSpec::public());

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "status"]),
        4,
        "{}",
        captured.stderr_text()
    );
    let report = common::stdout_json(&captured);
    assert_eq!(report["routing"], Value::from("vault"));
    assert_eq!(report["present"], Value::from(false));
    let profiles = report["profiles"].as_array().expect("profiles");
    let dev_row = row(profiles, "dev");
    assert_eq!(dev_row["presence"], Value::from("unusable"));
    assert_eq!(dev_row["from"], Value::from("vault"));
    let remedy = dev_row["remedy"].as_str().expect("a remedy");
    assert!(
        remedy.contains("chmod 600 ~/.secrets/gh_access_token"),
        "{remedy}"
    );
    assert!(!remedy.contains("agh auth add"), "{remedy}");
    assert_eq!(row(profiles, "release")["presence"], Value::from("present"));
    assert_eq!(
        common::error_json(&captured.stderr_text())["kind"],
        Value::from("unauthenticated")
    );

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["repo", "view", "fixture-dev/app"]),
        0,
        "{}",
        captured.stderr_text()
    );
    assert!(
        fake.seen()
            .iter()
            .all(|s| s.as_login.as_deref() != Some(DEV_LOGIN)),
        "a loose token is never sent"
    );
    let stderr = captured.stderr_text();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "one note for the loose token: {stderr}");
    assert!(lines[0].starts_with("agh: note vault=dev why="), "{stderr}");
    assert!(
        lines[0].contains("next=\"chmod 600 ~/.secrets/gh_access_token"),
        "{stderr}"
    );

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_ne!(
        run(&mut ctx, &["policy", "check"]),
        0,
        "a loose vault token is a fault, never ok"
    );
    let report = common::stdout_json(&captured);
    assert_eq!(report["ok"], Value::from(false));
    let faults = report["faults"].as_array().expect("faults");
    assert!(
        faults.iter().any(|f| f["rule"] == Value::from("file")
            && f["what"]
                .as_str()
                .is_some_and(|w| w.contains("chmod 600 ~/.secrets/gh_access_token"))),
        "{faults:?}"
    );
}

#[test]
fn an_explicit_agh_toml_wins_and_is_never_rewritten() {
    let root = common::scratch("vault-explicit");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    let dirs = dirs(root.path());
    for text in ["", "# mine\n[default]\nread = [\"anonymous\"]\n"] {
        common::write_config(&dirs, text);
        for args in [&["whoami"][..], &["auth", "status"], &["policy", "check"]] {
            let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
            assert_eq!(
                run(&mut ctx, args),
                0,
                "{args:?}: {}",
                captured.stderr_text()
            );
            let value = common::stdout_json(&captured);
            let text_out = value.to_string();
            assert!(!text_out.contains("\"vault\""), "{args:?}: {text_out}");
        }
        let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
        assert_eq!(run(&mut ctx, &["auth", "status"]), 0);
        assert_eq!(
            common::stdout_json(&captured)["routing"],
            Value::from("agh.toml")
        );
        assert_eq!(
            std::fs::read_to_string(dirs.config_file()).expect("agh.toml"),
            text
        );
    }
    assert!(fake.seen().is_empty(), "nothing is learned");
    assert_eq!(record(root.path()), None);
    let (kind, _) = decision(root.path(), &fake, "fixture-dev/app", "read");
    assert_eq!(kind, "anonymous");
}

#[test]
fn only_the_release_file_adopts_release_alone() {
    let root = common::scratch("vault-release-only");
    let fake = FakeGithub::start();
    let release = agh_fake::token("vault-release-only");
    fake.add_token(&release, RELEASE_LOGIN);
    vault_file(root.path(), vault::Slot::Release, &release);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let rows = rows(&captured);
    let names: Vec<&str> = rows.iter().filter_map(|r| r["profile"].as_str()).collect();
    assert_eq!(names, ["release"]);
    assert_eq!(
        decision(root.path(), &fake, "alabsystems/aterm", "read"),
        ("send".to_string(), "release".to_string())
    );
    let (kind, _) = decision(root.path(), &fake, "fixture-dev/app", "read");
    assert_eq!(
        kind, "anonymous",
        "the release token reads for no other owner"
    );
    let (kind, _) = decision(root.path(), &fake, "fixture-dev/app", "writer-read");
    assert_eq!(kind, "refuse");
}

#[test]
fn only_the_dev_file_adopts_dev_alone() {
    let root = common::scratch("vault-dev-only");
    let fake = FakeGithub::start();
    let dev = agh_fake::token("vault-dev-only");
    fake.add_token(&dev, DEV_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &dev);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let rows = rows(&captured);
    let names: Vec<&str> = rows.iter().filter_map(|r| r["profile"].as_str()).collect();
    assert_eq!(names, ["dev"]);
    assert_eq!(
        decision(root.path(), &fake, "fixture-dev/app", "read"),
        ("send".to_string(), "dev".to_string())
    );
    assert_eq!(
        decision(root.path(), &fake, "alabsystems/aterm", "read"),
        ("send".to_string(), "dev".to_string()),
        "the dev token reads the public owner's public repositories"
    );
    let (kind, code) = decision(root.path(), &fake, "alabsystems/aterm", "writer-read");
    assert_eq!((kind.as_str(), code.as_str()), ("refuse", "write_none"));

    let next = |slug: &str| {
        let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
        assert_ne!(run(&mut ctx, &["release", "list", slug, "--drafts"]), 0);
        let error = common::error_json(&captured.stderr_text());
        assert_eq!(error["sub"], Value::from("policy.write_none"), "{error}");
        error["next"].as_str().expect("next").to_string()
    };
    let public = next("alabsystems/aterm");
    assert!(
        public.contains(vault::RELEASE_TOKEN_FILE),
        "the public owner's writer is the vault's release file: {public}"
    );
    let other = next("fixture-ext/lib");
    assert!(
        other.contains("fixture-ext") && !other.contains(vault::DEV_TOKEN_FILE),
        "another owner's writer is a profile of its own, never a vault file: {other}"
    );
}

#[test]
fn policy_check_shows_the_adopted_rows_and_asks_github_nothing() {
    let root = common::scratch("vault-check");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    let check = |root: &Path| {
        let (mut ctx, captured) = common::ctx(root, &fake, common::env());
        assert_eq!(
            run(&mut ctx, &["policy", "check"]),
            0,
            "{}",
            captured.stderr_text()
        );
        common::stdout_json(&captured)
    };
    let before = check(root.path());
    assert_eq!(before["present"], Value::from(false));
    let adopted = before["adopted"].as_array().expect("adopted");
    let states: Vec<(&str, &str)> = adopted
        .iter()
        .map(|r| {
            (
                r["profile"].as_str().unwrap_or_default(),
                r["state"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(states, [("dev", "pending"), ("release", "pending")]);
    assert!(fake.seen().is_empty(), "policy check asks GitHub nothing");

    let (mut ctx, _) = common::ctx(root.path(), &fake, common::env());
    ctx.github().expect("adoption");
    let after = check(root.path());
    let adopted = after["adopted"].as_array().expect("adopted");
    assert_eq!(adopted[0]["state"], Value::from("adopted"));
    assert_eq!(adopted[0]["owner"], Value::from(DEV_LOGIN));
    assert_eq!(
        adopted[0]["token_file"],
        Value::from("~/.secrets/gh_access_token")
    );
    assert_eq!(adopted[1]["owner"], Value::from("alabsystems"));
    assert_eq!(adopted[1]["login"], Value::from(RELEASE_LOGIN));
    assert_eq!(adopted[1]["tier"], Value::from("release"));
}

/// `auth add` with no agh.toml writes `vault = true` and its own profile, never the vault's
/// rows as fixed text: the vault's tokens keep joining as adoption derives them, so a rotation
/// after it still relearns (drafts stay vouched) and a rotation to another login still refuses.
#[test]
fn auth_add_with_no_agh_toml_keeps_the_vault_joining_through_rotations() {
    let root = common::scratch("vault-seed");
    let fake = FakeGithub::start();
    let first = agh_fake::fine_grained_token("seed-first");
    fake.add_token(&first, DEV_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &first);
    let release = agh_fake::token("seed-release");
    fake.add_token(&release, RELEASE_LOGIN);
    vault_file(root.path(), vault::Slot::Release, &release);
    let ext = agh_fake::token("vault-ext");
    fake.add_token(&ext, "fixture-ext-bot");
    let dirs = dirs(root.path());
    let file = common::write_token(&root.path().join("ext"), "ext.token", &ext);
    let path = file.display().to_string();
    let add = [
        "auth",
        "add",
        "ext",
        "--file",
        &path,
        "--tier",
        "external",
        "--owner",
        "fixture-ext",
    ];
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &add), 0, "{}", captured.stderr_text());
    let text = std::fs::read_to_string(dirs.config_file()).expect("agh.toml");
    let parsed = agh::config::parse(&text).expect("agh.toml parses");
    assert!(parsed.vault, "{text}");
    let names: Vec<&str> = parsed.profiles.keys().map(String::as_str).collect();
    assert_eq!(names, ["ext"], "no vault row is written: {text}");
    assert!(!text.contains(DEV_LOGIN), "{text}");
    assert!(!text.contains(&ext), "no token in agh.toml");

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let listed = rows(&captured);
    assert_eq!(row(&listed, "dev")["from"], Value::from("vault"));
    assert_eq!(row(&listed, "release")["from"], Value::from("vault"));
    assert_eq!(row(&listed, "ext")["from"], Value::from("agh.toml"));
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "status"]),
        0,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(
        common::stdout_json(&captured)["routing"],
        Value::from("agh.toml+vault")
    );
    let send = |profile: &str| ("send".to_string(), profile.to_string());
    let rp = root.path();
    assert_eq!(decision(rp, &fake, "fixture-ext/lib", "read"), send("ext"));
    assert_eq!(
        decision(rp, &fake, "fixture-dev/app", "writer-read"),
        send("dev")
    );
    assert_eq!(
        decision(rp, &fake, "alabsystems/aterm", "writer-read"),
        send("release")
    );

    let second = agh_fake::fine_grained_token("seed-second");
    fake.add_token(&second, DEV_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &second);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let gh = ctx.github().expect("the client");
    assert!(
        gh.drafts_vouched("dev"),
        "drafts follow the rotated token: {}",
        captured.stderr_text()
    );
    assert_eq!(
        decision(rp, &fake, "fixture-dev/app", "writer-read"),
        send("dev")
    );
    assert_eq!(
        std::fs::read_to_string(dirs.config_file()).expect("agh.toml"),
        text,
        "a rotation leaves agh.toml as it is"
    );

    let other = agh_fake::token("seed-other");
    fake.add_token(&other, "fixture-other");
    vault_file(root.path(), vault::Slot::Dev, &other);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["repo", "view", "fixture-dev/app"]), 1);
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(error["kind"], Value::from("refused"));
    let why = error["why"].as_str().expect("why");
    assert!(
        why.contains("fixture-other") && why.contains(DEV_LOGIN),
        "a changed login refuses, naming both: {why}"
    );
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "dev"]),
        0,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(
        decision(rp, &fake, "fixture-other/x", "writer-read"),
        send("dev"),
        "forgotten, the token is adopted as its new login"
    );
    assert_eq!(decision(rp, &fake, "fixture-ext/lib", "read"), send("ext"));
}

/// Both vault tokens of one login break L7 together, and neither's tier is known: neither is
/// sent, the fault is named once on stderr with the vault's step, both are reported, and the
/// CLI, auth add included, keeps working.
#[test]
fn two_vault_tokens_of_one_login_route_neither() {
    let root = common::scratch("vault-one-login");
    let fake = FakeGithub::start();
    let dev = agh_fake::token("one-login-dev");
    let release = agh_fake::token("one-login-release");
    fake.add_token(&dev, DEV_LOGIN);
    fake.add_token(&release, DEV_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &dev);
    vault_file(root.path(), vault::Slot::Release, &release);
    fake.add_repo("fixture-ext/lib", RepoSpec::public());
    left_out_both(root.path(), &fake, "both speak as");

    let ext = agh_fake::token("one-login-ext");
    fake.add_token(&ext, "fixture-ext-bot");
    let file = common::write_token(&root.path().join("ext"), "ext.token", &ext);
    let path = file.display().to_string();
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(
            &mut ctx,
            &["auth", "add", "ext", "--file", &path, "--tier", "external"]
        ),
        0,
        "{}",
        captured.stderr_text()
    );
}

/// One token written to both vault files: its tier is unknown, so neither file routes and
/// agh asks GitHub nothing for it; each call's note, auth status and policy check name both.
#[test]
fn one_token_in_both_vault_files_routes_neither() {
    let root = common::scratch("vault-one-token");
    let fake = FakeGithub::start();
    let token = agh_fake::token("one-token");
    fake.add_token(&token, RELEASE_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &token);
    vault_file(root.path(), vault::Slot::Release, &token);
    fake.add_repo("fixture-ext/lib", RepoSpec::public());
    left_out_both(root.path(), &fake, "hold one token");
    assert_eq!(user_gets(&fake), 0, "no login is learned for it");
    assert_eq!(record(root.path()), None);
}

/// With both vault files left out for one L7 fault (`why` in it): a routed call is anonymous
/// and prints one note naming both, with the vault's step; auth status reports both unusable
/// with that step; policy check reports the L7 fault with it as its next.
fn left_out_both(root: &Path, fake: &FakeGithub, why: &str) {
    let (mut ctx, captured) = common::ctx(root, fake, common::env());
    assert_eq!(
        run(&mut ctx, &["repo", "view", "fixture-ext/lib"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let stderr = captured.stderr_text();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(
        lines[0].starts_with("agh: note vault=dev,release why="),
        "{stderr}"
    );
    assert!(
        lines[0].contains("L7") && lines[0].contains(why),
        "{stderr}"
    );
    assert_eq!(next_of(lines[0]), vault::pair_next(), "{stderr}");
    assert!(
        fake.seen()
            .iter()
            .filter(|s| s.path_and_query != "/user")
            .all(|s| s.as_login.is_none() && !s.had_authorization),
        "neither token is sent"
    );
    for (target, op) in [
        ("fixture-dev/app", "writer-read"),
        ("alabsystems/aterm", "writer-read"),
    ] {
        let (kind, _) = decision(root, fake, target, op);
        assert_eq!(kind, "refuse", "{target}");
    }
    assert_eq!(
        decision(root, fake, "fixture-ext/lib", "read").0,
        "anonymous"
    );

    let (mut ctx, captured) = common::ctx(root, fake, common::env());
    assert_eq!(run(&mut ctx, &["auth", "status"]), 1);
    let report = common::stdout_json(&captured);
    let profiles = report["profiles"].as_array().expect("profiles");
    for name in ["dev", "release"] {
        let listed = row(profiles, name);
        assert_eq!(listed["presence"], Value::from("unusable"), "{listed}");
        assert!(
            listed["why"]
                .as_str()
                .is_some_and(|w| w.starts_with("rule_fault: L7")),
            "{listed}"
        );
        assert_eq!(
            listed["remedy"],
            Value::from(vault::pair_next()),
            "{listed}"
        );
    }
    assert_eq!(
        common::error_json(&captured.stderr_text())["next"],
        Value::from(vault::pair_next())
    );
    let (mut ctx, captured) = common::ctx(root, fake, common::env());
    assert_ne!(run(&mut ctx, &["policy", "check"]), 0);
    let report = common::stdout_json(&captured);
    let faults = report["faults"].as_array().expect("faults");
    assert_eq!(
        faults
            .iter()
            .filter(|f| f["rule"] == Value::from("L7"))
            .count(),
        1,
        "{report}"
    );
    for adopted in report["adopted"].as_array().expect("adopted") {
        assert_eq!(adopted["state"], Value::from("faulted"), "{adopted}");
        assert_eq!(adopted["remedy"], Value::from(vault::pair_next()));
    }
    assert_eq!(
        common::error_json(&captured.stderr_text())["next"],
        Value::from(vault::pair_next()),
        "policy check's step is the vault's, since no row of agh's is to fix"
    );
}

/// The token a call sends is the one adoption judged: a vault file rotated to another login's
/// token after the client is built never speaks for the rows learned from the old one.
#[test]
fn a_file_rotated_after_adoption_never_reroutes_the_call() {
    let root = common::scratch("vault-pinned");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let gh = ctx.github().expect("the client");
    assert_eq!(captured.stderr_text(), "");
    let other = agh_fake::token("pinned-other");
    fake.add_token(&other, "fixture-other");
    vault_file(root.path(), vault::Slot::Dev, &other);
    fake.clear_seen();
    let me = gh.users().me("dev").expect("GET /user as dev");
    assert_eq!(me.login, DEV_LOGIN);
    let seen = fake.seen();
    assert!(!seen.is_empty());
    assert!(
        seen.iter()
            .all(|s| s.as_login.as_deref() == Some(DEV_LOGIN)),
        "{seen:?}"
    );
}

/// `auth add dev` or `auth add release` with no agh.toml beside the vault's files writes a
/// plain agh.toml that routes alone (no `vault = true`), so the vault file it names is routed
/// once; beside an agh.toml that says `vault = true` it refuses as usage before it sends
/// anything.
#[test]
fn auth_add_of_a_vault_profile_with_no_agh_toml_routes_alone() {
    let root = common::scratch("vault-add-dev");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    let dirs = dirs(root.path());
    let dev_file = vault::Slot::Dev.path(&dirs.home).display().to_string();
    let add_dev = [
        "auth", "add", "dev", "--file", &dev_file, "--tier", "dev", "--owner", DEV_LOGIN, "--write",
    ];
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &add_dev), 0, "{}", captured.stderr_text());
    let text = std::fs::read_to_string(dirs.config_file()).expect("agh.toml");
    let parsed = agh::config::parse(&text).expect("agh.toml parses");
    assert!(!parsed.vault, "no vault key: {text}");
    assert!(!text.contains("vault"), "{text}");
    let names: Vec<&str> = parsed.profiles.keys().map(String::as_str).collect();
    assert_eq!(names, ["dev"]);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "status"]),
        0,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(
        common::stdout_json(&captured)["routing"],
        Value::from("agh.toml")
    );
    let release_file = vault::Slot::Release.path(&dirs.home).display().to_string();
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(
            &mut ctx,
            &[
                "auth",
                "add",
                "release",
                "--file",
                &release_file,
                "--tier",
                "release",
                "--owner",
                "alabsystems",
                "--write",
            ]
        ),
        0,
        "{}",
        captured.stderr_text()
    );

    common::write_config(&dirs, "vault = true\n");
    fake.clear_seen();
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &add_dev), 2, "{}", captured.stderr_text());
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(error["kind"], Value::from("usage"));
    assert!(
        error["why"]
            .as_str()
            .is_some_and(|w| w.contains("~/.secrets/gh_access_token")),
        "{error}"
    );
    assert!(fake.seen().is_empty(), "a refused add sends nothing");
    assert_eq!(
        std::fs::read_to_string(dirs.config_file()).expect("agh.toml"),
        "vault = true\n"
    );
}

/// A learned login that cannot be recorded (the agh directory open to others) is reported:
/// each call prints a note with the chmod that fixes it, and policy check a `file` fault.
#[test]
fn a_record_agh_cannot_write_is_reported() {
    let root = common::scratch("vault-unrecorded");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    let dirs = dirs(root.path());
    std::fs::create_dir_all(&dirs.config).expect("the agh directory");
    common::chmod(&dirs.config, 0o755);
    for _ in 0..2 {
        let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
        assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
        let stderr = captured.stderr_text();
        let lines: Vec<&str> = stderr.lines().collect();
        assert_eq!(lines.len(), 1, "{stderr}");
        assert!(
            lines[0].starts_with("agh: note vault=dev,release why="),
            "{stderr}"
        );
        assert!(next_of(lines[0]).contains("chmod 700"), "{stderr}");
    }
    assert_eq!(record(root.path()), None);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_ne!(run(&mut ctx, &["policy", "check"]), 0);
    let report = common::stdout_json(&captured);
    let rules: Vec<&str> = report["faults"]
        .as_array()
        .expect("faults")
        .iter()
        .filter_map(|f| f["rule"].as_str())
        .collect();
    assert_eq!(rules, ["file"], "{report}");
}

/// `auth add` provisions while every routed call refuses (a vault token rotated to another
/// login): it binds its profile on a client that never adopts.
#[test]
fn auth_add_works_while_adoption_refuses() {
    let root = common::scratch("vault-add-refused");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let other = agh_fake::token("add-refused-other");
    fake.add_token(&other, "fixture-other");
    vault_file(root.path(), vault::Slot::Dev, &other);
    let (mut ctx, _) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 1, "adoption refuses");

    let ext = agh_fake::token("add-refused-ext");
    fake.add_token(&ext, "fixture-ext-bot");
    let file = common::write_token(&root.path().join("ext"), "ext.token", &ext);
    let path = file.display().to_string();
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(
            &mut ctx,
            &[
                "auth",
                "add",
                "ext",
                "--file",
                &path,
                "--tier",
                "external",
                "--owner",
                "fixture-ext",
            ]
        ),
        0,
        "{}",
        captured.stderr_text()
    );
    assert!(dirs(root.path()).config_file().exists());
}

/// The `GET /user` that learns a login is sent once under the production retry policy, so an
/// offline call pauses for no retry: it reads anonymously at once, and the next call asks again.
/// Once the dev token's learn could not reach GitHub, the release token's is not sent.
#[test]
fn an_offline_learn_is_sent_once_with_no_retry_pause() {
    let root = common::scratch("vault-no-retry");
    let offline = FakeGithub::start();
    offline.fault_always(Match::api("/user"), Fault::Close);
    offline.add_repo("fixture-ext/lib", RepoSpec::public());
    both(root.path(), &offline);
    let (mut ctx, captured) = common::ctx(root.path(), &offline, common::env());
    ctx.retry = agh::retry::RetryPolicy::STANDARD;
    let fx = ctx.fx.clone();
    assert_eq!(
        run(&mut ctx, &["repo", "view", "fixture-ext/lib"]),
        0,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(fx.slept(), Vec::new(), "no retry pause before the call");
    assert_eq!(
        user_gets(&offline),
        1,
        "once GitHub cannot be reached, the release token waits out no second connect: {:?}",
        offline.seen()
    );
    let stderr = captured.stderr_text();
    assert!(
        stderr.starts_with("agh: note vault=dev,release why="),
        "both tokens are named unlearned: {stderr}"
    );
    assert_eq!(record(root.path()), None);
}

/// A call that learns a login while another agh call holds the record's lock routes adopted
/// and records nothing; the next call records. An auth edit's lock never stands in its way,
/// and an adoption's lock never stands in an auth edit's.
#[test]
fn the_record_lock_is_adoptions_own() {
    let root = common::scratch("vault-lock");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    let dirs = dirs(root.path());
    {
        let _held = vault::hold_record_lock_for_tests(&dirs).expect("the record lock");
        let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
        assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
        assert_eq!(
            captured.stderr_text(),
            "",
            "a held record lock is another call's write, which the next call outlasts: no note"
        );
        assert_eq!(
            row(&rows(&captured), "dev")["login"],
            Value::from(DEV_LOGIN)
        );
        let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
        assert_eq!(
            run(&mut ctx, &["policy", "check"]),
            0,
            "{}",
            captured.stderr_text()
        );
        let report = common::stdout_json(&captured);
        assert!(
            report["faults"].as_array().is_some_and(Vec::is_empty),
            "a held record lock is no fault: {report}"
        );
        assert_eq!(
            record(root.path()),
            None,
            "nothing is recorded under a held lock"
        );

        let ext = agh_fake::token("lock-ext");
        fake.add_token(&ext, "fixture-ext-bot");
        let file = common::write_token(&root.path().join("ext"), "ext.token", &ext);
        let path = file.display().to_string();
        let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
        assert_eq!(
            run(
                &mut ctx,
                &["auth", "add", "ext", "--file", &path, "--tier", "external"]
            ),
            0,
            "auth add never waits on adoption: {}",
            captured.stderr_text()
        );
    }
    {
        let _held = identity::provision::hold_auth_edit_for_tests(&dirs).expect("the auth lock");
        let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
        assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    }
    let text = record(root.path()).expect("the next call records");
    assert!(text.contains(DEV_LOGIN), "{text}");
}

#[test]
fn auth_remove_of_an_unadopted_name_with_no_agh_toml_is_still_usage() {
    let root = common::scratch("vault-remove");
    let fake = FakeGithub::start();
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["auth", "remove", "dev"]), 2);
    assert_eq!(
        common::error_json(&captured.stderr_text())["kind"],
        Value::from("usage")
    );
}

/// Neither vault file's token routes while it is the token the record keeps for the other file,
/// or speaks as the login the record keeps for it, whatever the other file holds this call: a
/// routed call is anonymous and notes `names` with the vault's step, auth status reports one L7
/// fault with that step, and the record is left as it was. `by_fingerprint`: the record's
/// fingerprint alone shows it, so policy check, which asks GitHub nothing, reports it too.
fn crossed(root: &Path, fake: &FakeGithub, names: &str, by_fingerprint: bool) {
    let before = record(root);
    fake.add_repo("fixture-ext/lib", RepoSpec::public());
    let (mut ctx, captured) = common::ctx(root, fake, common::env());
    assert_eq!(
        run(&mut ctx, &["repo", "view", "fixture-ext/lib"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let stderr = captured.stderr_text();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(
        lines[0].starts_with(&format!("agh: note vault={names} why=")),
        "{stderr}"
    );
    assert_eq!(next_of(lines[0]), vault::pair_next(), "{stderr}");
    assert!(
        fake.seen()
            .iter()
            .filter(|s| s.path_and_query != "/user")
            .all(|s| s.as_login.is_none() && !s.had_authorization),
        "neither token is sent: {:?}",
        fake.seen()
    );
    for target in ["fixture-dev/app", "alabsystems/aterm"] {
        let (kind, _) = decision(root, fake, target, "writer-read");
        assert_eq!(kind, "refuse", "{target}");
    }
    assert_eq!(
        decision(root, fake, "fixture-ext/lib", "read").0,
        "anonymous"
    );
    let rules = |report: &Value| -> Vec<String> {
        report["faults"]
            .as_array()
            .expect("faults")
            .iter()
            .filter_map(|f| f["rule"].as_str().map(str::to_string))
            .collect()
    };
    let (mut ctx, captured) = common::ctx(root, fake, common::env());
    assert_ne!(run(&mut ctx, &["auth", "status"]), 0);
    let report = common::stdout_json(&captured);
    assert_eq!(rules(&report), ["L7"], "{report}");
    for name in names.split(',') {
        let listed = row(report["profiles"].as_array().expect("profiles"), name);
        assert_eq!(listed["presence"], Value::from("unusable"), "{listed}");
        assert_eq!(
            listed["remedy"],
            Value::from(vault::pair_next()),
            "{listed}"
        );
    }
    if by_fingerprint {
        let (mut ctx, captured) = common::ctx(root, fake, common::env());
        assert_ne!(run(&mut ctx, &["policy", "check"]), 0);
        let report = common::stdout_json(&captured);
        assert_eq!(rules(&report), ["L7"], "{report}");
        for adopted in report["adopted"].as_array().expect("adopted") {
            if adopted["state"] != Value::from("absent") {
                assert_eq!(adopted["state"], Value::from("faulted"), "{adopted}");
                assert_eq!(adopted["remedy"], Value::from(vault::pair_next()));
            }
        }
    }
    assert_eq!(record(root), before, "the record is left as it was");
}

/// (a) The release file gone and its token in the dev file, with no dev row: the dev slot's
/// rows would make the release token every owner's first reader, so it never routes, and it is
/// never sent to learn a login.
#[test]
fn the_release_token_in_the_dev_file_routes_neither() {
    let root = common::scratch("vault-crossed-release");
    let fake = FakeGithub::start();
    let release = agh_fake::token("crossed-release");
    fake.add_token(&release, RELEASE_LOGIN);
    let fingerprint = identity::fingerprint(release.as_bytes());
    vault::record_for_tests(
        &dirs(root.path()),
        &[(vault::Slot::Release, fingerprint.as_str(), RELEASE_LOGIN)],
    )
    .expect("the record");
    vault_file(root.path(), vault::Slot::Dev, &release);
    crossed(root.path(), &fake, "dev", true);
    assert_eq!(user_gets(&fake), 0, "the crossed token is never sent");
}

/// (b) The dev file gone and its token in the release file, with no release row: the release
/// slot's rows would make the dev token alabsystems' writer, so it never routes.
#[test]
fn the_dev_token_in_the_release_file_routes_neither() {
    let root = common::scratch("vault-crossed-dev");
    let fake = FakeGithub::start();
    let dev = agh_fake::token("crossed-dev");
    fake.add_token(&dev, DEV_LOGIN);
    let fingerprint = identity::fingerprint(dev.as_bytes());
    vault::record_for_tests(
        &dirs(root.path()),
        &[(vault::Slot::Dev, fingerprint.as_str(), DEV_LOGIN)],
    )
    .expect("the record");
    vault_file(root.path(), vault::Slot::Release, &dev);
    crossed(root.path(), &fake, "release", true);
    assert_eq!(user_gets(&fake), 0, "the crossed token is never sent");
}

/// (c) The dev file holds another token of the release login while GitHub refuses the release
/// file's rotated token (a 401): the dev token speaks as the login the record keeps for the
/// release file, so neither routes, and the dev token's login is not recorded, so every later
/// call judges it again.
#[test]
fn a_dev_token_of_the_recorded_release_login_routes_neither_while_release_is_refused() {
    let root = common::scratch("vault-crossed-login");
    let fake = FakeGithub::start();
    vault::record_for_tests(
        &dirs(root.path()),
        &[(vault::Slot::Release, "0123456789ab", RELEASE_LOGIN)],
    )
    .expect("the record");
    let second = agh_fake::token("crossed-second-release");
    fake.add_token(&second, RELEASE_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &second);
    vault_file(
        root.path(),
        vault::Slot::Release,
        &agh_fake::token("crossed-refused"),
    );
    for _ in 0..2 {
        crossed(root.path(), &fake, "dev,release", false);
    }
}

/// A record that is not agh's (here, one that does not parse) routes neither vault token, so a
/// rotation to another login meanwhile never routes silently: each call is anonymous with a
/// note, policy check reports a `file` fault, nothing rewrites the file, and once it is removed
/// the next call learns each login again.
#[test]
fn a_record_agh_cannot_parse_routes_neither_and_is_never_rewritten() {
    let root = common::scratch("vault-damaged");
    let fake = FakeGithub::start();
    both(root.path(), &fake);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let other = agh_fake::token("damaged-other");
    fake.add_token(&other, "fixture-other");
    vault_file(root.path(), vault::Slot::Dev, &other);
    let dirs = dirs(root.path());
    let damaged = "[dev\n";
    aterm_uds::private_file::write_private(&dirs.config, vault::RECORD_FILE, damaged.as_bytes())
        .expect("the damaged record");
    fake.add_repo("fixture-other/x", RepoSpec::public());
    fake.clear_seen();

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["repo", "view", "fixture-other/x"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let stderr = captured.stderr_text();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(
        lines[0].starts_with("agh: note vault=dev,release why="),
        "{stderr}"
    );
    assert!(next_of(lines[0]).contains(vault::RECORD_FILE), "{stderr}");
    assert!(
        fake.seen()
            .iter()
            .all(|s| s.as_login.is_none() && !s.had_authorization),
        "no vault token is sent, to learn a login or to read: {:?}",
        fake.seen()
    );
    let (kind, _) = decision(root.path(), &fake, "fixture-other/x", "writer-read");
    assert_eq!(
        kind, "refuse",
        "the rotated token never routes to its new owner"
    );
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_ne!(run(&mut ctx, &["policy", "check"]), 0);
    let report = common::stdout_json(&captured);
    let rules: Vec<&str> = report["faults"]
        .as_array()
        .expect("faults")
        .iter()
        .filter_map(|f| f["rule"].as_str())
        .collect();
    assert_eq!(rules, ["file"], "{report}");
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_ne!(run(&mut ctx, &["auth", "status"]), 0);
    let report = common::stdout_json(&captured);
    let dev = row(report["profiles"].as_array().expect("profiles"), "dev");
    assert_eq!(dev["presence"], Value::from("unusable"), "{dev}");
    assert!(
        dev["remedy"]
            .as_str()
            .is_some_and(|r| r.contains(vault::RECORD_FILE)),
        "{dev}"
    );
    assert_eq!(
        std::fs::read_to_string(dirs.config.join(vault::RECORD_FILE)).expect("the record"),
        damaged,
        "nothing rewrites a record agh cannot parse"
    );

    std::fs::remove_file(dirs.config.join(vault::RECORD_FILE)).expect("removed");
    assert_eq!(
        decision(root.path(), &fake, "fixture-other/x", "writer-read"),
        ("send".to_string(), "dev".to_string()),
        "removed, the record is learned again"
    );
}

/// Each vault token file's unusable state names a step an agent can take, never a loop back to
/// auth status: malformed content is the vault's to rewrite.
#[test]
fn a_malformed_vault_file_names_the_vaults_rewrite() {
    let root = common::scratch("vault-malformed");
    let fake = FakeGithub::start();
    vault_file(root.path(), vault::Slot::Dev, "not a token at all");
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["auth", "status"]), 4);
    let report = common::stdout_json(&captured);
    let dev = row(report["profiles"].as_array().expect("profiles"), "dev");
    assert_eq!(dev["presence"], Value::from("unusable"), "{dev}");
    assert_eq!(dev["remedy"], Value::from(vault::Slot::Dev.sync_step()));
    assert_eq!(
        common::error_json(&captured.stderr_text())["next"],
        Value::from(vault::Slot::Dev.sync_step())
    );
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let stderr = captured.stderr_text();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert_eq!(next_of(lines[0]), vault::Slot::Dev.sync_step(), "{stderr}");
}

/// A machine provisioned before adoption: agh.toml names the vault's fine-grained token files
/// as profiles `dev` and `release` with `drafts_fp`. A rotation to the same login leaves the
/// dev token's drafts unproven, and the refusal's step (removing every such profile) brings the
/// file to `vault = true`, after which each rotation is vouched as adoption learns it.
#[test]
fn an_agh_toml_naming_the_vault_files_comes_to_adoption_by_its_drafts_step() {
    let root = common::scratch("vault-migrate");
    let fake = FakeGithub::start();
    let dev = agh_fake::fine_grained_token("migrate-dev-1");
    let release = agh_fake::fine_grained_token("migrate-release");
    fake.add_token(&dev, DEV_LOGIN);
    fake.add_token(&release, RELEASE_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &dev);
    vault_file(root.path(), vault::Slot::Release, &release);
    fake.add_repo(
        "fixture-dev/app",
        RepoSpec::private().pull(DEV_LOGIN).push(DEV_LOGIN),
    );
    let dirs = dirs(root.path());
    let text = format!(
        "# provisioned before vault adoption\n\
         [profiles.dev]\n\
         token_file = \"~/{dev_file}\"\n\
         tier = \"dev\"\n\
         login = \"{DEV_LOGIN}\"\n\
         drafts_fp = \"{dev_fp}\"\n\n\
         [profiles.release]\n\
         token_file = \"~/{release_file}\"\n\
         tier = \"release\"\n\
         login = \"{RELEASE_LOGIN}\"\n\
         drafts_fp = \"{release_fp}\"\n\n\
         [default]\n\
         read = [\"dev\", \"anonymous\"]\n\n\
         [[owners]]\n\
         name = \"{DEV_LOGIN}\"\n\
         tier = \"dev\"\n\
         read = [\"dev\"]\n\
         write = \"dev\"\n\n\
         [[owners]]\n\
         name = \"alabsystems\"\n\
         tier = \"release\"\n\
         read = [\"dev\", \"anonymous\"]\n\
         write = \"release\"\n\n\
         # read anonymously by choice\n\
         [[owners]]\n\
         name = \"fixture-mirror\"\n\
         tier = \"external\"\n\
         read = [\"anonymous\"]\n",
        dev_file = vault::DEV_TOKEN_FILE,
        release_file = vault::RELEASE_TOKEN_FILE,
        dev_fp = identity::fingerprint(dev.as_bytes()),
        release_fp = identity::fingerprint(release.as_bytes()),
    );
    common::write_config(&dirs, &text);
    let drafts = |root: &Path| {
        let (mut ctx, captured) = common::ctx(root, &fake, common::env());
        let code = run(
            &mut ctx,
            &["release", "list", "fixture-dev/app", "--drafts"],
        );
        (code, captured)
    };
    let (code, captured) = drafts(root.path());
    assert_eq!(code, 0, "{}", captured.stderr_text());

    let rotated = agh_fake::fine_grained_token("migrate-dev-2");
    fake.add_token(&rotated, DEV_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &rotated);
    let (code, captured) = drafts(root.path());
    assert_ne!(code, 0);
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(
        error["sub"],
        Value::from("forbid.drafts_unproven"),
        "{error}"
    );
    let next = error["next"].as_str().expect("next").to_string();
    for step in ["agh auth remove dev", "agh auth remove release"] {
        assert!(next.contains(step), "{next}");
    }

    let remove = |name: &str| {
        let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
        assert_eq!(
            run(&mut ctx, &["auth", "remove", name]),
            0,
            "{}",
            captured.stderr_text()
        );
        common::stdout_json(&captured)["joins_vault"].clone()
    };
    assert_eq!(
        remove("dev"),
        Value::from(false),
        "release still routes alone"
    );
    assert_eq!(remove("release"), Value::from(true));
    let joined = std::fs::read_to_string(dirs.config_file()).expect("agh.toml");
    let parsed = agh::config::parse(&joined).expect("agh.toml parses");
    assert!(parsed.vault, "{joined}");
    assert!(parsed.profiles.is_empty(), "{joined}");
    assert!(
        parsed.default.is_none(),
        "the emptied [default] goes: {joined}"
    );
    let owners: Vec<&str> = parsed.owners.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(
        owners,
        ["fixture-mirror"],
        "the emptied rows go, a row of the person's own stands: {joined}"
    );
    assert!(joined.contains("# read anonymously by choice"), "{joined}");

    let (code, captured) = drafts(root.path());
    assert_eq!(
        code,
        0,
        "adopted, the rotated token's drafts are vouched: {}",
        captured.stderr_text()
    );
    let again = agh_fake::fine_grained_token("migrate-dev-3");
    fake.add_token(&again, DEV_LOGIN);
    vault_file(root.path(), vault::Slot::Dev, &again);
    let (code, captured) = drafts(root.path());
    assert_eq!(
        code,
        0,
        "each later rotation too: {}",
        captured.stderr_text()
    );
    assert_eq!(
        std::fs::read_to_string(dirs.config_file()).expect("agh.toml"),
        joined,
        "adoption never rewrites agh.toml"
    );
}
