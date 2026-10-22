// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The `auth` verbs and the first provisioning, with tokens from `agh_fake::token`:
//! `auth add` (stdin, file, refusals, drafts), `auth remove`, `auth status` and `whoami`.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use agh::cli::{Captured, Ctx};
use agh::config::{self, Dirs};
use agh::identity::{self, MapEnv};
use agh::policy::Router;
use agh_fake::{FakeGithub, RepoSpec};
use aterm_json::Value;

use crate::common;

/// A CLI context whose stderr is a terminal: a person at the keyboard.
fn person(root: &Path, fake: &FakeGithub, env: MapEnv) -> (Ctx, Captured) {
    let (ctx, captured) = common::ctx(root, fake, env);
    (ctx.with_terminals(false, true), captured)
}

/// The same, reading `stdin`.
fn person_with(root: &Path, fake: &FakeGithub, stdin: &str) -> (Ctx, Captured) {
    let (ctx, captured) = person(root, fake, common::env());
    (ctx.with_stdin(stdin.as_bytes().to_vec()), captured)
}

fn dirs(root: &Path) -> Dirs {
    Dirs::under(root)
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o7777
}

/// agh.toml's text.
fn config_text(dirs: &Dirs) -> String {
    std::fs::read_to_string(dirs.config_file()).expect("agh.toml")
}

/// agh.toml parsed (aterm-toml's value model).
fn parsed(dirs: &Dirs) -> aterm_toml::Value {
    aterm_toml::from_str(&config_text(dirs)).expect("agh.toml parses")
}

/// agh.toml validated, as a router.
fn validated(dirs: &Dirs) -> Router {
    let text = config_text(dirs);
    let parsed = config::parse(&text).expect("L1");
    let validated = config::validate(&parsed, &dirs.home, &config::file_id).expect("valid");
    Router::from_validated(validated)
}

/// The `[[owners]]` row named `name`.
fn owner(doc: &aterm_toml::Value, name: &str) -> aterm_toml::Value {
    doc.get("owners")
        .and_then(aterm_toml::Value::as_array)
        .and_then(|owners| {
            owners
                .iter()
                .find(|o| o.get("name").and_then(aterm_toml::Value::as_str) == Some(name))
        })
        .cloned()
        .expect("the owner row")
}

/// A TOML array of strings.
fn strings(value: Option<&aterm_toml::Value>) -> Vec<String> {
    value
        .and_then(aterm_toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(aterm_toml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The second stderr line's `error` object.
/// The files directly in `dir`, by name.
fn names_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn run(ctx: &mut Ctx, args: &[&str]) -> u8 {
    common::run_cli(ctx, args)
}

const ADD_DEV: [&str; 10] = [
    "auth",
    "add",
    "dev",
    "--stdin",
    "--tier",
    "dev",
    "--owner",
    "fixture-dev",
    "--write",
    "--format",
];

// ---------------------------------------------------------------- auth add: first provisioning

#[test]
fn auth_add_without_a_terminal_refuses_and_creates_nothing() {
    let root = common::scratch("auth-noterm");
    let fake = FakeGithub::start();
    let token = agh_fake::token("noterm");
    fake.add_token(&token, "fixture-dev-bot");
    let (ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let mut ctx = ctx.with_stdin(format!("{token}\n").into_bytes());
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--stdin", "--tier", "dev"],
    );
    assert_eq!(exit, 1);
    assert_eq!(
        common::error_json(&captured.stderr_text())["kind"],
        Value::from("refused")
    );
    assert!(!dirs(root.path()).config.exists());
    assert!(fake.seen().is_empty());
}

// A token typed at a terminal is echoed as it is typed: `--stdin` takes a pipe or a file, and a
// terminal stdin is usage before anything is read, made or sent. `--file` is not stdin.
#[test]
fn auth_add_stdin_from_a_terminal_is_usage_and_reads_nothing() {
    let root = common::scratch("auth-ttyin");
    let fake = FakeGithub::start();
    let token = agh_fake::token("ttyin");
    fake.add_token(&token, "fixture-dev-bot");
    let (ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let mut ctx = ctx.with_stdin_terminal(true);
    let exit = run(
        &mut ctx,
        &[
            "auth",
            "add",
            "dev",
            "--stdin",
            "--tier",
            "dev",
            "--owner",
            "fixture-dev",
            "--write",
            "--drafts",
        ],
    );
    assert_eq!(exit, 2);
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(error["kind"], Value::from("usage"));
    let next = error["next"].as_str().expect("a next");
    // The next is the whole request, stdin filled: following it provisions this profile.
    assert!(next.contains("dev --stdin --tier dev"), "{next}");
    assert!(next.contains("--owner fixture-dev"), "{next}");
    assert!(next.contains("--write"), "{next}");
    assert!(next.contains("--drafts"), "{next}");
    assert!(next.ends_with("< FILE"), "{next}");
    assert!(!dirs(root.path()).config.exists());
    assert!(fake.seen().is_empty());
    let mut unread = String::new();
    std::io::Read::read_to_string(&mut ctx.stdin, &mut unread).expect("stdin");
    assert_eq!(unread, format!("{token}\n"), "stdin was read");
    // With --file, a terminal stdin is no refusal: the token is not on stdin.
    let dirs = dirs(root.path());
    std::fs::create_dir_all(&dirs.home).expect("home");
    let file = common::write_token(&dirs.home.join("tok"), "dev.token", &token);
    let (ctx, captured) = person(root.path(), &fake, common::env());
    let mut ctx = ctx.with_stdin_terminal(true);
    let path = file.display().to_string();
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--file", &path, "--tier", "dev"],
    );
    assert_eq!(exit, 0, "{}", captured.stderr_text());
}

#[test]
fn auth_add_from_stdin_writes_private_files_that_validate() {
    let root = common::scratch("auth-stdin");
    let fake = FakeGithub::start();
    let token = agh_fake::token("stdin");
    fake.add_token(&token, "fixture-dev-bot");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let mut args = ADD_DEV.to_vec();
    args.push("json");
    assert_eq!(run(&mut ctx, &args), 0, "{}", captured.stderr_text());
    let dirs = dirs(root.path());
    assert_eq!(mode(&dirs.config), 0o700);
    assert_eq!(mode(&dirs.config_file()), 0o600);
    assert_eq!(mode(&dirs.tokens()), 0o700);
    let token_file = dirs.tokens().join("dev.token");
    assert_eq!(mode(&token_file), 0o600);
    assert_eq!(names_in(&dirs.tokens()), ["dev.token"]);
    assert_eq!(
        std::fs::read_to_string(&token_file).expect("the token file"),
        format!("{token}\n")
    );
    let router = validated(&dirs);
    let profile = router.profile("dev").expect("the dev profile");
    assert_eq!(profile.login.as_deref(), Some("fixture-dev-bot"));
    let doc = parsed(&dirs);
    let row = owner(&doc, "fixture-dev");
    assert_eq!(
        strings(row.get("read")).first().map(String::as_str),
        Some("dev")
    );
    assert_eq!(
        row.get("write").and_then(aterm_toml::Value::as_str),
        Some("dev")
    );
    let answer = common::stdout_json(&captured);
    assert_eq!(answer["profile"], Value::from("dev"));
    assert_eq!(answer["presence"], Value::from("present"));
    assert_eq!(
        answer["fingerprint"],
        Value::from(identity::fingerprint(token.as_bytes()))
    );
    assert_eq!(answer["login"], Value::from("fixture-dev-bot"));
    assert_eq!(answer["kind"], Value::from("classic"));
    let user = fake
        .seen()
        .into_iter()
        .find(|s| s.path_and_query == "/user")
        .expect("GET /user");
    assert_eq!(user.as_login.as_deref(), Some("fixture-dev-bot"));
}

// ---------------------------------------------------------------- auth add: editing a file

/// A valid agh.toml with a comment above an owner table and one at the end of its read line.
const COMMENTED: &str = "# kept above the owner table\n\
[[owners]]\n\
name = \"fixture-dev\"\n\
tier = \"dev\"\n\
read = [\"anonymous\"] # kept at the end of the read line\n";

#[test]
fn auth_add_keeps_a_files_comments() {
    let root = common::scratch("auth-comments");
    let fake = FakeGithub::start();
    let token = agh_fake::token("comments");
    fake.add_token(&token, "fixture-dev-bot");
    let dirs = dirs(root.path());
    common::write_config(&dirs, COMMENTED);
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let mut args = ADD_DEV.to_vec();
    args.push("json");
    assert_eq!(run(&mut ctx, &args), 0, "{}", captured.stderr_text());
    let text = config_text(&dirs);
    assert!(text.contains("# kept above the owner table"), "{text}");
    assert!(
        text.contains("# kept at the end of the read line"),
        "{text}"
    );
    let row = owner(&parsed(&dirs), "fixture-dev");
    assert_eq!(strings(row.get("read")), ["dev", "anonymous"], "{text}");
    validated(&dirs);
}

#[test]
fn auth_add_again_replaces_the_token_and_login_and_a_tier_change_is_refused() {
    let root = common::scratch("auth-again");
    let fake = FakeGithub::start();
    let first = agh_fake::token("first");
    let second = agh_fake::token("second");
    fake.add_token(&first, "fixture-dev-bot");
    fake.add_token(&second, "fixture-reader");
    let dirs = dirs(root.path());
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{first}\n"));
    let mut args = ADD_DEV.to_vec();
    args.push("json");
    assert_eq!(run(&mut ctx, &args), 0, "{}", captured.stderr_text());

    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{second}\n"));
    assert_eq!(run(&mut ctx, &args), 0, "{}", captured.stderr_text());
    let token_file = dirs.tokens().join("dev.token");
    assert_eq!(
        std::fs::read_to_string(&token_file).expect("the token file"),
        format!("{second}\n")
    );
    assert_eq!(names_in(&dirs.tokens()), ["dev.token"]);
    let router = validated(&dirs);
    assert_eq!(
        router.profile("dev").and_then(|p| p.login.as_deref()),
        Some("fixture-reader")
    );
    let row = owner(&parsed(&dirs), "fixture-dev");
    assert_eq!(strings(row.get("read")), ["dev"]);

    // Park a real request after its credential is prepared, before its network
    // send. All environment reads are injected and return no value.
    struct GateEnv(
        std::sync::Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    );
    impl identity::EnvSource for GateEnv {
        fn var_os(&self, _name: &str) -> Option<std::ffi::OsString> {
            let gate = self.0.lock().expect("the injected environment").take();
            if let Some((entered, release)) = gate {
                entered
                    .send(())
                    .expect("the request reached its environment");
                release
                    .recv_timeout(common::HANG)
                    .expect("release the parked request");
            }
            None
        }
    }
    struct Release(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(release) = self.0.take() {
                let _ = release.send(());
            }
        }
    }
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let env = std::sync::Arc::new(GateEnv(std::sync::Mutex::new(Some((entered, released)))));
    std::thread::scope(|scope| {
        let pending = scope.spawn(|| {
            let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{first}\n"));
            ctx.env = env.clone();
            ctx.transport = Some(std::sync::Arc::new(agh::wire::http::HttpTransport::new(
                env.clone(),
            )));
            (run(&mut ctx, &args), captured)
        });
        // Dropped before scope joins on an assertion failure too.
        let release = Release(Some(release));
        ready
            .recv_timeout(common::HANG)
            .expect("the first edit reached its request");
        let staged = dirs.tokens().join("dev.token.agh-new");
        assert_eq!(
            std::fs::read_to_string(&staged).expect("the first stage"),
            format!("{first}\n")
        );
        let before = config_text(&dirs);
        let sent = fake.seen().len();
        for edit in ["same", "other", "remove"] {
            let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{second}\n"));
            let exit = match edit {
                "same" => run(&mut ctx, &args),
                "other" => run(
                    &mut ctx,
                    &["auth", "add", "other", "--stdin", "--tier", "dev"],
                ),
                "remove" => run(&mut ctx, &["auth", "remove", "dev"]),
                _ => unreachable!(),
            };
            assert_eq!(exit, 1, "{}", captured.stderr_text());
            assert_eq!(config_text(&dirs), before);
            assert_eq!(
                std::fs::read_to_string(&staged).expect("the held stage"),
                format!("{first}\n")
            );
            assert_eq!(
                std::fs::read_to_string(&token_file).expect("the earlier token"),
                format!("{second}\n")
            );
            assert_eq!(fake.seen().len(), sent, "a competing edit sent no request");
        }
        drop(release);
        let (exit, captured) = pending.join().expect("the first editor completed");
        assert_eq!(exit, 0, "{}", captured.stderr_text());
    });
    assert_eq!(
        std::fs::read_to_string(&token_file).expect("the first token"),
        format!("{first}\n")
    );
    assert_eq!(
        validated(&dirs)
            .profile("dev")
            .and_then(|p| p.login.as_deref()),
        Some("fixture-dev-bot")
    );
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{second}\n"));
    assert_eq!(run(&mut ctx, &args), 0, "{}", captured.stderr_text());
    assert_eq!(
        std::fs::read_to_string(&token_file).expect("the replacement token"),
        format!("{second}\n")
    );
    assert_eq!(
        validated(&dirs)
            .profile("dev")
            .and_then(|p| p.login.as_deref()),
        Some("fixture-reader")
    );
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{second}\n"));
    assert_eq!(
        run(
            &mut ctx,
            &["auth", "add", "other", "--stdin", "--tier", "dev"]
        ),
        0,
        "{}",
        captured.stderr_text()
    );
    assert!(validated(&dirs).profile("other").is_some());
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "other"]),
        0,
        "{}",
        captured.stderr_text()
    );
    assert!(validated(&dirs).profile("dev").is_some());
    assert!(validated(&dirs).profile("other").is_none());

    let before = config_text(&dirs);
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{second}\n"));
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--stdin", "--tier", "external"],
    );
    assert_eq!(exit, 1, "{}", captured.stderr_text());
    assert_eq!(config_text(&dirs), before);
}

