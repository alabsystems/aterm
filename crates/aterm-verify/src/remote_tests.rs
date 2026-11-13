// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;
use std::os::unix::process::ExitStatusExt as _;

const BEFORE: &str = "1111111111111111111111111111111111111111";
const AFTER: &str = "2222222222222222222222222222222222222222";
const ZERO: &str = "0000000000000000000000000000000000000000";

#[test]
fn github_commands_use_the_linked_host_instead_of_path_agh() {
    let _host = crate::remote_host::pin().unwrap();
    let root = aterm_tempfile::tempdir().unwrap();
    let git = |_: &Path| aterm_uds::spawnfd::command("git");
    let cmd = command(
        root.path(),
        &git,
        Tool::Agh,
        &words(&["repo", "refs", "a/b"]),
    )
    .unwrap();
    #[cfg(target_os = "linux")]
    assert_eq!(cmd.get_program(), "/proc/self/exe");
    #[cfg(target_os = "macos")]
    assert_eq!(cmd.get_program(), crate::remote_host::image().unwrap());
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    assert_eq!(cmd.get_program(), std::env::current_exe().unwrap());
    assert_eq!(
        cmd.get_args().collect::<Vec<_>>(),
        words(&[
            "--remote-agh",
            &std::process::id().to_string(),
            "repo",
            "refs",
            "a/b"
        ])
    );
}

fn out(code: i32, text: &str) -> Output {
    Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: text.as_bytes().to_vec(),
        stderr: if code == 0 {
            Vec::new()
        } else {
            b"recorded refusal".to_vec()
        },
    }
}

/// GitHub operations may run local URL discovery, never git's remote transport. This assertion
/// is shared by every scripted GitHub read/write below, including all failure arms.
fn url(tool: Tool, args: &[OsString]) -> Option<Output> {
    if tool != Tool::Git {
        return None;
    }
    if args.first().is_some_and(|a| a == "cat-file") {
        return None;
    }
    assert!(
        args == words(&["remote", "get-url", "--all", "origin"])
            || args == words(&["remote", "get-url", "--all", "--push", "origin"]),
        "raw GitHub git operation: {args:?}"
    );
    Some(out(0, "https://github.com/Fixture/Repo.git\n"))
}

fn input(args: &[OsString]) -> Value {
    let index = args
        .iter()
        .position(|word| word == "--input")
        .expect("structured input");
    aterm_json::from_slice(&std::fs::read(&args[index + 1]).expect("live input")).expect("JSON")
}

#[test]
fn github_spellings_share_a_route_and_unsupported_urls_never_become_git_fallbacks() {
    for source in [
        "https://github.com/A/R.git",
        "http://github.com/a/r/",
        "git@github.com:a/r",
        "ssh://git@github.com/a/r.git",
    ] {
        assert_eq!(kind(source), Ok(Kind::Github("a/r".into())));
    }
    for source in [
        "",
        "https://token@github.com/a/r",
        "https://github.com/a/r?token=secret",
        "https://github.com/a/r/extra",
        "https://github.com/../r",
        "https://github.com.evil/a/r",
        "ssh://git@elsewhere/a/r",
        "-option",
        "a\nb",
    ] {
        let error = kind(source).expect_err("ambiguous/unsupported remote");
        assert!(!error.contains(source) || source.is_empty());
        assert!(!error.contains("secret") && !error.contains("token@"));
    }
    for source in ["/tmp/origin.git", "../origin.git", "repo with spaces"] {
        assert_eq!(kind(source), Ok(Kind::Local(source.into())));
    }
}

