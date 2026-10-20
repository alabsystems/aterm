// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A worker's frame announces 0 to 64 KiB; a length past that, or none at
/// all, is refused unread (the connection then closes), naming the cap.
#[test]
fn a_task_frame_is_at_most_64_kib() {
    assert_eq!(frame_len("0"), Ok(0));
    assert_eq!(frame_len("65536"), Ok(TASK_MAX));
    assert_eq!(
        frame_len("65537"),
        Err("ERR spawn agent=claude: the task is 65537 bytes (max 65536)\n".to_string())
    );
    assert!(frame_len("99999999999999999999999").is_err_and(|e| e.contains("bytes (max 65536)")));
    for bad in ["", "-1", "1k", "0x10"] {
        assert!(
            frame_len(bad).is_err_and(|e| e.starts_with("ERR usage: spawn")),
            "{bad:?}"
        );
    }
}

/// The order a worker's spawn makes of its frame: no frame, an empty task
/// and a NUL byte are refused by name; a task with every other byte is kept
/// as given, and the cwd named is the canonical trusted folder.
#[test]
fn a_task_is_refused_empty_absent_or_holding_a_nul() {
    let refused = |frame: Option<&[u8]>| {
        TaskOrder::of_frame(frame.map(<[u8]>::to_vec), None, None, None).unwrap_err()
    };
    assert!(refused(None).contains("the task follows the line as a frame"));
    assert_eq!(
        refused(Some(b"")),
        "ERR spawn agent=claude: the task is empty\n"
    );
    assert!(refused(Some(b"a\0b")).contains("NUL byte"));
    let task = "-x\n'q' \"q\" $(rm -rf /) `id`\n\u{1b}[31m"
        .as_bytes()
        .to_vec();
    let from = SessionId::new("s-ab12");
    // A drive's root on Windows (`C:\`), which `canonicalize` answers
    // verbatim (`\\?\C:\`): trusted as a person spells it.
    let root = std::env::temp_dir();
    let root = root.ancestors().last().expect("a root").to_string_lossy();
    let order = TaskOrder::of_frame(
        Some(task.clone()),
        Some("opus".into()),
        Some(&root),
        Some(from.clone()),
    )
    .expect("an order");
    assert_eq!(order.task, task);
    assert_eq!(order.model.as_deref(), Some("opus"));
    assert_eq!(order.caller, Some(from));
    assert_eq!(order.trusted_cwd.as_deref(), Some(&*root));
    assert_eq!(cfg!(windows), root.ends_with(r":\"), "{root}");
    let unnamed = TaskOrder::of_frame(Some(b"t".to_vec()), None, Some("/no/such/dir"), None)
        .expect("an order");
    assert_eq!(
        unnamed.trusted_cwd, None,
        "a folder that is not there trusts nothing"
    );
}

/// The task file is the task, its last line ended, a blank line, then the
/// one line of fact: it names the starter without requiring an output marker.
#[test]
fn the_task_file_is_the_task_a_blank_line_and_the_fact() {
    let fact = fact_line("s-ab12");
    assert!(!fact.contains('\n'), "one line: {fact}");
    assert!(fact.contains("`s-ab12`"), "names the starter: {fact}");
    assert!(
        !fact.contains(aterm_agent::supervise::policy::turn_end::FINISHED),
        "requires no output marker: {fact}"
    );
    assert_eq!(
        task_text(b"do it", Some("ctl")),
        format!("do it\n\n{}\n", fact_line("ctl")).into_bytes()
    );
    assert_eq!(
        task_text(b"two\nlines\n", Some("ctl")),
        format!("two\nlines\n\n{}\n", fact_line("ctl")).into_bytes()
    );
    // With no starter to name, the task alone, its last line ended.
    assert_eq!(task_text(b"do it", None), b"do it\n");
}

/// The task file is written 0600 in a 0700 directory, under the sid that
/// names it, and never over a file already there; the seed that wrote it
/// removes it when its worker did not start, and only it.
#[cfg(unix)]
#[test]
fn the_task_file_is_private_and_never_overwritten() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-task-file")
        .tempdir()
        .expect("scratch dir");
    let dir = scratch.path().join("tasks");
    let sid = SessionId::new("s-0123abcd");
    let seed = TaskSeed::new(b"the task\n".to_vec());
    let path = seed.write_in(&dir, &sid).expect("written");
    assert_eq!(path, dir.join("s-0123abcd-1.txt"));
    assert_eq!(std::fs::read(&path).unwrap(), b"the task\n");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&dir), mode(&path)), (0o700, 0o600));
    assert!(
        TaskSeed::new(b"other".to_vec())
            .write_in(&dir, &sid)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"the task\n", "kept");
    TaskSeed::new(b"unwritten".to_vec()).discard();
    assert!(path.exists(), "a seed that wrote nothing removes nothing");
    seed.discard();
    assert!(!path.exists(), "discarded");
    seed.discard();

    // Cancellation may answer the main thread before the spawner writes.
    let cancelled = TaskSeed::new(b"never started".to_vec());
    let late = cancelled.clone();
    cancelled.discard();
    let sid = SessionId::new("s-0001");
    assert_eq!(
        late.write_in(&dir, &sid).unwrap_err().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(!dir.join(task_file_name(&sid)).exists());

    // Or discard can win after the actual exclusive write but before its
    // path is published. Interpose it deterministically on the real writer;
    // no new thread, wait or program is involved.
    let racing = TaskSeed::new(b"cancelled during write".to_vec());
    let cancel = racing.clone();
    let sid = SessionId::new("s-0002");
    let path = dir.join(task_file_name(&sid));
    let outcome = racing.write_in_using(&dir, &sid, |pinned, name, text| {
        let file = pinned.write_new_private(name, text)?;
        assert_eq!(std::fs::read(&path).unwrap(), b"cancelled during write");
        cancel.discard();
        Ok(file)
    });
    assert_eq!(outcome.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
    assert!(!path.exists(), "the late owned file was retired");
    assert!(racing.write_in(&dir, &sid).is_err(), "discard stays sticky");

    // Dropping the last seed is not cancellation: a successfully spawned
    // shell may not have reached its first prompt/read of the task file yet.
    let running = TaskSeed::new(b"still needed".to_vec());
    let path = running.write_in(&dir, &SessionId::new("s-0003")).unwrap();
    drop(running);
    assert_eq!(std::fs::read(&path).unwrap(), b"still needed");
}

/// The frame a request carried is the dispatch's alone: taken once, and
/// gone when the guard drops.
#[test]
fn a_carried_task_is_taken_once_and_cleared_with_its_request() {
    {
        let _carried = CarriedTask::enter(Some(b"t".to_vec()));
        assert_eq!(take_frame(), Some(b"t".to_vec()));
        assert_eq!(take_frame(), None, "taken once");
    }
    {
        let _carried = CarriedTask::enter(Some(b"u".to_vec()));
    }
    assert_eq!(
        take_frame(),
        None,
        "cleared with the request that carried it"
    );
}

/// A worker's shell is zsh, bash or fish, by its program's name; another is
/// refused by name. fish is asked its version, and only 3.4 or later takes
/// the operand.
#[test]
fn a_worker_shell_is_zsh_bash_or_a_fish_that_takes_dollar_paren() {
    assert_eq!(worker_dialect(Path::new("/bin/zsh")), Ok(Dialect::Zsh));
    assert_eq!(worker_dialect(Path::new("bash")), Ok(Dialect::Bash));
    assert_eq!(
        worker_dialect(Path::new("/opt/homebrew/bin/fish")),
        Ok(Dialect::Fish)
    );
    assert_eq!(worker_dialect(Path::new("/bin/sh")), Err("sh".to_string()));
    for (said, takes) in [
        ("fish, version 3.4.0\n", Some(true)),
        ("fish, version 3.7.1", Some(true)),
        ("fish, version 4.0b1", Some(true)),
        ("fish, version 3.3.1", Some(false)),
        ("fish, version 2.7.1", Some(false)),
        ("", None),
        ("fish", None),
    ] {
        assert_eq!(fish_version_takes_dollar_paren(said), takes, "{said:?}");
    }
}

/// A worker's line carries its starter's Claude home, `HOME` and login only
/// when the worker runs under the starter's own identity (none for both is the
/// human's own agent config); under another one — `identity=<other>`,
/// `identity=-` beneath a starter's, a named one beneath the human's — it
/// carries the starter's program alone, so the identity's own env stands.
/// The same identity is the negative control: there the home, `HOME` and
/// login do ride, so the other cases' absence is the identity's doing.
#[test]
fn a_worker_under_another_identity_carries_its_starters_program_alone() {
    use aterm_agent::harness::relaunch::CallerClaude;
    let starter = || CallerClaude {
        exe: PathBuf::from("/opt/twin/claude"),
        home: Some("/homes/a/.claude".to_string()),
        homedir: Some("/homes/a".to_string()),
        login: Some(String::new()),
    };
    for (worker, from) in [(Some("a"), Some("a")), (None, None)] {
        assert!(carries_starters_login(worker, from), "{worker:?} {from:?}");
        assert_eq!(
            line_claude(Some(starter()), carries_starters_login(worker, from)),
            starter(),
            "{worker:?} under {from:?}"
        );
    }
    for (worker, from) in [(Some("b"), Some("a")), (None, Some("a")), (Some("b"), None)] {
        assert!(!carries_starters_login(worker, from), "{worker:?} {from:?}");
        assert_eq!(
            line_claude(Some(starter()), carries_starters_login(worker, from)),
            CallerClaude {
                exe: PathBuf::from("/opt/twin/claude"),
                home: None,
                homedir: None,
                login: None,
            },
            "{worker:?} under {from:?}"
        );
    }
    assert_eq!(
        line_claude(None, true),
        CallerClaude {
            exe: PathBuf::from("claude"),
            home: None,
            homedir: None,
            login: None,
        },
        "a starter that runs no Claude Code: plain claude"
    );
}

/// A STARTER THE SWEEP HAS NOT NAMED YET is read from its terminal: a
/// manager that spawns within half a second of its composer is asked before
/// the status sweep reads its screen (the workers lab's three silent stalls,
/// 2026-10-18), and its line then carried plain `claude`. The group in front
/// of the starter's terminal is followed instead (and `caller_claude` reads
/// whether it runs a Claude Code); a starter the sweep named as another
/// agent's program is not followed, and is not read. NEGATIVE CONTROL: the
/// old rule (no reader, no group) is the `None` the first case refuses.
#[test]
fn a_starter_not_named_yet_is_read_from_its_terminal() {
    use aterm_phase::Program;
    let unread = || -> Option<i32> { panic!("the front group is not read") };
    assert_eq!(
        starter_group(Some(Program::Claude), 41, unread),
        Some(StarterGroup::Named(41)),
        "named Claude Code: the sweep's group, nothing read"
    );
    assert_eq!(
        starter_group(None, 0, || Some(77)),
        Some(StarterGroup::Front(77)),
        "not named yet: the terminal's front group"
    );
    assert_eq!(starter_group(None, 0, || None), None, "no front group");
    assert_eq!(starter_group(None, 0, || Some(0)), None, "no group is none");
    for other in [Program::Codex, Program::Generic, Program::Gemini] {
        assert_eq!(
            starter_group(Some(other), 41, unread),
            None,
            "{other:?}: another agent's program is never followed"
        );
    }
}

/// A WORKER THAT NEVER STARTED SAYS WHAT BECAME OF ITS LINE: the startup
/// stall's log line names what the launcher noted on the worker's timeline —
/// typed, with the receipt's word and the program, or never typed and why —
/// and says so where this process's launcher noted nothing. NEGATIVE
/// CONTROL: before any note, the words are the never-typed ones.
#[test]
fn a_stalled_worker_says_whether_its_line_was_typed() {
    let session = crate::stub_session(1);
    let ctx = &session.ctx;
    assert!(
        crate::harness_host::launch_words(ctx).contains("never typed by this process"),
        "no note: never typed here"
    );
    note_launch(
        ctx,
        "typed (291 B, written whole): /opt/claude (the starter's Claude Code)".to_string(),
    );
    let words = crate::harness_host::launch_words(ctx);
    assert!(
        words.starts_with("; its launch line was typed (291 B, written whole): /opt/claude"),
        "{words}"
    );
    assert!(words.ends_with(" s before"), "{words}");
}

/// Write an empty file `name` in `dir`.
#[cfg(unix)]
fn touch(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, "").expect("written");
    path
}