#[test]
fn auth_add_of_a_token_github_rejects_changes_nothing() {
    let root = common::scratch("auth-401");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::write_config(&dirs, COMMENTED);
    let before = config_text(&dirs);
    let rejected = agh_fake::token("rejected");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{rejected}\n"));
    let mut args = ADD_DEV.to_vec();
    args.push("json");
    assert_eq!(run(&mut ctx, &args), 4, "{}", captured.stderr_text());
    assert_eq!(config_text(&dirs), before);
    assert!(
        names_in(&dirs.tokens()).is_empty(),
        "{:?}",
        names_in(&dirs.tokens())
    );
    assert!(captured.stdout().is_empty());
}

// ---------------------------------------------------------------- auth add: refusals

#[test]
fn auth_add_of_a_loose_file_names_the_chmods() {
    let root = common::scratch("auth-loose");
    let fake = FakeGithub::start();
    let token = agh_fake::token("loose");
    fake.add_token(&token, "fixture-dev-bot");
    let dirs = dirs(root.path());
    std::fs::create_dir_all(&dirs.home).expect("home");
    let file = common::write_token(&dirs.home.join("tok"), "dev.token", &token);
    common::chmod(&file, 0o644);
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    let path = file.display().to_string();
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--file", &path, "--tier", "dev"],
    );
    assert_eq!(exit, 1, "{}", captured.stderr_text());
    let error = common::error_json(&captured.stderr_text());
    let next = error["next"].as_str().unwrap_or_default();
    assert!(next.contains("chmod 600"), "{next}");
    assert!(next.contains("~/tok/dev.token"), "{next}");
    assert!(next.contains("644"), "{next}");
    assert!(!dirs.config_file().exists());
    assert!(fake.seen().is_empty());
}

#[test]
fn auth_add_from_a_private_file_names_it_in_place() {
    let root = common::scratch("auth-file");
    let fake = FakeGithub::start();
    let token = agh_fake::token("file");
    fake.add_token(&token, "fixture-dev-bot");
    let dirs = dirs(root.path());
    std::fs::create_dir_all(&dirs.home).expect("home");
    let file = common::write_token(&dirs.home.join("tok"), "dev.token", &token);
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    let path = file.display().to_string();
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--file", &path, "--tier", "dev"],
    );
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let doc = parsed(&dirs);
    let row = doc
        .get("profiles")
        .and_then(|p| p.get("dev"))
        .expect("the dev row");
    assert_eq!(
        row.get("token_file").and_then(aterm_toml::Value::as_str),
        Some("~/tok/dev.token")
    );
    assert!(names_in(&dirs.tokens()).is_empty());
    assert_eq!(
        common::stdout_json(&captured)["source"],
        Value::from("file:~/tok/dev.token")
    );
    validated(&dirs);
}

