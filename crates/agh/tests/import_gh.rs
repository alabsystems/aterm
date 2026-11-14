// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh auth import --from-gh` over scripted children (WP8).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agh::cli::{Captured, Ctx};
use agh::config::Dirs;
use agh::fx::Fx;
use agh::fx::child::{ChildOut, ScriptedChild};
use agh::identity::MapEnv;
use agh::identity::import::GH_ABSOLUTE;
use agh_fake::FakeGithub;
use aterm_json::Value;

use crate::common;

/// `gh auth token --hostname github.com`, then `extra`.
fn gh_args(extra: &[&str]) -> Vec<String> {
    ["auth", "token", "--hostname", "github.com"]
        .iter()
        .chain(extra)
        .map(|a| (*a).to_string())
        .collect()
}

/// The scripted `gh auth token` with `extra` arguments, exiting `code` with `stdout` and
/// `stderr`.
fn gh(extra: &[&str], code: Option<i32>, stdout: &str, stderr: &str) -> ScriptedChild {
    ScriptedChild {
        program: PathBuf::from(GH_ABSOLUTE[0]),
        args: gh_args(extra),
        out: ChildOut {
            code,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        },
    }
}

/// The environment the caller runs in: `HOME` and `PATH`, and the two token variables gh is
/// never handed.
fn caller_env(root: &Path) -> MapEnv {
    MapEnv::new()
        .with("HOME", &root.join("home").display().to_string())
        .with("PATH", "/usr/bin:/bin")
        .with("GH_TOKEN", &agh_fake::token("ambient-gh"))
        .with("GITHUB_TOKEN", &agh_fake::token("ambient-github"))
}

/// A CLI context over `fake` whose effect seam answers `children`, stderr a terminal.
fn scripted_ctx(
    root: &Path,
    fake: &FakeGithub,
    children: Vec<ScriptedChild>,
) -> (Ctx, Captured, Arc<Fx>) {
    let (mut ctx, captured) = common::ctx(root, fake, caller_env(root));
    let fx = Arc::new(Fx::simulated(common::SIM_START).with_children(children));
    ctx.fx = fx.clone();
    (ctx.with_terminals(false, true), captured, fx)
}

const IMPORT: [&str; 9] = [
    "auth",
    "import",
    "dev2",
    "--from-gh",
    "--tier",
    "dev",
    "--owner",
    "fixture-dev",
    "--format",
];

fn import_args(extra: &[&'static str]) -> Vec<&'static str> {
    let mut args = IMPORT.to_vec();
    args.push("json");
    args.extend_from_slice(extra);
    args
}

fn token_file(dirs: &Dirs) -> PathBuf {
    dirs.tokens().join("dev2.token")
}

#[test]
fn import_provisions_gh_s_token() {
    let root = common::scratch("import");
    let fake = FakeGithub::start();
    let token = agh_fake::token("import");
    fake.add_token(&token, "fixture-dev-bot");
    let (mut ctx, captured, fx) = scripted_ctx(
        root.path(),
        &fake,
        vec![gh(&[], Some(0), &format!("{token}\n"), "")],
    );
    let exit = common::run_cli(&mut ctx, &import_args(&[]));
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(
        common::stdout_json(&captured)["login"],
        Value::from("fixture-dev-bot")
    );
    let dirs = Dirs::under(root.path());
    let file = token_file(&dirs);
    let mode = std::fs::metadata(&file)
        .expect("the token file")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    // `auth add --stdin`'s bytes exactly: the token and one newline.
    assert_eq!(
        std::fs::read(&file).expect("the token file"),
        format!("{token}\n").into_bytes()
    );
    let config = std::fs::read_to_string(dirs.config_file()).expect("agh.toml");
    assert!(config.contains("[profiles.dev2]"), "{config}");
    let run = fx.children_run();
    assert_eq!(run.len(), 1);
    let mut names: Vec<String> = run[0]
        .env
        .iter()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["HOME", "PATH"]);
}

#[test]
fn user_is_passed_to_gh() {
    let root = common::scratch("import-user");
    let fake = FakeGithub::start();
    let token = agh_fake::token("import-user");
    fake.add_token(&token, "fixture-dev-bot");
    let (mut ctx, captured, fx) = scripted_ctx(
        root.path(),
        &fake,
        vec![gh(
            &["--user", "fixture-dev-bot"],
            Some(0),
            &format!("{token}\n"),
            "",
        )],
    );
    let exit = common::run_cli(&mut ctx, &import_args(&["--user", "fixture-dev-bot"]));
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let args: Vec<String> = fx.children_run()[0]
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args[args.len() - 2..], ["--user", "fixture-dev-bot"]);
}