/// A closed worker's task file and every result its dones snapshotted
/// (`<sid>-<rev>.result.md` and `.result.src`) are removed, and only its
/// own: another session's stay, a sid that merely starts with this one's
/// included, and so does a file that is no result of it. None there, or no
/// directory at all, is no error.
#[cfg(unix)]
#[test]
fn a_closed_workers_task_files_are_removed_and_no_other() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-task-retire")
        .tempdir()
        .expect("scratch dir");
    let dir = scratch.path().join("tasks");
    let (gone, kept) = (SessionId::new("s-0a"), SessionId::new("s-0b"));
    let path = TaskSeed::new(b"one".to_vec())
        .write_in(&dir, &gone)
        .expect("written");
    let other = TaskSeed::new(b"two".to_vec())
        .write_in(&dir, &kept)
        .expect("written");
    let results: Vec<PathBuf> = [
        "s-0a-2.result.md",
        "s-0a-2.result.src",
        "s-0a-17.result.md",
        "s-0a-17.result.src",
    ]
    .iter()
    .map(|n| touch(&dir, n))
    .collect();
    let others: Vec<PathBuf> = [
        "s-0b-2.result.md",
        "s-0b-2.result.src",
        "s-0ab-2.result.md",
        "s-0a-x.result.md",
        "s-0a-.result.md",
        "s-0a-2.result.txt",
        "s-0a-2.md",
    ]
    .iter()
    .map(|n| touch(&dir, n))
    .collect();
    retire_task_file_in(&dir, &gone);
    assert!(!path.exists(), "removed");
    for r in &results {
        assert!(!r.exists(), "{} removed", r.display());
    }
    assert!(other.exists(), "another session's stays");
    for o in &others {
        assert!(o.exists(), "{} stays", o.display());
    }
    retire_task_file_in(&dir, &gone);
    retire_task_file_in(&scratch.path().join("absent"), &gone);
    assert!(other.exists());
}