#[test]
fn main_refresh_requires_a_successful_incremental_snapshot_and_one_commit() {
    let root = aterm_tempfile::tempdir().unwrap();
    for (symbolic, fetched, tip, object, expected, calls_expected) in [
        (1, 0, AFTER, "commit\n", true, 4),
        (1, 4, BEFORE, "commit\n", false, 2),
        (0, 0, AFTER, "commit\n", false, 1),
        (128, 0, AFTER, "commit\n", false, 1),
        (1, 0, "bad", "commit\n", false, 3),
        (1, 0, AFTER, "tag\n", false, 4),
    ] {
        let calls = Cell::new(0);
        let current = Cell::new(BEFORE);
        let run = |tool, args: &[OsString]| {
            if tool == Tool::Git && args.first().is_some_and(|arg| arg == "remote") {
                return Ok(url(tool, args).unwrap());
            }
            let call = calls.get();
            calls.set(call + 1);
            match call {
                0 => {
                    assert_eq!(tool, Tool::Git);
                    assert_eq!(
                        args,
                        words(&["symbolic-ref", "--quiet", "refs/agh/fetched-main"])
                    );
                    Ok(out(symbolic, ""))
                }
                1 => {
                    assert_eq!(tool, Tool::Agh);
                    let mut command = words(&["repo", "fetch-main", "fixture/repo"]);
                    command.push(root.path().as_os_str().to_owned());
                    assert_eq!(args, command);
                    if fetched == 0 {
                        current.set(tip);
                    }
                    Ok(out(fetched, ""))
                }
                2 => {
                    assert_eq!(tool, Tool::Git);
                    assert_eq!(
                        args,
                        words(&["rev-parse", "--verify", "refs/agh/fetched-main"])
                    );
                    Ok(out(0, &format!("{}\n", current.get())))
                }
                3 => {
                    assert_eq!(tool, Tool::Git);
                    assert_eq!(args, words(&["cat-file", "-t", tip]));
                    Ok(out(0, object))
                }
                _ => panic!("no retry or raw GitHub transport: {tool:?} {args:?}"),
            }
        };
        let value = Remote::open(root.path(), "origin", &run)
            .unwrap()
            .refresh_main();
        assert_eq!(value.is_ok(), expected);
        if expected {
            assert_eq!(value.unwrap(), AFTER);
        }
        assert_eq!(calls.get(), calls_expected);
    }
}

#[test]
fn github_fetch_keeps_the_observed_commit_when_main_or_the_journal_advances() {
    let root = aterm_tempfile::tempdir().expect("root");
    for reference in ["refs/heads/main", "refs/aterm/judged"] {
        let calls = Cell::new(0);
        let current = Cell::new(BEFORE);
        let imported = Cell::new(false);
        let run = |tool, args: &[OsString]| {
            if let Some(out) = url(tool, args) {
                return Ok(out);
            }
            let call = calls.get();
            calls.set(call + 1);
            if call == 0 {
                assert_eq!(
                    args,
                    words(&["repo", "refs", "fixture/repo", "--ref", reference])
                );
                // Prefix matches are not the exact ref, and must not determine its object.
                // The published ref advances after the API snapshot, before the import.
                current.set(AFTER);
                Ok(out(0, &json!([{"ref":format!("{reference}-other"),"oid":AFTER}, {"ref":reference,"oid":BEFORE}]).to_string()))
            } else if call == 1 {
                assert_eq!(&args[..3], words(&["repo", "fetch", "fixture/repo"]));
                assert_eq!(args[3], root.path().as_os_str());
                let batch = input(args);
                let source = batch[0]["source"].as_str().expect("source");
                let fetched = if source == reference {
                    current.get()
                } else {
                    source
                };
                // agh rejects a snapshot whose fetched OID differs from expected_oid.
                if Some(fetched) != batch[0]["expected_oid"].as_str() {
                    return Ok(out(9, ""));
                }
                assert_eq!(fetched, BEFORE, "import the observation, not the moved ref");
                let into = format!(
                    "refs/agh/verify/{}",
                    reference.strip_prefix("refs/").unwrap()
                );
                assert_eq!(batch[0]["into"].as_str(), Some(into.as_str()));
                imported.set(true);
                Ok(out(0, ""))
            } else {
                assert_eq!(call, 2);
                assert_eq!(tool, Tool::Git);
                assert_eq!(args, words(&["cat-file", "-t", BEFORE]));
                Ok(out(0, "commit\n"))
            }
        };
        let remote = Remote::open(root.path(), "origin", &run).unwrap();
        assert_eq!(remote.fetch(reference), Ok(Some(BEFORE.into())));
        assert_eq!(
            calls.get(),
            3,
            "two quota-free Git operations and one local type check; no REST"
        );
        assert!(imported.get());
    }
}

