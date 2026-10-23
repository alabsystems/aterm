// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! CLI `--record` and `--replay` tape verification and round-trip tests.

use std::path::Path;
use std::sync::Arc;

use agh::cli::{Captured, Ctx};
use agh::fx::tape::{self, TapeHeader};
use agh::fx::{DOOR_EXIT, DOOR_STDOUT};
use agh::identity::MapEnv;
use agh::wire::Endpoints;
use agh::wire::scripted::ScriptedTransport;
use astream_effects::EffectRecord;

use crate::common::{run_cli, scratch, tape_path};

fn scripted_ctx(root: &Path, transport: &Arc<ScriptedTransport>) -> (Ctx, Captured) {
    Ctx::for_tests(
        root,
        transport.clone(),
        Endpoints::github_com(),
        MapEnv::new(),
    )
}

fn has_field(captured: &Captured, field: &str) -> bool {
    captured
        .stderr_text()
        .split_whitespace()
        .any(|word| word == field)
}

fn error_why(captured: &Captured) -> String {
    let text = captured.stderr_text();
    text.lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .find_map(|line| line.get("error")?.get("why")?.as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("an error line with a why: {text}"))
}

fn write_tape(path: &Path, argv: &[&str], out: &[u8]) {
    let argv: Vec<String> = argv.iter().map(|a| (*a).to_string()).collect();
    let header = TapeHeader::for_run(&argv, &Endpoints::github_com());
    let records = [
        EffectRecord::Emit {
            door: DOOR_STDOUT.to_string(),
            bytes: out.to_vec(),
        },
        EffectRecord::Emit {
            door: DOOR_EXIT.to_string(),
            bytes: b"0".to_vec(),
        },
    ];
    std::fs::write(path, tape::render(&header, &records)).expect("a tape");
}

#[test]
fn a_verb_a_tape_cannot_hold_is_refused_and_no_file_is_made() {
    let dir = scratch("tapes");
    let transport = Arc::new(ScriptedTransport::new());
    let t = tape_path(dir.path(), "t");
    let t = t.to_str().expect("a UTF-8 path");
    for args in [
        vec!["--record", t, "auth", "status"],
        vec![
            "--record",
            t,
            "--as",
            "dev",
            "release",
            "list",
            "fixture-dev/app",
        ],
        vec![
            "--record",
            t,
            "release",
            "download",
            "fixture-dev/app",
            "v1",
            "--asset",
            "x",
        ],
        vec!["--record", t, "whoami"],
    ] {
        let (mut ctx, captured) = scripted_ctx(dir.path(), &transport);
        assert_eq!(
            run_cli(&mut ctx, &args),
            2,
            "{args:?}: {}",
            captured.stderr_text()
        );
        assert!(!Path::new(t).exists(), "{args:?}");
    }
    assert!(transport.requests().is_empty());
}

#[test]
fn a_command_line_that_holds_a_token_shape_is_never_taped() {
    let dir = scratch("tapes");
    let transport = Arc::new(ScriptedTransport::new());
    let t = tape_path(dir.path(), "t");
    let t_arg = t.to_str().expect("a UTF-8 path");
    let shaped = format!("/repos/fixture-dev/{}", agh_fake::token("argv"));
    let (mut ctx, captured) = scripted_ctx(dir.path(), &transport);
    let exit = run_cli(&mut ctx, &["--record", t_arg, "api", &shaped]);
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    let why = error_why(&captured);
    assert!(why.split_whitespace().any(|w| w == "2"), "{why}");
    assert!(!t.exists());
    assert!(transport.requests().is_empty());
}

#[test]
fn a_replay_compares_the_typed_command_line_shaped_as_the_tape_holds_it() {
    let dir = scratch("tapes");
    let transport = Arc::new(ScriptedTransport::new());
    let p = tape_path(dir.path(), "p");
    write_tape(&p, &["api", "/repos/fixture-dev/[redacted]"], b"{}\n");
    let shaped = format!("/repos/fixture-dev/{}", agh_fake::token("argv"));
    let (mut ctx, captured) = scripted_ctx(dir.path(), &transport);
    let p_arg = p.to_str().expect("a UTF-8 path");
    let exit = run_cli(&mut ctx, &["--replay", p_arg, "api", &shaped]);
    assert_eq!(exit, 3, "{}", captured.stderr_text());
    assert!(
        has_field(&captured, "sub=replay.kind"),
        "{}",
        captured.stderr_text()
    );
    assert!(transport.requests().is_empty());
}