#[test]
fn auth_add_refuses_a_missing_file_a_long_stdin_and_a_loose_config_directory() {
    let root = common::scratch("auth-refuse");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    let missing: PathBuf = root.path().join("no-such.token");
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    let path = missing.display().to_string();
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--file", &path, "--tier", "dev"],
    );
    assert_eq!(exit, 2, "{}", captured.stderr_text());

    let long = "x".repeat(300);
    let (mut ctx, captured) = person_with(root.path(), &fake, &long);
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--stdin", "--tier", "dev"],
    );
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert!(!dirs.config.exists());

    let parent = dirs.config.parent().expect("a parent");
    std::fs::create_dir_all(parent).expect("the parent");
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&dirs.config)
        .expect("a loose config directory");
    common::chmod(&dirs.config, 0o755);
    let token = agh_fake::token("loose-dir");
    fake.add_token(&token, "fixture-dev-bot");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--stdin", "--tier", "dev"],
    );
    assert_eq!(exit, 1, "{}", captured.stderr_text());
    assert_eq!(mode(&dirs.config), 0o755);
    assert!(!dirs.tokens().exists());

    // A loose tokens/ under a private config directory: refused, the directory named once,
    // by the `~/…`-or-absolute form agh chose, never again by aterm-uds's absolute form.
    common::chmod(&dirs.config, 0o700);
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(dirs.tokens())
        .expect("a loose tokens directory");
    common::chmod(&dirs.tokens(), 0o755);
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--stdin", "--tier", "dev"],
    );
    assert_eq!(exit, 1, "{}", captured.stderr_text());
    assert_eq!(mode(&dirs.tokens()), 0o755);
    let error = common::error_json(&captured.stderr_text());
    let why = error["why"].as_str().unwrap_or_default();
    let tokens = dirs.tokens().display().to_string();
    assert_eq!(why.matches(tokens.as_str()).count(), 1, "{why}");
    // The data the refusal carries: the chmod that makes it private, and its inverse with
    // the 0755 this test set.
    assert!(
        error["next"]
            .as_str()
            .is_some_and(|next| next.starts_with("chmod 700 ") && next.contains("chmod 755 ")),
        "{error}"
    );
}

#[test]
fn auth_add_checks_its_arguments() {
    let root = common::scratch("auth-args");
    let fake = FakeGithub::start();
    for args in [
        &["auth", "add", "anonymous", "--stdin", "--tier", "dev"][..],
        &["auth", "add", "Dev", "--stdin", "--tier", "dev"],
        &["auth", "add", "dev", "--stdin", "--tier", "staging"],
        &["auth", "add", "dev", "--stdin"],
        &["auth", "add", "dev", "--tier", "dev"],
        &[
            "auth", "add", "dev", "--stdin", "--file", "/x", "--tier", "dev",
        ],
        &["auth", "add", "dev", "--stdin", "--tier", "dev", "--write"],
        &[
            "auth", "add", "dev", "--stdin", "--tier", "dev", "--owner", "-bad",
        ],
    ] {
        let (mut ctx, captured) = person(root.path(), &fake, common::env());
        assert_eq!(
            run(&mut ctx, args),
            2,
            "{args:?}: {}",
            captured.stderr_text()
        );
    }
    assert!(!dirs(root.path()).config.exists());

    // Library callers need the same name admission before stdin or filesystem work.
    let dirs = dirs(root.path());
    let gh = common::github(Router::empty(), &fake);
    let token = agh_fake::token("api-args");
    fake.add_token(&token, "fixture-dev-bot");
    let mut req = identity::provision::AddRequest {
        name: "../escape".to_string(),
        from: identity::provision::AddFrom::Stdin,
        tier: agh::policy::Tier::Dev,
        owners: Vec::new(),
        write: false,
        drafts: false,
    };
    let mut stdin = std::io::Cursor::new(format!("{token}\n").into_bytes());
    let error = identity::provision::add(&gh, &dirs, &req, &mut stdin)
        .expect_err("the public API refuses traversal before creating its directory");
    assert_eq!(error.kind, agh::error::Kind::Usage, "{error:?}");
    assert_eq!(stdin.position(), 0);
    assert!(!dirs.config.exists());

    // Existing intermediate directories must not make slash or absolute names usable.
    std::fs::create_dir_all(dirs.config.parent().expect("the config parent"))
        .expect("the config parent");
    aterm_uds::private_file::create_private_dir(&dirs.config).expect("the private config");
    aterm_uds::private_file::create_private_dir(&dirs.tokens()).expect("the private tokens");
    let sub = dirs.tokens().join("sub");
    aterm_uds::private_file::create_private_dir(&sub).expect("a token subdirectory");
    let sentinel = dirs.config.join("keep");
    std::fs::write(&sentinel, b"kept outside tokens").expect("the owned sentinel");
    let absolute = root
        .path()
        .join("absolute")
        .to_str()
        .expect("UTF-8")
        .to_string();
    for name in ["../escape", "sub/token", absolute.as_str()] {
        req.name = name.to_string();
        let mut stdin = std::io::Cursor::new(format!("{token}\n").into_bytes());
        let error = identity::provision::add(&gh, &dirs, &req, &mut stdin)
            .expect_err("the public API refuses a name outside the profile grammar");
        assert_eq!(error.kind, agh::error::Kind::Usage, "{name}: {error:?}");
        assert_eq!(stdin.position(), 0);
        assert_eq!(names_in(&dirs.config), ["keep", "tokens"]);
        assert_eq!(names_in(&dirs.tokens()), ["sub"]);
        assert!(names_in(&sub).is_empty());
        assert!(!root.path().join("absolute.token.agh-new").exists());
        assert_eq!(
            std::fs::read(&sentinel).expect("the sentinel"),
            b"kept outside tokens"
        );
        assert!(fake.seen().is_empty(), "{:?}", fake.seen());
    }
    req.name = "dev".to_string();
    let mut stdin = std::io::Cursor::new(format!("{token}\n").into_bytes());
    let added = identity::provision::add(&gh, &dirs, &req, &mut stdin)
        .expect("the same public API admits an ordinary profile");
    assert_eq!(added.login, "fixture-dev-bot");
    assert_eq!(
        std::fs::read_to_string(added.token_file).expect("the token"),
        format!("{token}\n")
    );
    assert_eq!(fake.seen().len(), 1);
}

// ---------------------------------------------------------------- auth remove

#[test]
fn auth_remove_drops_every_reference_and_only_its_own_token_file() {
    let root = common::scratch("auth-remove");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::canonical_tokens(root.path(), &fake, true, true);
    common::write_config(&dirs, &common::canonical_config(root.path()));
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "dev"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let answer = common::stdout_json(&captured);
    assert_eq!(answer["profile"], Value::from("dev"));
    assert_eq!(answer["token_file_deleted"], Value::from(false));
    assert_eq!(
        answer["owners_edited"],
        aterm_json::json!(["fixture-dev", "alabsystems"])
    );
    assert!(root.path().join("tokens").join("dev.token").exists());
    let doc = parsed(&dirs);
    assert!(doc.get("profiles").and_then(|p| p.get("dev")).is_none());
    for name in ["fixture-dev", "alabsystems"] {
        let row = owner(&doc, name);
        assert!(
            !strings(row.get("read")).contains(&"dev".to_string()),
            "{name}"
        );
        assert_ne!(
            row.get("write").and_then(aterm_toml::Value::as_str),
            Some("dev")
        );
    }
    let default = doc.get("default").expect("[default]");
    assert_eq!(strings(default.get("read")), ["anonymous"]);
    validated(&dirs);

    let token = agh_fake::token("own");
    fake.add_token(&token, "fixture-ext-bot");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let exit = run(
        &mut ctx,
        &[
            "auth",
            "add",
            "ext",
            "--stdin",
            "--tier",
            "external",
            "--owner",
            "fixture-ext",
        ],
    );
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert!(dirs.tokens().join("ext.token").exists());
    // An interrupted stage may be the only surviving copy of a token. Removing
    // the profile does not claim ownership of that occupied staging name.
    let stale = common::write_token(&dirs.tokens(), "ext.token.agh-new", &token);
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "ext"]),
        0,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(
        common::stdout_json(&captured)["token_file_deleted"],
        Value::from(true)
    );
    assert!(!dirs.tokens().join("ext.token").exists());
    assert_eq!(
        std::fs::read_to_string(&stale).expect("the interrupted stage"),
        format!("{token}\n")
    );

    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "nobody"]),
        2,
        "{}",
        captured.stderr_text()
    );

    let (ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let mut ctx = ctx;
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "release"]),
        1,
        "{}",
        captured.stderr_text()
    );
}