#[test]
fn only_a_successful_exact_ref_listing_can_report_absence() {
    let root = aterm_tempfile::tempdir().unwrap();
    for code in [0, 4] {
        let calls = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            if let Some(out) = url(tool, args) {
                return Ok(out);
            }
            calls.set(calls.get() + 1);
            assert_eq!(&args[..2], words(&["repo", "refs"]));
            Ok(out(
                code,
                &json!([{"ref":"refs/aterm/judged-other","oid":BEFORE}]).to_string(),
            ))
        };
        let remote = Remote::open(root.path(), "origin", &run).unwrap();
        let fetched = remote.fetch("refs/aterm/judged");
        if code == 0 {
            assert_eq!(fetched, Ok(None))
        } else {
            assert!(fetched.is_err())
        }
        assert_eq!(calls.get(), 1, "no import after absence or refusal");
    }
}

#[test]
fn malformed_or_duplicate_refs_are_never_imported() {
    let root = aterm_tempfile::tempdir().unwrap();
    for rows in [
        json!({"not":"an array"}),
        json!([{"ref":"refs/heads/main","oid":"bad"}]),
        json!([{"ref":"refs/heads/main","oid":BEFORE}, {"ref":"refs/heads/main","oid":AFTER}]),
    ] {
        let run = |tool, args: &[OsString]| {
            if let Some(out) = url(tool, args) {
                return Ok(out);
            }
            assert_eq!(
                &args[..2],
                words(&["repo", "refs"]),
                "never import invalid observations"
            );
            Ok(out(0, &rows.to_string()))
        };
        assert!(
            Remote::open(root.path(), "origin", &run)
                .unwrap()
                .fetch("refs/heads/main")
                .is_err()
        );
    }
}

#[test]
fn a_tag_or_blob_in_the_control_namespace_never_becomes_a_commit_observation() {
    let root = aterm_tempfile::tempdir().unwrap();
    for object_type in ["tag\n", "blob\n"] {
        let run = |tool, args: &[OsString]| {
            if let Some(out) = url(tool, args) {
                return Ok(out);
            }
            if tool == Tool::Git {
                return Ok(out(0, object_type));
            }
            if args[1] == "refs" {
                return Ok(out(
                    0,
                    &json!([{"ref":"refs/aterm/judged", "oid":BEFORE}]).to_string(),
                ));
            }
            Ok(out(0, ""))
        };
        assert!(
            Remote::open(root.path(), "origin", &run)
                .unwrap()
                .fetch("refs/aterm/judged")
                .is_err()
        );
    }
}

#[test]
fn github_push_dry_runs_the_same_exact_graph_and_lease_before_its_only_write() {
    let root = aterm_tempfile::tempdir().unwrap();
    for before in [Some(BEFORE), None] {
        let calls = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            if let Some(out) = url(tool, args) {
                return Ok(out);
            }
            let call = calls.get();
            calls.set(call + 1);
            assert_eq!(&args[..3], words(&["repo", "push", "fixture/repo"]));
            assert_eq!(args[3], root.path().as_os_str());
            assert_eq!(args.last().unwrap() == "--dry-run", call == 0);
            assert_eq!(
                input(args),
                json!([{"ref":"refs/aterm/judged","before":before.unwrap_or(ZERO),"after":AFTER}])
            );
            Ok(out(0, ""))
        };
        assert!(matches!(
            Remote::open(root.path(), "origin", &run).unwrap().push(
                "refs/aterm/judged",
                before,
                AFTER
            ),
            Ok(Pushed::Done)
        ));
        assert_eq!(calls.get(), 2);
    }
}

#[test]
fn a_rejected_dry_run_never_writes_and_an_unknown_write_never_replays() {
    let root = aterm_tempfile::tempdir().unwrap();
    for (dry_code, actual_code) in [(9, 0), (13, 0), (4, 0), (0, 9), (0, 13), (0, 4)] {
        let calls = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            if let Some(out) = url(tool, args) {
                return Ok(out);
            }
            let call = calls.get();
            calls.set(call + 1);
            assert!(call < 2, "no replay");
            Ok(out(if call == 0 { dry_code } else { actual_code }, ""))
        };
        let result = Remote::open(root.path(), "origin", &run).unwrap().push(
            "refs/heads/main",
            Some(BEFORE),
            AFTER,
        );
        if dry_code == 9 || (dry_code == 0 && actual_code == 9) {
            assert!(matches!(result, Ok(Pushed::Rejected(_))))
        } else {
            assert!(result.is_err())
        }
        assert_eq!(calls.get(), if dry_code == 0 { 2 } else { 1 });
    }
}