#[test]
fn record_never_overwrites_a_file() {
    let dir = scratch("tapes");
    let transport = Arc::new(ScriptedTransport::new());
    let t = tape_path(dir.path(), "kept");
    std::fs::write(&t, b"someone's file").expect("a file");
    let (mut ctx, captured) = scripted_ctx(dir.path(), &transport);
    let t_arg = t.to_str().expect("a UTF-8 path");
    let exit = run_cli(
        &mut ctx,
        &["--record", t_arg, "repo", "view", "fixture-dev/app"],
    );
    assert_eq!(exit, 3, "{}", captured.stderr_text());
    assert!(
        has_field(&captured, "sub=local"),
        "{}",
        captured.stderr_text()
    );
    assert_eq!(std::fs::read(&t).expect("the file"), b"someone's file");
    assert!(transport.requests().is_empty());
}

#[test]
fn a_replay_with_another_command_line_is_a_usage_error() {
    let dir = scratch("tapes");
    let transport = Arc::new(ScriptedTransport::new());
    let p = tape_path(dir.path(), "p");
    write_tape(&p, &["repo", "view", "fixture-dev/app"], b"{}\n");
    let q = tape_path(dir.path(), "q");
    let p_arg = p.to_str().expect("a UTF-8 path");
    let q_arg = q.to_str().expect("a UTF-8 path");
    for args in [
        vec!["--replay", p_arg, "repo", "view", "fixture-dev/other"],
        vec![
            "--replay",
            p_arg,
            "--record",
            q_arg,
            "repo",
            "view",
            "fixture-dev/other",
        ],
    ] {
        let (mut ctx, captured) = scripted_ctx(dir.path(), &transport);
        assert_eq!(run_cli(&mut ctx, &args), 2, "{}", captured.stderr_text());
        assert!(
            has_field(&captured, "sub=replay.argv"),
            "{}",
            captured.stderr_text()
        );
        assert!(captured.stderr_text().contains("fixture-dev/app"));
    }
    assert!(!q.exists());
    assert!(transport.requests().is_empty());
}

#[test]
fn a_tape_that_is_missing_or_not_a_tape_stops_the_run() {
    let dir = scratch("tapes");
    let transport = Arc::new(ScriptedTransport::new());
    let missing = tape_path(dir.path(), "missing");
    let missing_arg = missing.to_str().expect("a UTF-8 path");
    let (mut ctx, captured) = scripted_ctx(dir.path(), &transport);
    assert_eq!(run_cli(&mut ctx, &["--replay", missing_arg]), 3);
    assert!(
        has_field(&captured, "sub=local"),
        "{}",
        captured.stderr_text()
    );

    let junk = tape_path(dir.path(), "junk");
    std::fs::write(&junk, b"atape-text 1\nnote x\n| y\n").expect("a file");
    let junk_arg = junk.to_str().expect("a UTF-8 path");
    for args in [
        vec!["--replay", junk_arg],
        vec!["--replay", junk_arg, "repo", "view", "fixture-dev/app"],
    ] {
        let (mut ctx, captured) = scripted_ctx(dir.path(), &transport);
        assert_eq!(run_cli(&mut ctx, &args), 3, "{}", captured.stderr_text());
        assert!(
            has_field(&captured, "sub=replay.format"),
            "{}",
            captured.stderr_text()
        );
    }

    let no_verb = tape_path(dir.path(), "no-verb");
    write_tape(&no_verb, &[], b"");
    let no_verb_arg = no_verb.to_str().expect("a UTF-8 path");
    let (mut ctx, captured) = scripted_ctx(dir.path(), &transport);
    assert_eq!(run_cli(&mut ctx, &["--replay", no_verb_arg]), 3);
    assert!(
        has_field(&captured, "sub=replay.format"),
        "{}",
        captured.stderr_text()
    );
    assert!(transport.requests().is_empty());
}

/// The round trips over agh-fake (plan §12a.3): a recording run speaks to the fake, and a
/// replay over an empty scripted transport answers the same. Each world holds the canonical
/// config and both token files, which a tape must not read (plan §1, rule 12: unix-only files).
#[cfg(unix)]
mod over_the_fake {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use agh::config::Dirs;
    use agh::fx::canon;
    use agh::fx::tape::{self, TapeEndpoints, TapeHeader, TapeMode};
    use agh::fx::{DOOR_EXIT, DOOR_GH, DOOR_SLEEP, DOOR_STDOUT};
    use agh::wire::scripted::ScriptedTransport;
    use agh_fake::{FakeGithub, ReleaseSpec, RepoSpec};
    use astream_effects::EffectRecord;

