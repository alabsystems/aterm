// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;
use std::collections::BTreeSet;

// The write half (push, the intent watch, the commit import, push admission) builds where
// `verify land` does (remote.rs), so its tests and their diagnostics are unix-only.

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

/// The status of a process that exited with `code`.
fn exit_status(code: i32) -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        std::process::ExitStatus::from_raw(code << 8)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt as _;
        std::process::ExitStatus::from_raw(code.cast_unsigned())
    }
}

fn out(code: i32, text: &str) -> Output {
    Output {
        status: exit_status(code),
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
    file_input(args, "--input")
}

fn file_input(args: &[OsString], flag: &str) -> Value {
    let index = args
        .iter()
        .position(|word| word == flag)
        .expect("structured input");
    aterm_json::from_slice(&std::fs::read(&args[index + 1]).expect("live input")).expect("JSON")
}

const JOURNAL_TIMEOUT: &str = "injected journal transport timeout";
const PRIVATE_REMOTE: &str = "ls96-remote-private";
const PRIVATE_SLUG: &str = "ls96-owner-private/ls96-repo-private";
const PRIVATE_URL: &str = "https://github.com/ls96-owner-private/ls96-repo-private.git";
const PRIVATE_REF: &str = "refs/aterm/ls96-ref-private";

fn assert_journal_timeout(error: &str, step: ReadStep) {
    assert_eq!(error, format!("{}: {JOURNAL_TIMEOUT}", step.context()));
    for private in [
        PRIVATE_REMOTE,
        "ls96-owner-private",
        "ls96-repo-private",
        "ls96-local-url-private",
        "ls96-ref-private",
        "ls96-argv-private",
        BEFORE,
    ] {
        assert!(
            !error.contains(private),
            "private input entered the context"
        );
    }
}

#[test]
fn journal_timeout_names_url_discovery_and_preserves_failed_output_redaction() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut timeouts = BTreeSet::new();
    let mut failed_outputs = BTreeSet::new();
    for (failed_call, step) in [(0, ReadStep::FetchUrl), (1, ReadStep::PushUrl)] {
        for nonzero in [false, true] {
            let calls = Cell::new(0);
            let run = |tool, args: &[OsString]| {
                let call = calls.get();
                calls.set(call + 1);
                assert_eq!(tool, Tool::Git);
                assert_eq!(
                    args,
                    if call == 0 {
                        words(&["remote", "get-url", "--all", PRIVATE_REMOTE])
                    } else {
                        assert_eq!(call, 1, "nothing after URL discovery fails");
                        words(&["remote", "get-url", "--all", "--push", PRIVATE_REMOTE])
                    }
                );
                if call == failed_call {
                    if nonzero {
                        let mut failed = out(128, PRIVATE_URL);
                        failed.stderr =
                            format!("{PRIVATE_REMOTE}: {PRIVATE_URL} {PRIVATE_REF}").into_bytes();
                        Ok(failed)
                    } else {
                        Err(JOURNAL_TIMEOUT.to_owned())
                    }
                } else {
                    Ok(out(0, &format!("{PRIVATE_URL}\n")))
                }
            };
            let error = Remote::open(root.path(), PRIVATE_REMOTE, &run)
                .err()
                .expect("URL discovery fails");
            if nonzero {
                assert!(error.starts_with(step.context()));
                assert!(error.contains("cannot read"));
                for private in [
                    PRIVATE_REMOTE,
                    PRIVATE_URL,
                    PRIVATE_REF,
                    "ls96-owner-private",
                    "ls96-repo-private",
                ] {
                    assert!(!error.contains(private));
                }
                failed_outputs.insert(error);
            } else {
                assert_journal_timeout(&error, step);
                timeouts.insert(error);
            }
            assert_eq!(
                calls.get(),
                failed_call + 1,
                "no later discovery or transport"
            );
        }
    }
    assert_eq!(
        timeouts.len(),
        2,
        "URL stages need distinct timeout diagnostics"
    );
    assert_eq!(
        failed_outputs.len(),
        2,
        "failed URL output keeps its read stage"
    );
}