const INTENT: &str = "66-fc387e8b41a0";

fn intent_refusal() -> Value {
    agh::error::Error::new(
        agh::error::Kind::OutcomeUnknown,
        "another intent began first",
    )
    .with_sub(agh::error::Sub::IntentOpen {
        intent: INTENT.into(),
    })
    .with_repo("fixture/repo".to_owned())
    .to_json(&agh::redact::Redactor::new())
}

fn refused_out(value: &Value) -> Output {
    let mut result = out(13, "");
    result.stderr = format!("human diagnostic is not parsed\n{value}\n").into_bytes();
    result
}

#[test]
fn only_exact_structured_presend_intent_refusals_allow_observation() {
    let root = aterm_tempfile::tempdir().unwrap();
    let valid = intent_refusal();
    assert_eq!(
        blocking_intent(&refused_out(&valid), "fixture/repo"),
        agh::IntentId::parse(INTENT)
    );
    for (field, value) in [
        ("v", json!(2)),
        ("exit", json!(9)),
        ("kind", json!("refused")),
        ("sub", json!("unknown")),
        ("side_effect", json!("unknown")),
        ("repo", json!("other/repo")),
        ("intent", Value::Null),
        ("intent", json!("66-INVALID")),
    ] {
        let mut changed = valid.clone();
        let Value::Object(outer) = &mut changed else {
            unreachable!()
        };
        let Value::Object(error) = outer.get_mut("error").unwrap() else {
            unreachable!()
        };
        error.insert(field.into(), value);
        assert!(
            blocking_intent(&refused_out(&changed), "fixture/repo").is_none(),
            "{changed}"
        );
    }
    let mut duplicate = refused_out(&valid);
    duplicate.stderr = String::from_utf8(duplicate.stderr)
        .unwrap()
        .replace(
            "\"side_effect\":\"none\"",
            "\"side_effect\":\"unknown\",\"side_effect\":\"none\"",
        )
        .into_bytes();
    assert!(blocking_intent(&duplicate, "fixture/repo").is_none());
    for actual in [false, true] {
        let calls = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            if let Some(result) = url(tool, args) {
                return Ok(result);
            }
            let call = calls.get();
            calls.set(call + 1);
            assert_eq!(args.last().unwrap() == "--dry-run", call == 0);
            Ok(if actual && call == 0 {
                out(0, "")
            } else {
                refused_out(&valid)
            })
        };
        assert!(matches!(Remote::open(root.path(), "origin", &run).unwrap()
            .push("refs/heads/main", Some(BEFORE), AFTER),
            Ok(Pushed::Blocked { intent, .. }) if intent.as_str() == INTENT));
        assert_eq!(calls.get(), if actual { 2 } else { 1 });
    }
}

#[test]
fn observing_a_foreign_intent_never_writes_and_requires_its_exact_identity() {
    let root = aterm_tempfile::tempdir().unwrap();
    let document = json!({"id":INTENT,"repo":"fixture/repo","op":"ref.update-atomic",
        "key":"atomic refs","open":true});
    let reads = Cell::new(0);
    let pauses = Cell::new(0);
    let run = |tool, args: &[OsString]| {
        if let Some(result) = url(tool, args) {
            return Ok(result);
        }
        assert_eq!(tool, Tool::Agh);
        assert_eq!(
            args,
            words(&["intent", "show", "fixture/repo", INTENT, "--format", "json"])
        );
        let mut doc = document.clone();
        let Value::Object(fields) = &mut doc else {
            unreachable!()
        };
        fields.insert("open".into(), json!(reads.get() < 2));
        reads.set(reads.get() + 1);
        Ok(out(0, &doc.to_string()))
    };
    let intent = agh::IntentId::parse(INTENT).unwrap();
    Remote::open(root.path(), "origin", &run)
        .unwrap()
        .wait_for_intent(&intent, &|| {
            pauses.set(pauses.get() + 1);
            Ok(())
        })
        .unwrap();
    assert_eq!((reads.get(), pauses.get()), (3, 2));
    for (field, value) in [
        ("id", json!("67-000000000000")),
        ("repo", json!("other/repo")),
        ("op", json!("release.create")),
        ("key", json!("other")),
        ("open", Value::Null),
    ] {
        let mut doc = document.clone();
        let Value::Object(fields) = &mut doc else {
            unreachable!()
        };
        fields.insert(field.into(), value);
        let run = |tool, args: &[OsString]| {
            Ok(url(tool, args).unwrap_or_else(|| out(0, &doc.to_string())))
        };
        assert!(
            Remote::open(root.path(), "origin", &run)
                .unwrap()
                .wait_for_intent(&intent, &|| panic!("invalid identity must not wait"))
                .is_err()
        );
    }
    let run = |tool, args: &[OsString]| {
        Ok(url(tool, args).unwrap_or_else(|| out(0, &document.to_string())))
    };
    let ticks = Cell::new(0);
    let result = Remote::open(root.path(), "origin", &run)
        .unwrap()
        .wait_for_intent(&intent, &|| {
            ticks.set(ticks.get() + 1);
            if ticks.get() == 3 {
                Err("absolute deadline".into())
            } else {
                Ok(())
            }
        });
    assert_eq!(result, Err("absolute deadline".into()));
    assert_eq!(ticks.get(), 3);
}