#[test]
fn a_staged_token_is_never_named_and_never_deleted_while_a_profile_names_it() {
    fn make_fifo(path: &Path) -> Result<std::process::Output, aterm_uds::deadline::RunError> {
        let mut cmd = aterm_uds::spawnfd::command("mkfifo");
        cmd.arg(path);
        aterm_uds::deadline::outputs_until(
            &mut [&mut cmd],
            std::time::Instant::now() + common::HANG,
            common::HANG,
        )
        .pop()
        .expect("one child result")
    }
    let root = common::scratch("auth-staged");
    let fake = FakeGithub::start();
    let token = agh_fake::token("staged");
    fake.add_token(&token, "fixture-dev-bot");
    let dirs = dirs(root.path());
    common::write_config(&dirs, "");
    std::fs::remove_file(dirs.config_file()).expect("no agh.toml yet");
    // The token an interrupted `auth add dev --stdin` left: naming it is usage, and it stays.
    let staged = common::write_token(&dirs.tokens(), "dev.token.agh-new", &token);
    let path = staged.display().to_string();
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    let exit = run(
        &mut ctx,
        &["auth", "add", "ci", "--file", &path, "--tier", "dev"],
    );
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert!(staged.exists(), "the person's token is kept");
    assert!(!dirs.config_file().exists());
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());

    // Even when no profile names it, another edit's or an interrupted stage is
    // not overwritten. A link is not followed, and a FIFO is not opened.
    let own = dirs.tokens().join("dev.token");
    let neighbor = common::write_token(root.path(), "kept-token", &token);
    let other = agh_fake::token("other");
    fake.add_token(&other, "fixture-dev-bot");
    for kind in ["file", "link", "fifo"] {
        let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{other}\n"));
        assert_eq!(
            run(
                &mut ctx,
                &["auth", "add", "dev", "--stdin", "--tier", "dev"]
            ),
            1,
            "{kind}: {}",
            captured.stderr_text()
        );
        assert!(!dirs.config_file().exists());
        assert!(fake.seen().is_empty());
        match kind {
            "file" => {
                assert_eq!(
                    std::fs::read_to_string(&staged).expect("the occupied stage"),
                    format!("{token}\n")
                );
                std::fs::remove_file(&staged).expect("remove the fixture's file");
                std::os::unix::fs::symlink(&neighbor, &staged).expect("a fixture link");
            }
            "link" => {
                assert!(
                    std::fs::symlink_metadata(&staged)
                        .expect("the link")
                        .file_type()
                        .is_symlink()
                );
                std::fs::remove_file(&staged).expect("remove the fixture's link");
                let made = match make_fifo(&staged) {
                    Ok(made) => made,
                    Err(error) => {
                        let kept = root.keep();
                        panic!(
                            "mkfifo did not reach confirmed EOF: {error}; retained {}",
                            kept.display()
                        );
                    }
                };
                assert!(made.status.success(), "mkfifo: {made:?}");
            }
            "fifo" => {
                use std::os::unix::fs::FileTypeExt as _;
                assert!(
                    std::fs::symlink_metadata(&staged)
                        .expect("the FIFO")
                        .file_type()
                        .is_fifo()
                );
                std::fs::remove_file(&staged).expect("remove the fixture's FIFO");
            }
            _ => unreachable!(),
        }
    }
    assert_eq!(
        std::fs::read_to_string(&neighbor).expect("the link's target"),
        format!("{token}\n")
    );
    let lock = dirs.config.join(".auth-edit.lock");
    // Existing lock entries are admitted, never truncated or followed.
    std::fs::write(&lock, b"occupied lock").expect("the fixture's lock file");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{other}\n"));
    assert_eq!(
        run(
            &mut ctx,
            &["auth", "add", "dev", "--stdin", "--tier", "dev"]
        ),
        1,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(
        std::fs::read(&lock).expect("the occupied lock"),
        b"occupied lock"
    );
    std::fs::remove_file(&lock).expect("remove the fixture's occupied lock");
    std::os::unix::fs::symlink(&neighbor, &lock).expect("a fixture lock link");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{other}\n"));
    assert_eq!(
        run(
            &mut ctx,
            &["auth", "add", "dev", "--stdin", "--tier", "dev"]
        ),
        1,
        "{}",
        captured.stderr_text()
    );
    assert!(
        std::fs::symlink_metadata(&lock)
            .expect("the lock link")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(&neighbor).expect("the lock link's target"),
        format!("{token}\n")
    );
    std::fs::remove_file(&lock).expect("remove the fixture's lock link");
    let made = match make_fifo(&lock) {
        Ok(made) => made,
        Err(error) => {
            let kept = root.keep();
            panic!(
                "mkfifo did not reach confirmed EOF: {error}; retained {}",
                kept.display()
            );
        }
    };
    assert!(made.status.success(), "mkfifo: {made:?}");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{other}\n"));
    assert_eq!(
        run(
            &mut ctx,
            &["auth", "add", "dev", "--stdin", "--tier", "dev"]
        ),
        1,
        "{}",
        captured.stderr_text()
    );
    {
        use std::os::unix::fs::FileTypeExt as _;
        assert!(
            std::fs::symlink_metadata(&lock)
                .expect("the lock FIFO")
                .file_type()
                .is_fifo()
        );
    }
    std::fs::remove_file(&lock).expect("remove the fixture's lock FIFO");
    assert!(!dirs.config_file().exists());
    assert!(fake.seen().is_empty());
    // Only after the fixture removes its own occupied entries can a normal
    // provision complete; its created stage is cleaned on success.
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{other}\n"));
    assert_eq!(
        run(
            &mut ctx,
            &["auth", "add", "dev", "--stdin", "--tier", "dev"]
        ),
        0,
        "{}",
        captured.stderr_text()
    );
    assert!(!staged.exists());
    assert_eq!(
        std::fs::read_to_string(&own).expect("the new token"),
        format!("{other}\n")
    );
    std::fs::remove_file(&own).expect("remove the fixture's new token");
    common::write_token(&dirs.tokens(), "dev.token.agh-new", &token);
    fake.clear_seen();

    // A hand-written agh.toml naming it as ci's token: removing dev keeps ci's token, and a
    // stdin add of dev refuses before it stages over it.
    common::write_config(
        &dirs,
        &format!(
            "[profiles.ci]\ntoken_file = \"{path}\"\ntier = \"dev\"\n\n\
             [profiles.dev]\ntoken_file = \"{}\"\ntier = \"dev\"\n",
            own.display()
        ),
    );
    validated(&dirs);
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "dev"]),
        0,
        "{}",
        captured.stderr_text()
    );
    assert!(staged.exists(), "ci's token is kept");
    let other = agh_fake::token("other");
    fake.add_token(&other, "fixture-dev-bot");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{other}\n"));
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--stdin", "--tier", "dev"],
    );
    assert_eq!(exit, 1, "{}", captured.stderr_text());
    assert_eq!(
        std::fs::read_to_string(&staged).expect("ci's token"),
        format!("{token}\n")
    );
    assert!(!own.exists());
}

// ---------------------------------------------------------------- auth add --drafts