#[test]
fn journal_timeout_names_each_github_fetch_stage_and_stops() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("ls96-argv-private");
    std::fs::create_dir(&root).unwrap();
    let mut diagnostics = BTreeSet::new();
    for (failed_call, step) in [
        (0, ReadStep::GithubRef),
        (1, ReadStep::GithubSnapshot),
        (2, ReadStep::CommitInspection),
    ] {
        let calls = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            let call = calls.get();
            calls.set(call + 1);
            let reply = match call {
                0 => {
                    assert_eq!(tool, Tool::Agh);
                    assert_eq!(
                        args,
                        words(&["repo", "refs", PRIVATE_SLUG, "--ref", PRIVATE_REF])
                    );
                    json!([{"ref":PRIVATE_REF,"oid":BEFORE}]).to_string()
                }
                1 => {
                    assert_eq!(tool, Tool::Agh);
                    assert_eq!(&args[..3], words(&["repo", "fetch", PRIVATE_SLUG]));
                    assert_eq!(args[3], root.as_os_str());
                    assert_eq!(file_input(args, "--objects"), json!([BEFORE]));
                    String::new()
                }
                2 => {
                    assert_eq!(tool, Tool::Git);
                    assert_eq!(args, words(&["cat-file", "-t", BEFORE]));
                    "commit\n".to_owned()
                }
                _ => panic!("nothing after the failed journal read"),
            };
            if call == failed_call {
                Err(JOURNAL_TIMEOUT.to_owned())
            } else {
                Ok(out(0, &reply))
            }
        };
        let remote = Remote {
            root: &root,
            name: PRIVATE_REMOTE,
            kind: Kind::Github(PRIVATE_SLUG.to_owned()),
            run: &run,
        };
        let error = remote.fetch(PRIVATE_REF).unwrap_err();
        assert_journal_timeout(&error, step);
        diagnostics.insert(error);
        assert_eq!(
            calls.get(),
            failed_call + 1,
            "no import, inspection or retry after failure"
        );
    }
    assert_eq!(
        diagnostics.len(),
        3,
        "each failed stage has its own diagnostic"
    );
}

#[test]
fn journal_timeout_names_each_local_fetch_stage_and_stops() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut diagnostics = BTreeSet::new();
    for (failed_call, step) in [(0, ReadStep::LocalRef), (1, ReadStep::LocalSnapshot)] {
        let calls = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            let call = calls.get();
            calls.set(call + 1);
            assert_eq!(tool, Tool::Git);
            let reply = match call {
                0 => {
                    assert_eq!(
                        args,
                        local_words(&["ls-remote", PRIVATE_REMOTE, PRIVATE_REF])
                    );
                    format!("{BEFORE}\t{PRIVATE_REF}\n")
                }
                1 => {
                    assert_eq!(
                        args,
                        local_words(&["fetch", "--quiet", "--no-tags", PRIVATE_REMOTE, BEFORE])
                    );
                    String::new()
                }
                _ => panic!("nothing after the failed local journal read"),
            };
            if call == failed_call {
                Err(JOURNAL_TIMEOUT.to_owned())
            } else {
                Ok(out(0, &reply))
            }
        };
        let remote = Remote {
            root: root.path(),
            name: PRIVATE_REMOTE,
            kind: Kind::Local("../ls96-local-url-private/repo.git".to_owned()),
            run: &run,
        };
        let error = remote.fetch(PRIVATE_REF).unwrap_err();
        assert_journal_timeout(&error, step);
        diagnostics.insert(error);
        assert_eq!(
            calls.get(),
            failed_call + 1,
            "no import or retry after failure"
        );
    }
    assert_eq!(
        diagnostics.len(),
        2,
        "each failed stage has its own diagnostic"
    );
}

#[cfg(unix)]
#[test]
fn journal_timeout_names_local_commit_import_and_inspection() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut diagnostics = BTreeSet::new();
    for (failed_call, step) in [
        (0, ReadStep::LocalSnapshot),
        (1, ReadStep::CommitInspection),
    ] {
        let calls = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            let call = calls.get();
            calls.set(call + 1);
            assert_eq!(tool, Tool::Git);
            let reply = match call {
                0 => {
                    let into = format!("+{BEFORE}:{PRIVATE_REF}");
                    assert_eq!(
                        args,
                        local_words(&[
                            "fetch",
                            "--quiet",
                            "--no-tags",
                            "--no-write-fetch-head",
                            "--recurse-submodules=no",
                            PRIVATE_REMOTE,
                            &into,
                        ])
                    );
                    ""
                }
                1 => {
                    assert_eq!(args, words(&["cat-file", "-t", BEFORE]));
                    "commit\n"
                }
                _ => panic!("nothing after the failed commit import"),
            };
            if call == failed_call {
                Err(JOURNAL_TIMEOUT.to_owned())
            } else {
                Ok(out(0, reply))
            }
        };
        let remote = Remote {
            root: root.path(),
            name: PRIVATE_REMOTE,
            kind: Kind::Local("../ls96-local-url-private/repo.git".to_owned()),
            run: &run,
        };
        let error = remote.import_commit(BEFORE, PRIVATE_REF).unwrap_err();
        assert_journal_timeout(&error, step);
        diagnostics.insert(error);
        assert_eq!(
            calls.get(),
            failed_call + 1,
            "no inspection or retry after failure"
        );
    }
    assert_eq!(
        diagnostics.len(),
        2,
        "each failed stage has its own diagnostic"
    );
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
    // A drive's colon is a local path's on Windows alone; elsewhere git reads `C:\…` as the
    // ssh host `C`. A colon past the drive, and a drive-relative path, are refused everywhere.
    for source in [r"C:\Users\me\origin.git", "c:/Users//me/origin.git"] {
        if cfg!(windows) {
            assert_eq!(kind(source), Ok(Kind::Local(source.into())));
        } else {
            assert!(kind(source).is_err(), "{source}");
        }
    }
    for source in [
        "C:origin.git",
        r"C:\Users\me\origin.git:stream",
        "host:origin.git",
        r"\\server\share:origin.git",
    ] {
        assert!(kind(source).is_err(), "{source}");
    }
}