#[test]
fn several_or_different_expanded_urls_stop_before_network_or_write() {
    let root = aterm_tempfile::tempdir().unwrap();
    for push in [
        "https://github.com/fixture/other\n",
        "https://github.com/fixture/repo\nhttps://github.com/fixture/other\n",
        "https://secret@github.com/fixture/repo\n",
    ] {
        let run = |tool, args: &[OsString]| {
            assert_eq!(tool, Tool::Git, "no network after ambiguous origin");
            Ok(out(
                0,
                if args.iter().any(|arg| arg == "--push") {
                    push
                } else {
                    "https://github.com/fixture/repo\n"
                },
            ))
        };
        let error = Remote::open(root.path(), "origin", &run)
            .err()
            .expect("refused origin");
        assert!(!error.contains("secret"));
    }
}

#[test]
fn local_transport_allows_only_the_file_protocol_even_if_configuration_changes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let operations = Cell::new(0);
    let run = |tool, args: &[OsString]| {
        assert_eq!(tool, Tool::Git);
        if args[0] == "remote" {
            return Ok(out(0, "/local/origin.git\n"));
        }
        assert_eq!(
            &args[..4],
            words(&[
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.file.allow=always"
            ])
        );
        operations.set(operations.get() + 1);
        let named = format!("{BEFORE}\trefs/heads/main\n");
        Ok(out(0, if args[4] == "ls-remote" { &named } else { "" }))
    };
    let remote = Remote::open(root.path(), "origin", &run).unwrap();
    assert_eq!(remote.fetch("refs/heads/main"), Ok(Some(BEFORE.into())));
    assert!(matches!(
        remote.push("refs/heads/main", Some(BEFORE), AFTER),
        Ok(Pushed::Done)
    ));
    assert_eq!(operations.get(), 3);
}