#[test]
fn auth_add_drafts_vouches_for_the_token_and_a_re_add_without_it_drops_the_line() {
    let root = common::scratch("auth-drafts");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    let exit = run(
        &mut ctx,
        &[
            "auth",
            "add",
            "dev",
            "--stdin",
            "--tier",
            "dev",
            "--owner",
            "fixture-dev",
            "--drafts",
        ],
    );
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert!(!dirs.config.exists());

    let token = agh_fake::fine_grained_token("dev");
    fake.add_token(&token, "fixture-dev-bot");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let mut args = ADD_DEV.to_vec();
    args.insert(9, "--drafts");
    args.push("json");
    assert_eq!(run(&mut ctx, &args), 0, "{}", captured.stderr_text());
    let doc = parsed(&dirs);
    let row = doc
        .get("profiles")
        .and_then(|p| p.get("dev"))
        .cloned()
        .expect("the dev row");
    assert_eq!(
        row.get("drafts_fp").and_then(aterm_toml::Value::as_str),
        Some(identity::fingerprint(token.as_bytes()).as_str())
    );
    let router = validated(&dirs);
    let gh = common::github(router, &fake);
    assert!(gh.drafts_vouched("dev"));

    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let mut args = ADD_DEV.to_vec();
    args.push("json");
    assert_eq!(run(&mut ctx, &args), 0, "{}", captured.stderr_text());
    let doc = parsed(&dirs);
    let row = doc
        .get("profiles")
        .and_then(|p| p.get("dev"))
        .expect("the dev row");
    assert!(row.get("drafts_fp").is_none(), "{}", config_text(&dirs));
    assert!(!common::github(validated(&dirs), &fake).drafts_vouched("dev"));
}

// ---------------------------------------------------------------- auth status

#[test]
fn auth_status_reports_a_valid_file() {
    let root = common::scratch("status-ok");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::canonical_tokens(root.path(), &fake, true, false);
    common::write_config(&dirs, &common::canonical_config(root.path()));
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "status"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let report = common::stdout_json(&captured);
    let mut keys: Vec<&str> = report
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "ambient", "config", "faults", "ok", "platform", "present", "profiles"
        ]
    );
    assert_eq!(report["ok"], Value::from(true));
    assert_eq!(report["present"], Value::from(true));
    assert_eq!(report["platform"], Value::from("unix"));
    let profiles = report["profiles"].as_array().expect("profiles");
    let names: Vec<&str> = profiles
        .iter()
        .filter_map(|p| p["profile"].as_str())
        .collect();
    assert_eq!(names, ["dev", "release"]);
    assert_eq!(profiles[0]["presence"], Value::from("present"));
    assert_eq!(profiles[1]["presence"], Value::from("absent"));
    assert!(fake.seen().is_empty(), "auth status sends nothing");
}

#[test]
fn auth_status_of_an_invalid_file_prints_the_report_then_the_error() {
    let root = common::scratch("status-bad");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::write_config(
        &dirs,
        "[[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"ghost\"]\n",
    );
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["auth", "status"]), 1);
    let report = common::stdout_json(&captured);
    assert_eq!(report["ok"], Value::from(false));
    assert_eq!(report["profiles"], aterm_json::json!([]));
    let faults = report["faults"].as_array().expect("faults");
    assert_eq!(faults[0]["rule"], Value::from("L1"));
    let stderr = captured.stderr_text();
    assert_eq!(stderr.lines().count(), 2, "{stderr}");
    let error = common::error_json(&stderr);
    assert_eq!(
        error["sub"],
        Value::from("policy.config_invalid"),
        "{error}"
    );
}

#[test]
fn auth_status_fails_on_an_unusable_referenced_profile() {
    let root = common::scratch("status-loose");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::canonical_tokens(root.path(), &fake, true, false);
    common::chmod(&root.path().join("tokens").join("dev.token"), 0o644);
    common::write_config(&dirs, &common::canonical_config(root.path()));
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "status"]),
        4,
        "{}",
        captured.stderr_text()
    );
    let report = common::stdout_json(&captured);
    let dev = &report["profiles"][0];
    assert_eq!(dev["profile"], Value::from("dev"));
    assert_eq!(dev["presence"], Value::from("unusable"));
    assert!(
        dev["remedy"]
            .as_str()
            .is_some_and(|r| r.contains("chmod 600")),
        "{dev}"
    );
    assert_eq!(
        common::error_json(&captured.stderr_text())["kind"],
        Value::from("unauthenticated")
    );
}

// ---------------------------------------------------------------- whoami

/// The canonical config with a third profile, `ext`, whose token file is `{root}/loose`.
fn three_profiles(root: &Path) -> String {
    let loose = root.join("loose").join("ext.token");
    format!(
        "{}\n[profiles.ext]\ntoken_file = \"{}\"\ntier = \"external\"\nlogin = \"fixture-ext-bot\"\n",
        common::canonical_config(root),
        loose.display()
    )
}

fn identity_rows(captured: &Captured) -> Vec<Value> {
    common::stdout_json(captured)
        .as_array()
        .expect("rows")
        .clone()
}

#[test]
fn whoami_lists_present_absent_unusable_and_ambient_rows() {
    let root = common::scratch("whoami-rows");
    let fake = FakeGithub::start();
    fake.add_repo("fixture-dev/app", RepoSpec::public());
    let dirs = dirs(root.path());
    common::canonical_tokens(root.path(), &fake, true, false);
    let ext = common::write_token(
        &root.path().join("loose"),
        "ext.token",
        &agh_fake::token("ext"),
    );
    common::chmod(&ext, 0o644);
    common::write_config(&dirs, &three_profiles(root.path()));
    let ambient = agh_fake::token("ambient");
    fake.add_token(&ambient, "fixture-reader");
    let env = MapEnv::new().with("GH_TOKEN", &ambient);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, env);
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let rows = identity_rows(&captured);
    let presence: Vec<(Value, Value)> = rows
        .iter()
        .map(|r| (r["profile"].clone(), r["presence"].clone()))
        .collect();
    assert_eq!(
        presence,
        [
            (Value::from("dev"), Value::from("present")),
            (Value::from("ext"), Value::from("unusable")),
            (Value::from("release"), Value::from("absent")),
            (Value::Null, Value::from("present")),
        ]
    );
    assert_eq!(
        rows[0]["fingerprint"],
        Value::from(identity::fingerprint(agh_fake::token("dev").as_bytes()))
    );
    assert_eq!(rows[0]["login"], Value::from("fixture-dev-bot"));
    assert!(rows[1]["remedy"].as_str().is_some(), "{}", rows[1]);
    assert!(
        rows[1]["why"]
            .as_str()
            .is_some_and(|why| why.starts_with("loose: ")),
        "{}",
        rows[1]
    );
    assert!(rows[1]["fingerprint"].is_null());
    let gh_token = &rows[3];
    assert_eq!(gh_token["source"], Value::from("env:GH_TOKEN"));
    assert!(
        gh_token["remedy"]
            .as_str()
            .is_some_and(|r| r.contains("GH_TOKEN")),
        "{gh_token}"
    );
    assert_eq!(
        gh_token["fingerprint"],
        Value::from(identity::fingerprint(ambient.as_bytes()))
    );

    let gh = ctx.github().expect("the client");
    let user = gh.users().get("fixture-dev").expect("a routed read");
    assert_eq!(user.login, "fixture-dev");
    let seen = fake.seen();
    assert!(!seen.is_empty());
    assert!(
        seen.iter()
            .all(|s| s.as_login.as_deref() != Some("fixture-reader")),
        "the ambient token is never sent"
    );
}

#[test]
fn whoami_for_explains_the_routing() {
    let root = common::scratch("whoami-for");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::canonical_tokens(root.path(), &fake, true, false);
    common::write_config(&dirs, &common::canonical_config(root.path()));
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let exit = run(
        &mut ctx,
        &["whoami", "--for", "alabsystems/aterm", "--op", "write"],
    );
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let route = common::stdout_json(&captured);
    assert_eq!(
        route["decision"]["refuse"]["code"],
        Value::from("writer_absent"),
        "{route}"
    );
    let steps: Vec<&str> = route["chain"]
        .as_array()
        .expect("the chain")
        .iter()
        .filter_map(|s| s["step"].as_str())
        .collect();
    assert_eq!(steps, ["dev", "anonymous"], "{route}");

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami", "--op", "write"]), 2);
    assert_eq!(
        common::error_json(&captured.stderr_text())["kind"],
        Value::from("usage")
    );
    let (mut ctx, _captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(
            &mut ctx,
            &["whoami", "--for", "alabsystems/aterm", "--verify"]
        ),
        2
    );
    let (mut ctx, _captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(
            &mut ctx,
            &["whoami", "--for", "alabsystems/aterm", "--op", "push"]
        ),
        2
    );
}