/// What `agh repo fetch-main` prints: the ref and the commit its fetch wrote there.
fn fetched_main(oid: &str) -> String {
    json!({"ref": "refs/agh/fetched-main", "oid": oid}).to_string()
}

#[test]
fn main_refresh_requires_a_successful_incremental_snapshot_and_one_commit() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut fetch = words(&["repo", "fetch-main", "fixture/repo"]);
    fetch.push(root.path().as_os_str().to_owned());
    for (fetched, printed, object, expected, calls_expected) in [
        (0, fetched_main(AFTER), "commit\n", true, 2),
        (4, fetched_main(AFTER), "commit\n", false, 1),
        (0, String::new(), "commit\n", false, 1),
        (0, fetched_main("bad"), "commit\n", false, 1),
        (
            0,
            json!({"ref": "refs/agh/other", "oid": AFTER}).to_string(),
            "commit\n",
            false,
            1,
        ),
        (
            0,
            json!({"ref": "refs/agh/fetched-main"}).to_string(),
            "commit\n",
            false,
            1,
        ),
        (0, fetched_main(AFTER), "tag\n", false, 2),
    ] {
        let calls = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            if tool == Tool::Git && args.first().is_some_and(|arg| arg == "remote") {
                return Ok(url(tool, args).unwrap());
            }
            let call = calls.get();
            calls.set(call + 1);
            match call {
                0 => {
                    assert_eq!(tool, Tool::Agh);
                    assert_eq!(args, fetch);
                    Ok(out(fetched, &printed))
                }
                1 => {
                    assert_eq!(tool, Tool::Git);
                    assert_eq!(args, words(&["cat-file", "-t", AFTER]));
                    Ok(out(0, object))
                }
                _ => panic!("no retry, ref read or raw GitHub transport: {tool:?} {args:?}"),
            }
        };
        let value = Remote::open(root.path(), "origin", &run)
            .unwrap()
            .refresh_main();
        assert_eq!(value.is_ok(), expected, "{printed}: {value:?}");
        if expected {
            assert_eq!(value.unwrap(), AFTER);
        }
        assert_eq!(calls.get(), calls_expected, "{printed}");
    }
}

/// Every worktree of a clone shares `refs/agh/fetched-main`, and agh's last step writes it
/// unconditionally, so a sibling worktree's slower fetch, which read main before it moved, can
/// put the older main back between this fetch's write and any read of the ref. The refresh is
/// this fetch's own commit, as agh printed it, whatever the shared ref says by then. NEGATIVE
/// CONTROL: the shared ref, read back as the refresh once did, names the sibling's older main.
#[test]
fn main_refresh_is_its_own_fetch_even_when_a_sibling_rewrites_the_shared_ref() {
    let root = aterm_tempfile::tempdir().unwrap();
    let shared = std::cell::RefCell::new(String::from(ZERO));
    let run = |tool, args: &[OsString]| {
        if tool == Tool::Git && args.first().is_some_and(|arg| arg == "remote") {
            return Ok(url(tool, args).unwrap());
        }
        let args: Vec<_> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
        Ok(match (tool, args.as_slice()) {
            (Tool::Agh, ["repo", "fetch-main", "fixture/repo", _]) => {
                // This fetch writes main's new tip; a sibling's, which read main at BEFORE,
                // then finishes and writes the older tip over it.
                *shared.borrow_mut() = AFTER.into();
                *shared.borrow_mut() = BEFORE.into();
                out(0, &fetched_main(AFTER))
            }
            // The shared ref answers as git would, for a refresh that read it back.
            (Tool::Git, ["symbolic-ref", "--quiet", "refs/agh/fetched-main"]) => out(1, ""),
            (Tool::Git, ["rev-parse", "--verify", "refs/agh/fetched-main"]) => {
                out(0, &format!("{}\n", shared.borrow()))
            }
            (Tool::Git, ["cat-file", "-t", _]) => out(0, "commit\n"),
            other => panic!("unexpected remote step: {other:?}"),
        })
    };
    let remote = Remote::open(root.path(), "origin", &run).unwrap();
    assert_eq!(
        remote.refresh_main().as_deref(),
        Ok(AFTER),
        "the refresh is this fetch's commit, not the shared ref's"
    );
    assert_eq!(
        *shared.borrow(),
        BEFORE,
        "the sibling's older main is in the ref"
    );
}