/// A QUIT removes the task file of every worker it still holds
/// ([`retire_task_files_at_quit`]): the final exit path closes no session one
/// by one, so no close would. NEGATIVE CONTROLS: a session with no task
/// record keeps a file under its name, a file no session here names (a
/// sibling instance's worker sharing the state folder) stays, and so does
/// the file of a worker the parked predecessor still owns (an update
/// candidate quitting before its Commit) — removed once it is this
/// process's.
#[cfg(unix)]
#[test]
fn a_quit_removes_its_workers_task_files_and_no_other() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-task-quit")
        .tempdir()
        .expect("scratch dir");
    let dir = scratch.path().join("tasks");
    let store = crate::session_store::new_store();
    let subscribers = crate::subscribe::new_registry();
    let (worker, shell) = (
        crate::session_store::test_handle(1),
        crate::session_store::test_handle(2),
    );
    store.write().unwrap().register(worker.clone());
    store.write().unwrap().register(shell.clone());
    crate::task_record::mark_session_task(
        &worker.ctx,
        1,
        &TaskMark::new(TaskState::Working),
        &subscribers,
    )
    .expect("unfenced");
    let write = |sid: &SessionId| {
        TaskSeed::new(b"t".to_vec())
            .write_in(&dir, sid)
            .expect("written")
    };
    let tasked = write(&worker.sid);
    let untasked = write(&shell.sid);
    let elsewhere = write(&SessionId::new("s-0c"));
    let result = touch(&dir, &format!("{}-2.result.md", worker.sid.as_str()));
    let untasked_result = touch(&dir, &format!("{}-2.result.md", shell.sid.as_str()));
    // The parked predecessor's worker: kept.
    retire_task_files_in(&dir, &store, std::slice::from_ref(&worker.sid));
    assert!(tasked.exists(), "the predecessor's worker keeps its file");
    assert!(result.exists(), "and its result");
    retire_task_files_in(&dir, &store, &[]);
    assert!(
        !tasked.exists(),
        "the worker's task file goes with the quit"
    );
    assert!(!result.exists(), "and its result");
    assert!(untasked.exists(), "a session with no task record keeps it");
    assert!(untasked_result.exists());
    assert!(elsewhere.exists(), "a file no session here names stays");
}