#[test]
fn whoami_for_a_literal_through_argv() {
    let root = common::scratch("whoami-literal");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::canonical_tokens(root.path(), &fake, true, true);
    common::write_config(&dirs, &common::canonical_config(root.path()));
    let decide = |target: &str| {
        let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
        let exit = run(&mut ctx, &["whoami", "--for", target, "--op", "write"]);
        assert_eq!(exit, 0, "{target}: {}", captured.stderr_text());
        common::stdout_json(&captured)["decision"].clone()
    };
    let release = decide("alabsystems/aterm");
    assert_eq!(
        release["send"]["profile"],
        Value::from("release"),
        "{release}"
    );
    assert_eq!(release["send"]["tier"], Value::from("release"), "{release}");
    let channel = decide("@channel");
    assert_eq!(
        channel["refuse"]["code"],
        Value::from("release_target_for_write"),
        "{channel}"
    );
    let dev = decide("fixture-dev/app");
    assert_eq!(dev["send"]["profile"], Value::from("dev"), "{dev}");
    assert!(fake.seen().is_empty(), "whoami --for sends nothing");
}

#[test]
fn whoami_verify_keeps_a_matching_login() {
    let root = common::scratch("whoami-verify");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::canonical_tokens(root.path(), &fake, true, false);
    common::write_config(&dirs, &common::canonical_config(root.path()));
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["whoami", "--verify"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let rows = identity_rows(&captured);
    assert_eq!(rows[0]["profile"], Value::from("dev"));
    assert_eq!(rows[0]["presence"], Value::from("present"));
    assert!(rows[0]["why"].is_null(), "{}", rows[0]);
    let users: Vec<_> = fake
        .seen()
        .into_iter()
        .filter(|s| s.path_and_query == "/user")
        .collect();
    assert_eq!(users.len(), 1, "only the present profile is verified");
}

/// One dev profile over `{root}/tokens/dev.token`, binding `login`.
fn one_dev_profile(root: &Path, login: &str) -> String {
    format!(
        "[profiles.dev]\ntoken_file = \"{}\"\ntier = \"dev\"\nlogin = \"{login}\"\n",
        root.join("tokens").join("dev.token").display()
    )
}

#[test]
fn whoami_never_prints_an_unprefixed_token_pasted_into_login() {
    // No name rule holds a login, and the Redactor knows no unprefixed shape: the row masks
    // the long hex run itself.
    let root = common::scratch("whoami-pasted");
    let fake = FakeGithub::start();
    let pasted = agh_fake::token("pasted")[4..].to_string();
    common::write_config(&dirs(root.path()), &one_dev_profile(root.path(), &pasted));
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let rows = identity_rows(&captured);
    assert_eq!(rows[0]["profile"], Value::from("dev"));
    assert!(!captured.stdout_text().contains(&pasted), "{rows:?}");
    assert!(fake.seen().is_empty());
}

#[test]
fn whoami_verify_names_a_login_github_disagrees_with() {
    let root = common::scratch("whoami-mismatch");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    // The fake maps the dev token to fixture-dev-bot; agh.toml binds another login.
    common::canonical_tokens(root.path(), &fake, true, false);
    common::write_config(&dirs, &one_dev_profile(root.path(), "fixture-reader"));
    validated(&dirs);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["whoami", "--verify"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let rows = identity_rows(&captured);
    assert_eq!(rows[0]["profile"], Value::from("dev"));
    assert_eq!(rows[0]["presence"], Value::from("present"));
    let why = rows[0]["why"].as_str().unwrap_or_default();
    assert!(why.starts_with("login_mismatch:"), "{}", rows[0]);
    assert!(
        why.contains("fixture-reader") && why.contains("fixture-dev-bot"),
        "{why}"
    );
    assert!(
        error_lines_of(&captured).is_empty(),
        "{}",
        captured.stderr_text()
    );
}

#[test]
fn whoami_verify_marks_a_token_github_refuses_unusable() {
    let root = common::scratch("whoami-401");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    // A token the fake does not know: GET /user answers 401.
    common::write_token(
        &root.path().join("tokens"),
        "dev.token",
        &agh_fake::token("unknown-to-the-fake"),
    );
    common::write_config(&dirs, &one_dev_profile(root.path(), "fixture-dev-bot"));
    validated(&dirs);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["whoami", "--verify"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let rows = identity_rows(&captured);
    assert_eq!(rows[0]["profile"], Value::from("dev"));
    assert_eq!(rows[0]["presence"], Value::from("unusable"), "{}", rows[0]);
    assert_eq!(
        rows[0]["why"],
        Value::from("github_refused: 401"),
        "{}",
        rows[0]
    );
    assert!(
        error_lines_of(&captured).is_empty(),
        "{}",
        captured.stderr_text()
    );
}

/// stderr's error lines: every `agh: ` line but the receipt's `agh: ok `.
fn error_lines_of(captured: &Captured) -> Vec<String> {
    captured
        .stderr_text()
        .lines()
        .filter(|line| line.starts_with("agh: ") && !line.starts_with("agh: ok "))
        .map(str::to_string)
        .collect()
}

#[test]
fn whoami_verify_leaves_a_profile_that_binds_no_login_alone() {
    let root = common::scratch("whoami-unbound");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::canonical_tokens(root.path(), &fake, true, false);
    let unbound = format!(
        "[profiles.dev]\ntoken_file = \"{}\"\ntier = \"dev\"\n\n[[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\"]\n",
        root.path().join("tokens").join("dev.token").display()
    );
    common::write_config(&dirs, &unbound);
    validated(&dirs);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["whoami", "--verify"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let rows = identity_rows(&captured);
    assert_eq!(rows[0]["profile"], Value::from("dev"));
    assert_eq!(rows[0]["presence"], Value::from("present"));
    assert!(rows[0]["login"].is_null(), "{}", rows[0]);
    assert!(rows[0]["why"].is_null(), "{}", rows[0]);
}

#[test]
fn auth_remove_refuses_a_file_that_is_already_invalid_and_changes_nothing() {
    let root = common::scratch("auth-remove-invalid");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::write_config(&dirs, ""); // the private config directory, for tokens/
    let token = common::write_token(&dirs.tokens(), "b.token", &agh_fake::token("shared"));
    // L7: two profiles name one file.
    let text = format!(
        "[profiles.a]\ntoken_file = \"{shared}\"\ntier = \"dev\"\n\n[profiles.b]\ntoken_file = \"{shared}\"\ntier = \"dev\"\n",
        shared = token.display()
    );
    common::write_config(&dirs, &text);
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "b"]),
        1,
        "{}",
        captured.stderr_text()
    );
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(error["kind"], Value::from("refused"), "{error}");
    assert!(
        error["next"]
            .as_str()
            .is_some_and(|next| next.contains("policy check")),
        "{error}"
    );
    assert_eq!(config_text(&dirs), text);
    assert!(token.exists(), "the token another profile names is kept");
}

#[test]
fn auth_remove_of_a_linked_agh_directory_gives_the_loads_refusal() {
    let root = common::scratch("auth-remove-linked-dir");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    // A private agh directory elsewhere, holding a valid agh.toml, and the agh directory a
    // link to it.
    let real = Dirs {
        config: root.path().join("elsewhere").join("agh"),
        ..dirs.clone()
    };
    common::write_config(&real, "");
    let token = common::write_token(&real.tokens(), "dev.token", &agh_fake::token("linked"));
    let text = format!(
        "[profiles.dev]\ntoken_file = \"{}\"\ntier = \"dev\"\n",
        token.display()
    );
    common::write_config(&real, &text);
    std::fs::create_dir_all(dirs.config.parent().expect("a parent")).expect("the parent");
    std::os::unix::fs::symlink(&real.config, &dirs.config).expect("the link");

    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "dev"]),
        1,
        "{}",
        captured.stderr_text()
    );
    let removed = common::error_json(&captured.stderr_text());
    assert_eq!(removed["kind"], Value::from("refused"), "{removed}");
    let next = removed["next"].as_str().expect("a next");
    assert!(
        next.contains(&dirs.config.display().to_string()),
        "the remedy names the linked directory: {next}"
    );

    // `whoami` reads agh.toml through `config::load`: the same refusal, word for word.
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    assert_eq!(run(&mut ctx, &["whoami"]), 1, "{}", captured.stderr_text());
    let loaded = common::error_json(&captured.stderr_text());
    assert_eq!(removed["next"], loaded["next"]);
    assert_eq!(removed["why"], loaded["why"]);
    assert_eq!(config_text(&real), text, "nothing was edited");
    assert!(token.exists());
}