#[test]
fn github_fetch_keeps_the_observed_commit_when_remote_and_sibling_cache_advance() {
    let root = aterm_tempfile::tempdir().expect("root");
    for reference in ["refs/heads/main", "refs/aterm/record"] {
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
                // A sibling changed the shared destination after agh read its old value.
                // Ref publication must refuse that CAS; importing objects has no ref to race.
                if args.iter().any(|arg| arg == "--input") {
                    let batch = input(args);
                    assert_eq!(batch[0]["expected_oid"].as_str(), Some(BEFORE));
                    return Ok(out(9, "shared cache ref changed"));
                }
                let objects = file_input(args, "--objects");
                assert_eq!(
                    objects,
                    json!([BEFORE]),
                    "import the observation, not the moved ref"
                );
                assert_eq!(current.get(), AFTER, "the remote really advanced");
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
                &json!([{"ref":"refs/aterm/record-other","oid":BEFORE}]).to_string(),
            ))
        };
        let remote = Remote::open(root.path(), "origin", &run).unwrap();
        let fetched = remote.fetch("refs/aterm/record");
        if code == 0 {
            assert_eq!(fetched, Ok(None))
        } else {
            let error = fetched.unwrap_err();
            assert!(error.starts_with(ReadStep::GithubRef.context()));
            assert!(error.contains("recorded refusal"));
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
                    &json!([{"ref":"refs/aterm/record", "oid":BEFORE}]).to_string(),
                ));
            }
            Ok(out(0, ""))
        };
        assert!(
            Remote::open(root.path(), "origin", &run)
                .unwrap()
                .fetch("refs/aterm/record")
                .is_err()
        );
    }
}

/// A GITHUB PUSH IS ONE WRITE, MADE THROUGH THE WRITER: agh's write makes every check its dry
/// run makes before it sends, and each agh push re-verifies the pushed history while the
/// expected ref is free to move, so no rehearsal comes first. The push goes through the writer
/// it is handed ([`run_mutation`] in production), never the remote's reader, whose deadline
/// would end it, and names no `--timeout`, so agh bounds each git child of its write by its 60 s
/// no-progress window and ends a stalled one itself instead of hanging, settling its intent
/// Unknown once the pack may have gone, unless a re-read finds the push landed, which stays open
/// until it is reconciled. NEGATIVE CONTROL: the reader is handed no push.
#[cfg(unix)]
#[test]
fn github_push_is_one_write_of_the_exact_graph_and_lease() {
    let root = aterm_tempfile::tempdir().unwrap();
    for before in [Some(BEFORE), None] {
        let calls = Cell::new(0);
        let read = |tool, args: &[OsString]| {
            Ok(url(tool, args).unwrap_or_else(|| panic!("the reader ran {tool:?} {args:?}")))
        };
        let write = |tool, args: &[OsString]| {
            assert_eq!(tool, Tool::Agh);
            calls.set(calls.get() + 1);
            let index = args
                .iter()
                .position(|word| word == "--input")
                .expect("structured input");
            let mut asked = words(&["repo", "push", "fixture/repo"]);
            asked.extend([
                root.path().as_os_str().to_owned(),
                "--input".into(),
                args[index + 1].clone(),
            ]);
            assert_eq!(args, asked);
            assert_eq!(
                input(args),
                json!([{"ref":"refs/aterm/record","before":before.unwrap_or(ZERO),"after":AFTER}])
            );
            Ok(out(0, ""))
        };
        assert!(matches!(
            Remote::open(root.path(), "origin", &read)
                .unwrap()
                .push_with(WriteRunner(&write), "refs/aterm/record", before, AFTER),
            Ok(Pushed::Done)
        ));
        assert_eq!(calls.get(), 1);
    }
}