    use crate::common::{self, run_cli};

    const APP: &str = "fixture-dev/app";

    struct Ran {
        exit: u8,
        stdout: Vec<u8>,
        stderr: String,
    }

    struct World {
        root: aterm_tempfile::TempDir,
        fake: FakeGithub,
    }

    impl World {
        fn new(label: &str) -> World {
            let root = common::scratch(label);
            let fake = FakeGithub::start();
            common::canonical_tokens(root.path(), &fake, true, true);
            common::write_config(
                &Dirs::under(root.path()),
                &common::canonical_config(root.path()),
            );
            fake.add_repo(APP, RepoSpec::public().push("fixture-dev-bot"));
            World { root, fake }
        }

        fn tape(&self, label: &str) -> PathBuf {
            common::tape_path(self.root.path(), label)
        }

        fn live(&self, args: &[&str]) -> Ran {
            let (mut ctx, captured) = common::ctx(self.root.path(), &self.fake, common::env());
            let exit = run_cli(&mut ctx, args);
            Ran {
                exit,
                stdout: captured.stdout(),
                stderr: captured.stderr_text(),
            }
        }

        fn offline(&self, args: &[&str]) -> Ran {
            let fresh = common::scratch("tapes-replay");
            let transport = Arc::new(ScriptedTransport::new());
            let (mut ctx, captured) = agh::cli::Ctx::for_tests(
                fresh.path(),
                transport.clone(),
                common::endpoints(&self.fake),
                common::env(),
            );
            let exit = run_cli(&mut ctx, args);
            assert!(
                transport.requests().is_empty(),
                "{args:?} reached the transport: {:?}",
                transport.requests()
            );
            Ran {
                exit,
                stdout: captured.stdout(),
                stderr: captured.stderr_text(),
            }
        }
    }

    fn arg(path: &Path) -> &str {
        path.to_str().expect("a UTF-8 path")
    }

    fn read_tape(path: &Path) -> (TapeHeader, Vec<EffectRecord>) {
        let text = std::fs::read_to_string(path).expect("the tape");
        tape::parse(&text).unwrap_or_else(|e| panic!("{e:?}: {text}"))
    }

    fn exchanges(records: &[EffectRecord], door: &str) -> Vec<EffectRecord> {
        records
            .iter()
            .filter(|r| matches!(r, EffectRecord::Exchange { door: d, .. } if d == door))
            .cloned()
            .collect()
    }