#[test]
fn a_linked_tokens_directory_is_never_deleted_from() {
    let root = common::scratch("auth-linked-tokens");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    // The person's own token, and a file beside it at the name agh stages under, in a
    // private directory of theirs, with agh's tokens/ a link to that directory.
    let secrets = root.path().join("secrets");
    let token = common::write_token(&secrets, "dev.token", &agh_fake::token("linked-tokens"));
    let beside = common::write_token(&secrets, "dev.token.agh-new", &agh_fake::token("beside"));
    let text = format!(
        "[profiles.dev]\ntoken_file = \"{}\"\ntier = \"dev\"\n",
        token.display()
    );
    common::write_config(&dirs, &text);
    std::os::unix::fs::symlink(&secrets, dirs.tokens()).expect("the link");
    validated(&dirs);

    // A stdin add that is usage (an empty token) touches nothing.
    let (mut ctx, captured) = person_with(root.path(), &fake, "");
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--stdin", "--tier", "dev"],
    );
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert!(beside.exists(), "a usage error deletes nothing");

    // Removing the profile edits agh.toml and deletes no file reached through the link.
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "dev"]),
        0,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(
        common::stdout_json(&captured)["token_file_deleted"],
        Value::from(false)
    );
    assert!(token.exists(), "the person's token is kept");
    assert!(beside.exists(), "the file beside it is kept");
    assert!(
        parsed(&dirs)
            .get("profiles")
            .and_then(|p| p.get("dev"))
            .is_none()
    );
}