/// The writer that runs each write as `run` answers it: every write it is handed starts, so a
/// failure of `run` is one of unknown outcome ([`Failed::Ended`]).
#[cfg(unix)]
fn writer<'a>(run: &'a Runner<'a>) -> impl Fn(Tool, &[OsString]) -> Result<Output, Failed> + 'a {
    move |tool, args| run(tool, args).map_err(Failed::Ended)
}

/// A PUSH WHOSE WRITE STARTED NO CHILD SENT NOTHING: a writer's refusal before it started
/// ([`Failed::Unstarted`], as [`run_mutation`] answers once the run took an interrupt) is a push
/// that sent nothing ([`Pushed::Unsent`]) carrying the refusal's words, over a local remote and
/// over GitHub, never a failure of unknown outcome, which is never made again. NEGATIVE CONTROL:
/// a writer's failure after its child started ([`Failed::Ended`]) stays an error.
#[cfg(unix)]
#[test]
fn a_push_whose_write_started_no_child_sent_nothing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let refused = "Git remote write: interrupted before the write started; nothing was sent";
    for github in [false, true] {
        for failed in [
            Failed::Unstarted(refused.to_owned()),
            Failed::Ended("Git remote write stalled".to_owned()),
        ] {
            let read = |tool, args: &[OsString]| {
                if github {
                    return Ok(url(tool, args).expect("the reader reads the remote's URLs"));
                }
                Ok(out(0, "../origin.git\n"))
            };
            let writes = Cell::new(0);
            let write = |_: Tool, _: &[OsString]| {
                writes.set(writes.get() + 1);
                Err(failed.clone())
            };
            let pushed = Remote::open(root.path(), "origin", &read)
                .unwrap()
                .push_with(WriteRunner(&write), "refs/heads/main", Some(BEFORE), AFTER);
            match (&failed, pushed) {
                (Failed::Unstarted(_), Ok(Pushed::Unsent(why))) => assert_eq!(why, refused),
                (Failed::Ended(said), Err(why)) => assert_eq!(&why, said),
                (failed, pushed) => {
                    panic!("github {github}: {failed:?} came to {:?}", pushed.err())
                }
            }
            assert_eq!(writes.get(), 1, "one write was handed over");
        }
    }
}