#[test]
fn a_failing_gh_is_unauthenticated_and_quotes_nothing() {
    let root = common::scratch("import-fails");
    let fake = FakeGithub::start();
    let dirs = Dirs::under(root.path());
    common::write_config(&dirs, "# kept as it is\n");
    let before = std::fs::read(dirs.config_file()).expect("agh.toml");
    let (mut ctx, captured, _fx) = scripted_ctx(
        root.path(),
        &fake,
        vec![gh(
            &[],
            Some(1),
            "",
            "no oauth token found for github.com account someone",
        )],
    );
    let exit = common::run_cli(&mut ctx, &import_args(&[]));
    assert_eq!(exit, 4, "{}", captured.stderr_text());
    let error = common::error_json(&captured.stderr_text());
    let why = error["why"].as_str().unwrap_or_default();
    assert!(why.contains("exited 1"), "{why}");
    assert!(!captured.stderr_text().contains("someone"));
    assert!(!token_file(&dirs).exists());
    assert_eq!(std::fs::read(dirs.config_file()).expect("agh.toml"), before);
    assert!(fake.seen().is_empty());
}

#[test]
fn two_lines_are_invalid() {
    let root = common::scratch("import-lines");
    let fake = FakeGithub::start();
    let (mut ctx, captured, _fx) =
        scripted_ctx(root.path(), &fake, vec![gh(&[], Some(0), "a\nb\n", "")]);
    let exit = common::run_cli(&mut ctx, &import_args(&[]));
    assert_eq!(exit, 10, "{}", captured.stderr_text());
    let dirs = Dirs::under(root.path());
    assert!(!token_file(&dirs).exists());
    assert!(!dirs.config_file().exists());
    assert!(fake.seen().is_empty());
}

#[test]
fn a_child_that_cannot_start_names_the_vault_token() {
    let root = common::scratch("import-nochild");
    let fake = FakeGithub::start();
    let (mut ctx, captured, _fx) = scripted_ctx(root.path(), &fake, Vec::new());
    let exit = common::run_cli(&mut ctx, &import_args(&[]));
    assert_eq!(exit, 3, "{}", captured.stderr_text());
    let error = common::error_json(&captured.stderr_text());
    let next = error["next"].as_str().unwrap_or_default();
    assert!(next.contains("aterm secrets need gh:dev"), "{next}");
    assert!(!Dirs::under(root.path()).config_file().exists());
}

/// An agent imports as a person does, so neither a stderr that is no terminal nor a terminal
/// stdin stops the import.
#[test]
fn import_runs_without_a_terminal() {
    for stdin_terminal in [false, true] {
        let root = common::scratch("import-noterm");
        let fake = FakeGithub::start();
        let token = agh_fake::token("import-noterm");
        fake.add_token(&token, "fixture-dev-bot");
        let (ctx, captured) = common::ctx(root.path(), &fake, caller_env(root.path()));
        let mut ctx = ctx.with_stdin_terminal(stdin_terminal);
        let fx = Arc::new(Fx::simulated(common::SIM_START).with_children(vec![gh(
            &[],
            Some(0),
            &format!("{token}\n"),
            "",
        )]));
        ctx.fx = fx.clone();
        let exit = common::run_cli(&mut ctx, &import_args(&[]));
        assert_eq!(
            exit,
            0,
            "stdin terminal {stdin_terminal}: {}",
            captured.stderr_text()
        );
        assert_eq!(fx.children_run().len(), 1);
        let config =
            std::fs::read_to_string(Dirs::under(root.path()).config_file()).expect("agh.toml");
        assert!(config.contains("[profiles.dev2]"), "{config}");
    }
}

#[test]
fn import_takes_no_file() {
    let root = common::scratch("import-nofile");
    let fake = FakeGithub::start();
    let (mut ctx, captured, fx) = scripted_ctx(root.path(), &fake, Vec::new());
    let exit = common::run_cli(&mut ctx, &import_args(&["--file", "x"]));
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    // The row declares no `--file`, so the parser's refusal names the flag.
    let error = common::error_json(&captured.stderr_text());
    let why = error["why"].as_str().unwrap_or_default();
    assert!(why.contains("--file"), "{error}");
    assert!(fx.children_run().is_empty());

    let (mut ctx, captured, fx) = scripted_ctx(root.path(), &fake, Vec::new());
    let exit = common::run_cli(
        &mut ctx,
        &[
            "auth", "import", "dev2", "--tier", "dev", "--format", "json",
        ],
    );
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert!(fx.children_run().is_empty());
    assert!(!Dirs::under(root.path()).config_file().exists());
}

#[test]
fn import_is_not_taped() {
    let root = common::scratch("import-tape");
    let fake = FakeGithub::start();
    let tape = common::tape_path(root.path(), "t");
    let tape_arg = tape.display().to_string();
    let (mut ctx, captured, fx) = scripted_ctx(root.path(), &fake, Vec::new());
    let mut args = vec!["--record", tape_arg.as_str()];
    args.extend(import_args(&[]));
    let exit = common::run_cli(&mut ctx, &args);
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert!(!tape.exists());
    assert!(fx.children_run().is_empty());
}