#[test]
fn a_github_object_import_is_one_agh_request_into_a_scratch_then_file_transport_only() {
    let root = aterm_tempfile::tempdir().expect("root");
    let agh_calls = Cell::new(0);
    let git_calls = std::cell::RefCell::new(Vec::new());
    let scratch = std::cell::RefCell::new(None::<OsString>);
    let run = |tool, args: &[OsString]| {
        if tool == Tool::Git && args[0] == "remote" {
            return Ok(url(tool, args).expect("URL discovery"));
        }
        if tool == Tool::Agh {
            agh_calls.set(agh_calls.get() + 1);
            assert_eq!(&args[..3], words(&["repo", "fetch", "fixture/repo"]));
            assert_ne!(
                args[3],
                root.path().as_os_str(),
                "agh never runs in the clone"
            );
            assert_eq!(scratch.borrow().as_ref(), Some(&args[3]));
            assert_eq!(args[4], "--objects");
            let ids: Value = aterm_json::from_slice(&std::fs::read(&args[5]).expect("live input"))
                .expect("JSON");
            assert_eq!(ids, json!([BEFORE, AFTER]));
            return Ok(out(0, ""));
        }
        if args[0] == "init" {
            *scratch.borrow_mut() = Some(args[3].clone());
        }
        git_calls.borrow_mut().push(args.to_vec());
        Ok(out(0, ""))
    };
    let remote = Remote::open(root.path(), "origin", &run).unwrap();
    // NEGATIVE CONTROL: nothing to import sends nothing, and a malformed ID is refused locally.
    assert_eq!(remote.import(&[]), Ok(()));
    assert!(remote.import(&["not-an-oid".into()]).is_err());
    assert_eq!(agh_calls.get(), 0);
    assert!(git_calls.borrow().is_empty());
    assert_eq!(remote.import(&[BEFORE.into(), AFTER.into()]), Ok(()));
    assert_eq!(agh_calls.get(), 1);
    let git_calls = git_calls.borrow();
    let scratch = scratch.borrow().clone().expect("a scratch receiver");
    assert_eq!(git_calls.len(), 3, "{git_calls:?}");
    // The only fetch is from the scratch path, over the file transport alone.
    let fetch = git_calls.last().unwrap();
    assert_eq!(
        &fetch[..4],
        words(&[
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always"
        ])
    );
    let at = fetch
        .iter()
        .position(|word| *word == scratch)
        .expect("from the scratch");
    assert_eq!(&fetch[at + 1..], words(&[BEFORE, AFTER]));
}

/// Real git, the shape `verify census` imports into: a blobless partial clone with history,
/// named by a linked worktree's path, lacking a blob an ancestor holds. agh refuses to run git
/// in such a clone (its promisor keys are outside agh's transport policy), so its half is
/// played here by what agh's object import leaves (the exact object in the scratch bare
/// repository, no ref); the scratch-to-clone hop runs for real.
#[test]
fn a_github_object_import_lands_in_a_real_partial_clone_through_its_worktree() {
    let temp = aterm_tempfile::tempdir().expect("root");
    let base = temp.path();
    let git = |dir: &Path, args: &[&str]| -> String {
        let mut cmd = crate::git_provider::command();
        cmd.current_dir(dir).args(args);
        for (key, _) in std::env::vars_os() {
            if key.as_encoded_bytes().starts_with(b"GIT_") {
                cmd.env_remove(key);
            }
        }
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.invalid")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.invalid");
        let out = aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60))
            .expect("git within its hang detector");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let work = base.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("version.rs"), "pub const V: u32 = 3;\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "baseline"]);
    let blob = git(&work, &["rev-parse", "HEAD:version.rs"]);
    std::fs::write(work.join("version.rs"), "pub const V: u32 = 4;\n").unwrap();
    git(&work, &["commit", "-qam", "head"]);
    let server = base.join("server.git");
    git(
        base,
        &["clone", "-q", "--bare", "work", server.to_str().unwrap()],
    );
    git(&server, &["config", "uploadpack.allowFilter", "true"]);
    git(
        &server,
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    );
    let clone = base.join("partial");
    git(
        base,
        &[
            "clone",
            "-q",
            "--no-checkout",
            "--filter=blob:none",
            &format!("file://{}", server.display()),
            clone.to_str().unwrap(),
        ],
    );
    // Point origin at GitHub, as the real checkout's is; nothing may use that URL.
    git(
        &clone,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/Fixture/Repo.git",
        ],
    );
    let tree = base.join("partial-wt");
    git(
        &clone,
        &[
            "worktree",
            "add",
            "-q",
            "--no-checkout",
            "--detach",
            tree.to_str().unwrap(),
        ],
    );
    let holds = || {
        let mut cmd = crate::git_provider::command();
        cmd.current_dir(&tree)
            .args(["cat-file", "-e", &blob])
            .env("GIT_NO_LAZY_FETCH", "1");
        aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60))
            .unwrap()
            .status
            .success()
    };
    // NEGATIVE CONTROL: the blobless clone really lacks the ancestor's blob.
    assert!(
        !holds(),
        "the fixture's partial clone already holds the blob"
    );
    let refs = git(&clone, &["for-each-ref"]);
    let make = |root: &Path| {
        let mut cmd = crate::git_provider::command();
        cmd.current_dir(root);
        cmd
    };
    let until = Instant::now() + std::time::Duration::from_secs(60);
    let run = |tool, args: &[OsString]| match tool {
        Tool::Git => super::run(&tree, &make, tool, args, until),
        Tool::Agh => {
            let ids: Vec<String> =
                aterm_json::from_slice(&std::fs::read(&args[5]).expect("live input"))
                    .expect("JSON");
            let scratch = Path::new(&args[3]);
            // agh's config screen: a receiver with promisor keys is refused, nothing sent.
            let listed = git(scratch, &["config", "--list", "--name-only"]).to_ascii_lowercase();
            if listed.contains("promisor") || listed.contains("partialclone") {
                return Ok(out(1, ""));
            }
            let mut fetch = vec!["fetch", "-q", "--no-tags", server.to_str().unwrap()];
            fetch.extend(ids.iter().map(String::as_str));
            git(scratch, &fetch);
            Ok(out(0, ""))
        }
    };
    let remote = Remote::open(&tree, "origin", &run).unwrap();
    assert_eq!(remote.import(std::slice::from_ref(&blob)), Ok(()));
    assert!(holds(), "the blob reached the clone's shared store");
    assert_eq!(
        git(&clone, &["for-each-ref"]),
        refs,
        "an object import publishes no ref"
    );
}