    fn emit(records: &[EffectRecord], door: &str) -> Vec<u8> {
        let emits: Vec<&Vec<u8>> = records
            .iter()
            .filter_map(|r| match r {
                EffectRecord::Emit { door: d, bytes } if d == door => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(emits.len(), 1, "one {door} emit: {records:?}");
        emits[0].clone()
    }

    fn error(ran: &Ran) -> aterm_json::Value {
        let line = ran
            .stderr
            .lines()
            .nth(1)
            .unwrap_or_else(|| panic!("two stderr lines: {}", ran.stderr));
        aterm_json::from_str::<aterm_json::Value>(line)
            .expect("the error line")
            .get("error")
            .cloned()
            .expect("an error object")
    }

    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(at) = todo.pop() {
            for entry in std::fs::read_dir(&at).expect("a directory") {
                let path = entry.expect("an entry").path();
                if path.is_dir() {
                    todo.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out
    }

    fn record_repo_view(w: &World, p: &Path) -> Ran {
        let ran = w.live(&["--record", arg(p), "repo", "view", APP]);
        assert_eq!(ran.exit, 0, "{}", ran.stderr);
        assert!(!ran.stdout.is_empty());
        ran
    }

    #[test]
    fn a_recording_runs_anonymous_and_its_tape_holds_the_run() {
        let w = World::new("tapes-record");
        let state_before = Dirs::under(w.root.path()).state.exists();
        let p = w.tape("p");
        let ran = record_repo_view(&w, &p);

        let seen = w.fake.seen();
        assert!(!seen.is_empty());
        for s in &seen {
            assert!(!s.had_authorization, "{s:?}");
            assert_eq!(s.as_login, None, "{s:?}");
        }

        let text = std::fs::read_to_string(&p).expect("the tape");
        let (header, records) = read_tape(&p);
        assert_eq!(header.mode, TapeMode::Anonymous);
        assert_eq!(header.endpoints, TapeEndpoints::Loopback);
        assert_eq!(header.argv, ["repo", "view", APP]);
        for line in text.lines() {
            assert!(
                !line.to_ascii_lowercase().contains("authorization"),
                "{line}"
            );
        }
        assert_eq!(exchanges(&records, DOOR_GH).len(), seen.len());
        assert_eq!(emit(&records, DOOR_STDOUT), ran.stdout);
        assert_eq!(emit(&records, DOOR_EXIT), b"0");

        assert!(!Dirs::under(w.root.path()).rate().exists());
        assert_eq!(Dirs::under(w.root.path()).state.exists(), state_before);
        for file in files_under(w.root.path()) {
            assert!(
                !file.starts_with(Dirs::under(w.root.path()).state),
                "{}",
                file.display()
            );
        }
        w.fake.assert_clean();
    }

    #[test]
    fn a_replay_answers_the_recorded_run_with_and_without_its_verb() {
        let w = World::new("tapes-replay");
        let p = w.tape("p");
        let recorded = record_repo_view(&w, &p);
        w.fake.clear_seen();

        for args in [
            vec!["--replay", arg(&p)],
            vec!["--replay", arg(&p), "repo", "view", APP],
        ] {
            let ran = w.offline(&args);
            assert_eq!(ran.exit, recorded.exit, "{args:?}: {}", ran.stderr);
            assert_eq!(ran.stdout, recorded.stdout, "{args:?}");
        }
        assert!(w.fake.seen().is_empty());
        w.fake.assert_clean();
    }

    #[test]
    fn a_two_page_listing_records_and_replays_to_the_same_stdout() {
        let w = World::new("tapes-list");
        for i in 0..150u32 {
            let day = 1 + i % 28;
            let month = 1 + i / 28;
            w.fake.add_release(
                APP,
                ReleaseSpec::published(
                    &format!("v{i}"),
                    &format!("2026-{month:02}-{day:02}T00:00:00Z"),
                ),
            );
        }
        let p = w.tape("list");
        let recorded = w.live(&["--record", arg(&p), "release", "list", APP]);
        assert_eq!(recorded.exit, 0, "{}", recorded.stderr);
        let listed =
            aterm_json::from_slice::<aterm_json::Value>(&recorded.stdout).expect("a JSON listing");
        assert_eq!(listed.as_array().map(Vec::len), Some(150));

        let (_, records) = read_tape(&p);
        let gh = exchanges(&records, DOOR_GH);
        let answered = |status: &[u8]| {
            gh.iter()
                .filter(|r| {
                    matches!(r, EffectRecord::Exchange { request, reply, .. }
                        if request.windows(10).any(|w| w == b"/releases?") && reply.starts_with(status))
                })
                .count()
        };
        assert_eq!(answered(b"status 200\n"), 2, "the listing's page exchanges");
        assert_eq!(answered(b"status 304\n"), 2, "the listing's page exchanges");

        let ran = w.offline(&["--replay", arg(&p)]);
        assert_eq!(ran.exit, 0, "{}", ran.stderr);
        assert_eq!(ran.stdout, recorded.stdout);
        w.fake.assert_clean();
    }

    #[test]
    fn an_edited_request_path_is_a_request_divergence() {
        let w = World::new("tapes-diverge");
        let p = w.tape("p");
        record_repo_view(&w, &p);
        let text = std::fs::read_to_string(&p).expect("the tape");
        let from = format!("/repos/{APP}");
        let at = text.find(&from).expect("the first request's path");
        let edited = format!(
            "{}/repos/fixture-dev/apq{}",
            &text[..at],
            &text[at + from.len()..]
        );
        std::fs::write(&p, edited).expect("the edited tape");

        let ran = w.offline(&["--replay", arg(&p)]);
        assert_eq!(ran.exit, 3, "{}", ran.stderr);
        assert!(ran.stdout.is_empty(), "{:?}", ran.stdout);
        let error = error(&ran);
        assert_eq!(
            error["sub"],
            aterm_json::Value::from("replay.request"),
            "{error}"
        );
        let next = error["next"].as_str().unwrap_or_default();
        assert!(next.contains("--replay"), "{error}");
        assert!(next.contains("--record"), "{error}");
        w.fake.assert_clean();
    }

    #[test]
    fn a_replay_under_another_command_line_is_refused() {
        let w = World::new("tapes-argv");
        let p = w.tape("p");
        record_repo_view(&w, &p);
        let ran = w.offline(&["--replay", arg(&p), "repo", "view", "fixture-dev/other"]);
        assert_eq!(ran.exit, 2, "{}", ran.stderr);
        assert_eq!(
            error(&ran)["sub"],
            aterm_json::Value::from("replay.argv"),
            "{}",
            ran.stderr
        );
        w.fake.assert_clean();
    }

    #[test]
    fn a_rerecord_keeps_the_exchanges_and_the_exit_and_leaves_the_old_tape() {
        let w = World::new("tapes-rerecord");
        let p = w.tape("p");
        let recorded = record_repo_view(&w, &p);
        let before = std::fs::read(&p).expect("the tape");
        let q = w.tape("q");

        let ran = w.offline(&["--replay", arg(&p), "--record", arg(&q)]);
        assert_eq!(ran.exit, recorded.exit, "{}", ran.stderr);
        assert_eq!(ran.stdout, recorded.stdout);

        let (old_header, old) = read_tape(&p);
        let (new_header, new) = read_tape(&q);
        assert_eq!(new_header.argv, old_header.argv);
        assert_eq!(exchanges(&new, DOOR_GH), exchanges(&old, DOOR_GH));
        assert_eq!(emit(&new, DOOR_EXIT), emit(&old, DOOR_EXIT));
        assert_eq!(emit(&new, DOOR_STDOUT), emit(&old, DOOR_STDOUT));
        assert_eq!(std::fs::read(&p).expect("the old tape"), before);
        w.fake.assert_clean();
    }

    #[test]
    fn a_tape_replays_under_the_retries_it_was_recorded_with() {
        let w = World::new("tapes-retry");
        let p = w.tape("p");
        let recorded = record_repo_view(&w, &p);
        let (header, mut records) = read_tape(&p);
        let at = records
            .iter()
            .position(|r| matches!(r, EffectRecord::Exchange { door, .. } if door == DOOR_GH))
            .expect("a request");
        let request = match &records[at] {
            EffectRecord::Exchange { request, .. } => request.clone(),
            other => panic!("not an exchange: {other:?}"),
        };
        records.insert(
            at,
            EffectRecord::Exchange {
                door: DOOR_SLEEP.to_string(),
                request: b"2000000000".to_vec(),
                reply: Vec::new(),
            },
        );
        records.insert(
            at,
            EffectRecord::Exchange {
                door: DOOR_GH.to_string(),
                request,
                reply: canon::reply(502, &[], b""),
            },
        );
        let q = w.tape("retried");
        std::fs::write(&q, tape::render(&header, &records)).expect("the tape");

        let ran = w.offline(&["--replay", arg(&q)]);
        assert_eq!(ran.exit, recorded.exit, "{}", ran.stderr);
        assert_eq!(ran.stdout, recorded.stdout);
        w.fake.assert_clean();
    }

    #[test]
    fn arguments_that_read_as_a_shape_once_joined_are_never_taped() {
        let w = World::new("tapes-joined");
        let p = w.tape("p");
        let ran = w.live(&["--record", arg(&p), "repo", "view", "--jq", ".Bearer", APP]);
        assert_eq!(ran.exit, 2, "{}", ran.stderr);
        assert!(!p.exists());
        assert!(w.fake.seen().is_empty());
        let ran = w.live(&["--record", arg(&p), "repo", "view", "--jq", ".Bearex", APP]);
        assert_eq!(ran.exit, 0, "{}", ran.stderr);
        read_tape(&p);
        w.fake.assert_clean();
    }

    #[test]
    fn a_context_reused_after_a_tape_adds_nothing_to_it() {
        let w = World::new("tapes-reused");
        let p = w.tape("p");
        let (mut ctx, _captured) = common::ctx(w.root.path(), &w.fake, common::env());
        assert_eq!(
            run_cli(&mut ctx, &["--record", arg(&p), "repo", "view", APP]),
            0
        );
        let taped = std::fs::read(&p).expect("the tape");
        run_cli(&mut ctx, &["repo", "view", APP]);
        assert_eq!(std::fs::read(&p).expect("the tape"), taped);
        let ran = w.offline(&["--replay", arg(&p)]);
        assert_eq!(ran.exit, 0, "{}", ran.stderr);
        w.fake.assert_clean();
    }
}