/// A GITHUB PUSH WHOSE INPUT COULD NOT BE WRITTEN SENT NOTHING: where the directory for agh's
/// input file cannot be made, or the input cannot be written in it (its name taken by a
/// directory), the writer is never handed the push and no child started, so the push is one that
/// sent nothing ([`Pushed::Unsent`], saying so), made again as one, never an error of unknown
/// outcome. NEGATIVE CONTROL: a directory the input can be written in hands the writer its one
/// push.
#[cfg(unix)]
#[test]
fn a_github_push_whose_input_could_not_be_written_sent_nothing() {
    type Scratch<'a> = &'a dyn Fn() -> std::io::Result<aterm_tempfile::TempDir>;
    let root = aterm_tempfile::tempdir().unwrap();
    let read =
        |tool, args: &[OsString]| Ok(url(tool, args).expect("the reader reads the remote's URLs"));
    let remote = Remote::open(root.path(), "origin", &read).unwrap();
    let writes = Cell::new(0);
    let write = |tool, _: &[OsString]| {
        assert_eq!(tool, Tool::Agh);
        writes.set(writes.get() + 1);
        Ok(out(0, ""))
    };
    let unmade =
        || -> std::io::Result<aterm_tempfile::TempDir> { Err(std::io::Error::other("no room")) };
    let taken = || -> std::io::Result<aterm_tempfile::TempDir> {
        let dir = aterm_tempfile::tempdir()?;
        std::fs::create_dir(dir.path().join("push.json"))?;
        Ok(dir)
    };
    for (scratch, what) in [(&unmade as Scratch<'_>, "unmade"), (&taken, "taken")] {
        match remote.push_from(
            WriteRunner(&write),
            scratch,
            "refs/heads/main",
            Some(BEFORE),
            AFTER,
        ) {
            Ok(Pushed::Unsent(why)) => assert!(
                why.starts_with("Agh remote write: its input could not be written: ")
                    && why.ends_with("; nothing was sent"),
                "{what}: {why}"
            ),
            pushed => panic!("{what}: the push came to {:?}", pushed.err()),
        }
        assert_eq!(writes.get(), 0, "{what}: the writer was handed nothing");
    }
    assert!(matches!(
        remote.push_from(
            WriteRunner(&write),
            &aterm_tempfile::tempdir,
            "refs/heads/main",
            Some(BEFORE),
            AFTER
        ),
        Ok(Pushed::Done)
    ));
    assert_eq!(writes.get(), 1, "the writer was handed the push");
}

/// A write agh refused for the expected ref is a rejection; any other failure with no typed
/// diagnostic (an unknown outcome, a could-not-run or an authentication failure that names no
/// side effect) is an error, and the write is never made a second time here.
#[cfg(unix)]
#[test]
fn a_rejected_write_is_typed_and_an_untyped_failure_never_replays() {
    let root = aterm_tempfile::tempdir().unwrap();
    for code in [9, 13, 4, 3] {
        let calls = Cell::new(0);
        let run = |tool, args: &[OsString]| {
            if let Some(out) = url(tool, args) {
                return Ok(out);
            }
            calls.set(calls.get() + 1);
            Ok(out(code, ""))
        };
        let result = Remote::open(root.path(), "origin", &run)
            .unwrap()
            .push_with(
                WriteRunner(&writer(&run)),
                "refs/heads/main",
                Some(BEFORE),
                AFTER,
            );
        if code == 9 {
            assert!(matches!(result, Ok(Pushed::Rejected(_))))
        } else {
            assert!(result.is_err(), "exit {code}")
        }
        assert_eq!(calls.get(), 1, "no replay");
    }
}

/// agh's own diagnostic of `error`, as its failed command prints it on stderr.
#[cfg(unix)]
fn diagnosed(error: &agh::error::Error) -> Output {
    let mut result = out(i32::from(error.exit()), "");
    result.stderr = format!(
        "human diagnostic is not parsed\n{}\n",
        error.to_json(&agh::redact::Redactor::new())
    )
    .into_bytes();
    result
}

/// What a GitHub push of main over [`BEFORE`] comes to where agh's write answers `answer`.
#[cfg(unix)]
fn pushed_when_agh_answers(root: &Path, answer: Output) -> Result<Pushed, String> {
    let run = move |tool, args: &[OsString]| {
        if let Some(result) = url(tool, args) {
            return Ok(result);
        }
        Ok(Output {
            status: answer.status,
            stdout: answer.stdout.clone(),
            stderr: answer.stderr.clone(),
        })
    };
    Remote::open(root, "origin", &run).unwrap().push_with(
        WriteRunner(&writer(&run)),
        "refs/heads/main",
        Some(BEFORE),
        AFTER,
    )
}

/// ONLY AGH'S OWN WORD THAT NOTHING WAS SENT LETS A WRITE BE MADE AGAIN: a could-not-run, rate
/// or server failure, or a stop agh took before it dispatched git (interrupted, which agh settles
/// never sent), whose one diagnostic names no side effect is [`Pushed::Unsent`]. NEGATIVE
/// CONTROLS: the same failure with an unknown or applied side effect, for another repository,
/// with a second diagnostic, or with an exit its diagnostic does not name, stays an error, and a
/// refusal of any other kind is never made again.
#[cfg(unix)]
#[test]
fn only_agh_saying_it_sent_nothing_lets_the_write_be_made_again() {
    use agh::error::{Error, Kind, SideEffect};
    let root = aterm_tempfile::tempdir().unwrap();
    let pushed = |answer: Output| pushed_when_agh_answers(root.path(), answer);
    for kind in [
        Kind::CouldNotRun,
        Kind::RateLimited,
        Kind::Server,
        Kind::Interrupted,
    ] {
        let error = Error::new(kind, "the network went away before the send");
        assert!(
            matches!(pushed(diagnosed(&error)), Ok(Pushed::Unsent(_))),
            "{kind:?}"
        );
        let ours = error.clone().with_repo("fixture/repo".to_owned());
        assert!(matches!(pushed(diagnosed(&ours)), Ok(Pushed::Unsent(_))));
        for effect in [SideEffect::Unknown, SideEffect::Applied] {
            let sent = error.clone().with_side_effect(effect);
            assert!(pushed(diagnosed(&sent)).is_err(), "{kind:?} {effect:?}");
        }
        let theirs = error.clone().with_repo("other/repo".to_owned());
        assert!(pushed(diagnosed(&theirs)).is_err(), "{kind:?} elsewhere");
        let mut twice = diagnosed(&error);
        let again = twice.stderr.clone();
        twice.stderr.extend(again);
        assert!(pushed(twice).is_err(), "{kind:?} with two diagnostics");
        let mut other_exit = diagnosed(&error);
        other_exit.status = exit_status(4);
        assert!(pushed(other_exit).is_err(), "{kind:?} under another exit");
    }
    for kind in [
        Kind::Refused,
        Kind::Usage,
        Kind::Unauthenticated,
        Kind::Forbidden,
        Kind::Invalid,
        Kind::OutcomeUnknown,
        Kind::Failed,
    ] {
        let error = Error::new(kind, "not a failure another attempt cures");
        assert!(
            !matches!(pushed(diagnosed(&error)), Ok(Pushed::Unsent(_))),
            "{kind:?}"
        );
    }
}

/// A TYPED REFUSAL WITH NO EFFECT IS A REFUSAL, NEVER A PUSH OF UNKNOWN OUTCOME: agh's one
/// diagnostic, for this repository or naming none, with no side effect and of a kind another
/// attempt does not cure (an authentication failure, exit 4, as a revoked token's 401 at agh's
/// login binding gives before any intent is opened; a permission, gate, usage, input or subject
/// refusal; GitHub's rejection of the push it received, a branch rule's `[remote rejected]` or
/// the workflow scope, which agh words as a refusal or a forbidden write that wrote nothing), is
/// [`Pushed::Refused`] carrying agh's words, which leaves nothing to settle.
/// NEGATIVE CONTROLS: the same refusal with an unknown or applied side effect, for another
/// repository, with a second diagnostic, or under an exit its diagnostic does not name stays an
/// error of unknown outcome, as does an unknown outcome that names no side effect; an
/// expected-ref conflict stays a rejection; and a stop agh took before it dispatched git (a
/// signal sent to agh itself, or its watcher's lost bell: interrupted, exit 130, no side effect)
/// is a push that sent nothing, never a refusal.
#[cfg(unix)]
#[test]
fn a_typed_refusal_with_no_effect_is_refused_never_unknown() {
    use agh::error::{Error, Kind, SideEffect};
    let root = aterm_tempfile::tempdir().unwrap();
    let pushed = |answer: Output| pushed_when_agh_answers(root.path(), answer);
    for kind in [
        Kind::Unauthenticated,
        Kind::Forbidden,
        Kind::Refused,
        Kind::Usage,
        Kind::Invalid,
        Kind::Failed,
    ] {
        let error = Error::new(kind, "GitHub answered 401 for the routed writer's login");
        for refused in [
            error.clone(),
            error.clone().with_repo("fixture/repo".to_owned()),
        ] {
            match pushed(diagnosed(&refused)) {
                Ok(Pushed::Refused(why)) => assert!(
                    why.contains(&format!("exited Some({})", kind.exit()))
                        && why.contains("GitHub answered 401"),
                    "{kind:?}: {why}"
                ),
                Ok(_) => panic!("{kind:?}: not a refusal"),
                Err(why) => panic!("{kind:?}: of unknown outcome: {why}"),
            }
        }
        for effect in [SideEffect::Unknown, SideEffect::Applied] {
            let sent = error.clone().with_side_effect(effect);
            assert!(pushed(diagnosed(&sent)).is_err(), "{kind:?} {effect:?}");
        }
        let theirs = error.clone().with_repo("other/repo".to_owned());
        assert!(pushed(diagnosed(&theirs)).is_err(), "{kind:?} elsewhere");
        let mut twice = diagnosed(&error);
        let again = twice.stderr.clone();
        twice.stderr.extend(again);
        assert!(pushed(twice).is_err(), "{kind:?} with two diagnostics");
        let mut other_exit = diagnosed(&error);
        other_exit.status = exit_status(3);
        assert!(pushed(other_exit).is_err(), "{kind:?} under another exit");
    }
    let unknown = Error::new(
        Kind::OutcomeUnknown,
        "a write was sent and no answer was read",
    );
    assert!(pushed(diagnosed(&unknown)).is_err());
    let conflict = Error::new(Kind::Conflict, "the expected ref moved");
    assert!(matches!(
        pushed(diagnosed(&conflict)),
        Ok(Pushed::Rejected(_))
    ));
    for stopped in [
        Error::new(Kind::Interrupted, "native Git operation interrupted"),
        Error::new(Kind::Interrupted, "native Git operation interrupted")
            .with_repo("fixture/repo".to_owned()),
    ] {
        match pushed(diagnosed(&stopped)) {
            Ok(Pushed::Unsent(why)) => assert!(why.contains("exited Some(130)"), "{why}"),
            Ok(Pushed::Refused(why)) => panic!("a stop before dispatch is no refusal: {why}"),
            Ok(_) => panic!("a stop before dispatch sent nothing"),
            Err(why) => panic!("a stop before dispatch is of no unknown outcome: {why}"),
        }
    }
    for (rejected, words) in [
        (
            Error::refused(
                "GitHub refused the push: !\trefs/heads/main:refs/heads/main\t[remote rejected] \
                 (push declined due to repository rule violations)",
            ),
            "[remote rejected] (push declined due to repository rule violations)",
        ),
        (
            Error::new(
                Kind::Forbidden,
                "git push needs the workflow scope: refusing to allow an OAuth App to create or \
                 update workflow",
            ),
            "needs the workflow scope",
        ),
    ] {
        let rejected = rejected.with_repo("fixture/repo".to_owned());
        match pushed(diagnosed(&rejected)) {
            Ok(Pushed::Refused(why)) => assert!(why.contains(words), "{why}"),
            Ok(_) => panic!("GitHub's rejection is a refusal: {words}"),
            Err(why) => panic!("GitHub's rejection is of no unknown outcome: {why}"),
        }
    }
}

#[cfg(unix)]
const INTENT: &str = "66-fc387e8b41a0";

#[cfg(unix)]
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

#[cfg(unix)]
fn refused_out(value: &Value) -> Output {
    let mut result = out(13, "");
    result.stderr = format!("human diagnostic is not parsed\n{value}\n").into_bytes();
    result
}

#[cfg(unix)]
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
    let calls = Cell::new(0);
    let run = |tool, args: &[OsString]| {
        if let Some(result) = url(tool, args) {
            return Ok(result);
        }
        calls.set(calls.get() + 1);
        Ok(refused_out(&valid))
    };
    assert!(matches!(Remote::open(root.path(), "origin", &run).unwrap()
        .push_with(WriteRunner(&writer(&run)), "refs/heads/main", Some(BEFORE), AFTER),
        Ok(Pushed::Blocked { intent, .. }) if intent.as_str() == INTENT));
    assert_eq!(calls.get(), 1);
}