#[test]
fn push_source_admission_is_strict_for_github_and_absent_for_local_remotes() {
    let root = aterm_tempfile::tempdir().unwrap();
    for (code, answer, admitted) in [
        (0, "false\n", true),
        (0, "true\n", false),
        (0, "false", false),
        (0, "false\ntrue\n", false),
        (0, "", false),
        (128, "false\n", false),
    ] {
        let reads = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            assert_eq!(tool, Tool::Git);
            assert_eq!(args, words(&["rev-parse", "--is-shallow-repository"]));
            reads.set(reads.get() + 1);
            Ok(out(code, answer))
        };
        for github in [false, true] {
            reads.set(0);
            let remote = Remote {
                root: root.path(),
                name: "origin",
                kind: if github {
                    Kind::Github("fixture/repo".into())
                } else {
                    Kind::Local("../origin.git".into())
                },
                run: &run,
            };
            assert_eq!(remote.admit_push_source().is_ok(), !github || admitted);
            assert_eq!(reads.get(), usize::from(github));
        }
    }
}

/// A GITHUB COMMIT IMPORT (a gitlink's pin, which need be no ref's tip) is one exact agh fetch of
/// that object into the named ref, then a local type check; raw git never reaches the network.
/// NEGATIVE CONTROLS: a malformed ID asks nothing; a refused fetch is not type-checked; an object
/// that arrives as anything but a commit is refused.
#[test]
fn a_github_commit_import_is_one_exact_agh_fetch_then_a_type_check() {
    let root = aterm_tempfile::tempdir().expect("root");
    for (fetched, object, landed) in [
        (0, "commit\n", true),
        (9, "commit\n", false),
        (0, "blob\n", false),
    ] {
        let calls = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            if tool == Tool::Git && args[0] == "remote" {
                return Ok(url(tool, args).expect("URL discovery"));
            }
            let call = calls.get();
            calls.set(call + 1);
            if call == 0 {
                assert_eq!(tool, Tool::Agh);
                assert_eq!(&args[..3], words(&["repo", "fetch", "fixture/repo"]));
                assert_eq!(args[3], root.path().as_os_str());
                assert_eq!(
                    input(args),
                    json!([{"source":BEFORE,"into":"refs/agh/verify/gitlink","expected_oid":BEFORE}])
                );
                Ok(out(fetched, ""))
            } else {
                assert_eq!(call, 1, "nothing after the type check");
                assert_eq!(tool, Tool::Git);
                assert_eq!(args, words(&["cat-file", "-t", BEFORE]));
                Ok(out(0, object))
            }
        };
        let remote = Remote::open(root.path(), "origin", &run).unwrap();
        assert_eq!(
            remote
                .import_commit(BEFORE, "refs/agh/verify/gitlink")
                .is_ok(),
            landed
        );
        assert_eq!(calls.get(), if fetched == 0 { 2 } else { 1 });
        assert!(
            remote
                .import_commit("not-an-object", "refs/agh/verify/gitlink")
                .is_err()
        );
        assert_eq!(
            calls.get(),
            if fetched == 0 { 2 } else { 1 },
            "asks nothing"
        );
    }
}