#[test]
fn auth_add_under_a_parent_it_cannot_write_could_not_run() {
    let root = common::scratch("auth-unwritable-parent");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    let parent = dirs.config.parent().expect("a parent");
    std::fs::create_dir_all(parent).expect("the parent");
    common::chmod(parent, 0o500);
    let probe = parent.join("probe");
    if std::fs::create_dir(&probe).is_ok() {
        // A user the mode does not bind (root): nothing here is unwritable to it.
        let _ = std::fs::remove_dir(&probe);
        common::chmod(parent, 0o700);
        return;
    }
    let token = agh_fake::token("unwritable");
    fake.add_token(&token, "fixture-dev-bot");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--stdin", "--tier", "dev"],
    );
    common::chmod(parent, 0o700);
    // No directory exists to chmod: an environment fault, never a refusal with a remedy.
    assert_eq!(exit, 3, "{}", captured.stderr_text());
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(error["kind"], Value::from("could_not_run"), "{error}");
    assert!(!dirs.config.exists());
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn auth_remove_edits_an_inline_default_table() {
    let root = common::scratch("auth-remove-inline");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::write_config(&dirs, ""); // the private config directory, for tokens/
    let a = common::write_token(&dirs.tokens(), "a.token", &agh_fake::token("inline-a"));
    let b = common::write_token(&dirs.tokens(), "b.token", &agh_fake::token("inline-b"));
    let text = format!(
        "default = {{ read = [\"a\", \"b\", \"anonymous\"] }}\n\n[profiles.a]\ntoken_file = \"{}\"\ntier = \"dev\"\n\n[profiles.b]\ntoken_file = \"{}\"\ntier = \"dev\"\n",
        a.display(),
        b.display()
    );
    common::write_config(&dirs, &text);
    validated(&dirs);
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    assert_eq!(
        run(&mut ctx, &["auth", "remove", "b"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let doc = parsed(&dirs);
    let default = doc.get("default").expect("default");
    assert_eq!(strings(default.get("read")), ["a", "anonymous"]);
    assert!(doc.get("profiles").and_then(|p| p.get("b")).is_none());
    validated(&dirs);
}

#[test]
fn a_pure_edit_of_text_that_does_not_parse_names_only_the_line() {
    let raw = "opaque-unquoted-value-0123456789";
    let text = format!("[profiles.a]\ntier = \"dev\"\nlogin = {raw}\n");
    let error = agh::identity::provision::remove_from_config(&text, "a").expect_err("no parse");
    assert_eq!(error.kind, agh::error::Kind::Usage);
    assert!(!error.why.contains(raw), "{}", error.why);
    assert!(error.why.contains("line 3"), "{}", error.why);
}

#[test]
fn an_edit_and_policy_check_name_the_same_line_for_an_error_at_end_of_input() {
    // The parser places this error at end of input, after the final newline: both readers
    // name the last line, not the empty one after it.
    let text = "x = 1\na = [\n";
    let edit = agh::identity::provision::remove_from_config(text, "a").expect_err("no parse");
    assert!(edit.why.ends_with(" at line 2"), "{}", edit.why);
    let check = agh::config::parse(text).expect_err("no parse");
    assert!(check.what.contains(": line 2: "), "{}", check.what);
}

// ---------------------------------------------------------------- inline spellings and reach

#[test]
fn auth_add_re_provisions_an_inline_profile_row_and_drops_its_reach_claim() {
    let root = common::scratch("auth-inline-row");
    let fake = FakeGithub::start();
    let old = agh_fake::token("inline-old");
    let new = agh_fake::token("inline-new");
    fake.add_token(&old, "fixture-dev-bot");
    fake.add_token(&new, "fixture-reader");
    let dirs = dirs(root.path());
    let old_file = common::write_token(&root.path().join("elsewhere"), "dev.token", &old);
    for (case, text) in [
        (
            "an inline row",
            format!(
                "[profiles]\ndev = {{ token_file = \"{}\", tier = \"dev\", login = \"fixture-dev-bot\", covers_all_repos = true }}\n",
                old_file.display()
            ),
        ),
        (
            "an inline profiles table",
            format!(
                "profiles = {{ dev = {{ token_file = \"{}\", tier = \"dev\", login = \"fixture-dev-bot\", covers_all_repos = true }} }}\n",
                old_file.display()
            ),
        ),
    ] {
        common::write_config(&dirs, &text);
        let before = validated(&dirs);
        assert!(
            before.profile("dev").is_some_and(|p| p.covers_all_repos),
            "{case}"
        );
        let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{new}\n"));
        let exit = run(
            &mut ctx,
            &["auth", "add", "dev", "--stdin", "--tier", "dev"],
        );
        assert_eq!(exit, 0, "{case}: {}", captured.stderr_text());
        let router = validated(&dirs);
        let profile = router.profile("dev").expect("the dev profile");
        assert_eq!(profile.login.as_deref(), Some("fixture-reader"), "{case}");
        assert!(!profile.covers_all_repos, "{case}");
        assert_eq!(
            profile.source,
            identity::Source::File(dirs.tokens().join("dev.token")),
            "{case}"
        );
    }
}

#[test]
fn auth_add_refuses_an_inline_owners_array_before_any_request() {
    let root = common::scratch("auth-inline-owners");
    let fake = FakeGithub::start();
    let token = agh_fake::token("inline-owners");
    fake.add_token(&token, "fixture-dev-bot");
    let dirs = dirs(root.path());
    let text = "owners = [{ name = \"fixture-dev\", tier = \"dev\", read = [\"anonymous\"] }]\n";
    common::write_config(&dirs, text);
    validated(&dirs);
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let mut args = ADD_DEV.to_vec();
    args.push("json");
    assert_eq!(run(&mut ctx, &args), 2, "{}", captured.stderr_text());
    let error = common::error_json(&captured.stderr_text());
    assert!(
        error["next"]
            .as_str()
            .is_some_and(|next| next.contains("policy check")),
        "{error}"
    );
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
    assert!(names_in(&dirs.tokens()).is_empty());
    assert_eq!(config_text(&dirs), text);
}

#[test]
fn auth_remove_edits_an_inline_owners_array() {
    let root = common::scratch("auth-remove-inline-owners");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::write_config(&dirs, ""); // the private config directory, for tokens/
    let a = common::write_token(&dirs.tokens(), "a.token", &agh_fake::token("owners-a"));
    let b = common::write_token(&dirs.tokens(), "b.token", &agh_fake::token("owners-b"));
    let text = format!(
        "owners = [{{ name = \"fixture-dev\", tier = \"dev\", read = [\"a\", \"b\"], write = \"b\" }}, {{ name = \"fixture-ext\", tier = \"dev\", read = [\"a\"] }}]\n\n[profiles.a]\ntoken_file = \"{}\"\ntier = \"dev\"\nlogin = \"fixture-reader\"\n\n[profiles.b]\ntoken_file = \"{}\"\ntier = \"dev\"\nlogin = \"fixture-dev-bot\"\n",
        a.display(),
        b.display()
    );
    common::write_config(&dirs, &text);
    validated(&dirs);
    let (mut ctx, captured) = person(root.path(), &fake, common::env());
    let exit = run(&mut ctx, &["auth", "remove", "b", "--format", "json"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(
        common::stdout_json(&captured)["owners_edited"],
        aterm_json::json!(["fixture-dev"])
    );
    let doc = parsed(&dirs);
    let row = owner(&doc, "fixture-dev");
    assert_eq!(strings(row.get("read")), ["a"]);
    assert!(row.get("write").is_none(), "{row:?}");
    assert_eq!(strings(owner(&doc, "fixture-ext").get("read")), ["a"]);
    validated(&dirs);
}

#[test]
fn auth_add_refuses_anonymous() {
    let root = common::scratch("auth-anonymous");
    let fake = FakeGithub::start();
    let token = agh_fake::token("anonymous-add");
    fake.add_token(&token, "fixture-dev-bot");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let exit = run(
        &mut ctx,
        &[
            "--anonymous",
            "auth",
            "add",
            "dev",
            "--stdin",
            "--tier",
            "dev",
        ],
    );
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert!(fake.seen().is_empty());
    assert!(!dirs(root.path()).config_file().exists());
}

#[test]
fn whoami_verify_obeys_the_identity_flags() {
    let root = common::scratch("whoami-verify-flags");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::canonical_tokens(root.path(), &fake, true, true);
    common::write_config(&dirs, &common::canonical_config(root.path()));

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let exit = run(&mut ctx, &["--anonymous", "whoami", "--verify"]);
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert!(
        fake.seen().iter().all(|s| s.as_login.is_none()),
        "no credential under --anonymous"
    );

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let exit = run(&mut ctx, &["--as", "dev", "whoami", "--verify"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let users: Vec<Option<String>> = fake
        .seen()
        .into_iter()
        .filter(|s| s.path_and_query == "/user")
        .map(|s| s.as_login)
        .collect();
    assert_eq!(users, [Some("fixture-dev-bot".to_string())]);

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let exit = run(&mut ctx, &["--as", "nobody", "whoami", "--verify"]);
    assert_eq!(exit, 2, "{}", captured.stderr_text());
}

#[test]
fn an_ambient_token_a_profile_names_is_reported_as_that_profiles() {
    let root = common::scratch("whoami-ambient-used");
    let fake = FakeGithub::start();
    let dirs = dirs(root.path());
    common::write_config(
        &dirs,
        "[profiles.mine]\nenv = \"GH_TOKEN\"\ntier = \"dev\"\n",
    );
    let ambient = agh_fake::token("ambient-used");
    let env = MapEnv::new().with("GH_TOKEN", &ambient);
    let (mut ctx, captured) = common::ctx(root.path(), &fake, env);
    assert_eq!(run(&mut ctx, &["whoami"]), 0, "{}", captured.stderr_text());
    let rows = identity_rows(&captured);
    assert_eq!(rows[0]["profile"], Value::from("mine"));
    assert_eq!(rows[0]["source"], Value::from("env:GH_TOKEN"));
    let ambient_row = &rows[1];
    assert!(ambient_row["profile"].is_null(), "{ambient_row}");
    assert!(
        ambient_row["remedy"]
            .as_str()
            .is_some_and(|r| r.contains("mine")),
        "{ambient_row}"
    );
}

// ---------------------------------------------------------------- metering, unnameable files, quoted remedies

#[test]
fn auth_add_meters_the_get_user_it_sends_as_the_new_token() {
    let root = common::scratch("auth-metered");
    let fake = FakeGithub::start();
    let token = agh_fake::token("metered");
    fake.add_token(&token, "fixture-dev-bot");
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    let mut args = ADD_DEV.to_vec();
    args.extend(["json", "--meta"]);
    assert_eq!(run(&mut ctx, &args), 0, "{}", captured.stderr_text());
    let stderr = captured.stderr_text();
    let receipt = stderr
        .lines()
        .find(|line| line.starts_with("agh: ok "))
        .unwrap_or_else(|| panic!("a receipt: {stderr}"));
    let pairs: Vec<&str> = receipt.split(' ').collect();
    assert!(pairs.contains(&"reqs=1"), "{receipt}");
    assert!(pairs.contains(&"as=dev"), "{receipt}");
    assert_eq!(fake.seen().len(), 1, "{:?}", fake.seen());
}

#[test]
fn auth_add_refuses_a_token_file_agh_toml_cannot_name() {
    use std::os::unix::ffi::OsStrExt;
    let root = common::scratch("auth-not-utf8");
    let fake = FakeGithub::start();
    let token = agh_fake::token("not-utf8");
    fake.add_token(&token, "fixture-dev-bot");
    let (mut ctx, _captured) = person(root.path(), &fake, common::env());
    let gh = ctx.github().expect("the client");
    // A stdin token under a config directory whose path is not UTF-8 (an XDG_CONFIG_HOME
    // outside home): refused before any directory is made or request sent. Without the
    // check the directory's creation is the first failure (could_not_run), or, where the
    // file system takes the name, agh.toml names the token lossily.
    let mut odd = dirs(root.path());
    odd.config = root
        .path()
        .join(std::ffi::OsStr::from_bytes(b"cfg-\xff"))
        .join("agh");
    let req = identity::provision::AddRequest {
        name: "dev".to_string(),
        from: identity::provision::AddFrom::Stdin,
        tier: agh::policy::Tier::Dev,
        owners: Vec::new(),
        write: false,
        drafts: false,
    };
    let mut stdin = std::io::Cursor::new(format!("{token}\n").into_bytes());
    let error = identity::provision::add(&gh, &odd, &req, &mut stdin)
        .expect_err("a path that is not UTF-8 is refused");
    assert_eq!(error.kind, agh::error::Kind::Usage, "{error:?}");
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
    assert!(!odd.config.exists());
}

#[test]
fn a_loose_config_directory_with_a_space_gets_a_quoted_chmod() {
    let root = common::scratch("auth-spaced-dir");
    let fake = FakeGithub::start();
    let token = agh_fake::token("spaced-dir");
    fake.add_token(&token, "fixture-dev-bot");
    let mut spaced = dirs(root.path());
    spaced.config = root.path().join("My Config").join("aterm").join("agh");
    std::fs::create_dir_all(spaced.config.parent().expect("a parent")).expect("the parent");
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&spaced.config)
        .expect("a loose config directory");
    common::chmod(&spaced.config, 0o755);
    let (mut ctx, captured) = person_with(root.path(), &fake, &format!("{token}\n"));
    ctx.dirs = Some(spaced.clone());
    let exit = run(
        &mut ctx,
        &["auth", "add", "dev", "--stdin", "--tier", "dev"],
    );
    assert_eq!(exit, 1, "{}", captured.stderr_text());
    assert_eq!(mode(&spaced.config), 0o755);
    let error = common::error_json(&captured.stderr_text());
    let word = format!("'{}'", spaced.config.display());
    let next = error["next"].as_str().unwrap_or_default();
    assert!(next.starts_with(&format!("chmod 700 {word} ")), "{error}");
    assert!(next.contains(&format!("chmod 755 {word}")), "{error}");
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}