#[cfg(unix)]
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
    let ended = Remote::open(root.path(), "origin", &run)
        .unwrap()
        .wait_for_intent(&intent, &|| {
            pauses.set(pauses.get() + 1);
            Ok(())
        });
    assert_eq!(ended, Ok(IntentWait::Closed));
    assert_eq!((reads.get(), pauses.get()), (3, 2));
    // Open with its writer gone (`closable`): the wait ends at once, and says that it ended
    // there, not on the intent closed, so the caller knows the next push's reconciliation
    // answers for it.
    let mut gone = document.clone();
    let Value::Object(fields) = &mut gone else {
        unreachable!()
    };
    fields.insert("closable".into(), json!(true));
    let run =
        |tool, args: &[OsString]| Ok(url(tool, args).unwrap_or_else(|| out(0, &gone.to_string())));
    assert_eq!(
        Remote::open(root.path(), "origin", &run)
            .unwrap()
            .wait_for_intent(&intent, &|| panic!("a closable intent is not waited on")),
        Ok(IntentWait::Reconcilable)
    );
    // Open, recording no writer (`"writer": null`) and settled with an unknown outcome: no wait
    // or later write closes it, so the wait ends at once on it held. CONTROLS, each waited on:
    // the same intent not yet settled (its agh may still be writing), one an agh that prints no
    // `writer` showed, and one that records its writer.
    for (writer, settled, held) in [
        (Some(Value::Null), json!("unknown"), true),
        (Some(Value::Null), Value::Null, false),
        (None, json!("unknown"), false),
        (Some(json!({"boot": "fixture"})), json!("unknown"), false),
    ] {
        let mut doc = document.clone();
        let Value::Object(fields) = &mut doc else {
            unreachable!()
        };
        if let Some(writer) = writer {
            fields.insert("writer".into(), writer);
        }
        fields.insert("settled".into(), settled);
        let run = |tool, args: &[OsString]| {
            Ok(url(tool, args).unwrap_or_else(|| out(0, &doc.to_string())))
        };
        let ticks = Cell::new(0);
        let ended = Remote::open(root.path(), "origin", &run)
            .unwrap()
            .wait_for_intent(&intent, &|| {
                ticks.set(ticks.get() + 1);
                Err("still open".into())
            });
        if held {
            assert_eq!((ended, ticks.get()), (Ok(IntentWait::Held), 0), "{doc}");
        } else {
            assert_eq!((ended, ticks.get()), (Err("still open".into()), 1), "{doc}");
        }
    }
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

#[cfg(unix)]
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
        remote.push_with(
            WriteRunner(&writer(&run)),
            "refs/heads/main",
            Some(BEFORE),
            AFTER
        ),
        Ok(Pushed::Done)
    ));
    assert_eq!(operations.get(), 3);
}

#[cfg(unix)]
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
#[cfg(unix)]
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
        let result = remote.import_commit(BEFORE, "refs/agh/verify/gitlink");
        assert_eq!(result.is_ok(), landed);
        if fetched != 0 {
            let error = result.unwrap_err();
            assert!(error.starts_with(ReadStep::GithubSnapshot.context()));
            assert!(error.contains("recorded refusal"));
        }
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
