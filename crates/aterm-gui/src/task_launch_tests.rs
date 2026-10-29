// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_phase::Program;

#[path = "account_worker_env_tests.rs"]
mod account_worker_env;

#[test]
fn a_task_frame_is_at_most_64_kib() {
    assert_eq!(frame_len("0", "claude"), Ok(0));
    assert_eq!(frame_len("65536", "claude"), Ok(TASK_MAX));
    assert_eq!(
        frame_len("65537", "claude"),
        Err("ERR spawn agent=claude: the task is 65537 bytes (max 65536)\n".to_string())
    );
    assert_eq!(
        frame_len("65537", worker_word(Some("muse"))),
        Err("ERR spawn agent=muse: the task is 65537 bytes (max 65536)\n".to_string())
    );
    assert!(
        frame_len("99999999999999999999999", "claude")
            .is_err_and(|e| e.contains("bytes (max 65536)"))
    );
    for bad in ["", "-1", "1k", "0x10"] {
        assert!(
            frame_len(bad, "claude").is_err_and(|e| e.starts_with("ERR usage: spawn")),
            "{bad:?}"
        );
    }
    for word in ["claude", "codex", "agy", "muse", "grok"] {
        assert_eq!(worker_word(Some(word)), word);
        assert_eq!(retired_worker(Some(word)), None, "{word}: no retired agent");
    }
    for retired in ["gemini", "Gemini", "gemini-cli", "GEMINI-CLI"] {
        assert_eq!(
            retired_worker(Some(retired)).as_deref(),
            Some(
                format!(
                    "ERR spawn agent={retired}: Gemini CLI is no longer supported: agy replaces \
                     it (agent=agy)\n"
                )
                .as_str()
            ),
            "the retired Gemini CLI is refused by name, agy named"
        );
    }
    for none in [Some("geminis"), Some("gemini-cli-2"), Some(""), None] {
        assert_eq!(retired_worker(none), None, "CONTROL: {none:?}");
    }
    for none in [Some("codex-old"), Some("bash"), Some(""), None] {
        assert_eq!(worker_word(none), "claude", "CONTROL: {none:?}");
    }
}

#[test]
fn a_task_is_refused_empty_absent_or_holding_a_nul() {
    let refused = |frame: Option<&[u8]>| {
        TaskOrder::of_frame(
            frame.map(<[u8]>::to_vec),
            aterm_phase::Program::Claude,
            None,
            None,
            None,
        )
        .unwrap_err()
    };
    assert!(refused(None).contains("the task follows the line as a frame"));
    assert_eq!(
        refused(Some(b"")),
        "ERR spawn agent=claude: the task is empty\n"
    );
    assert!(refused(Some(b"a\0b")).contains("NUL byte"));
    assert_eq!(
        TaskOrder::of_frame(
            Some(Vec::new()),
            aterm_phase::Program::Muse,
            None,
            None,
            None
        )
        .unwrap_err(),
        "ERR spawn agent=muse: the task is empty\n",
        "named by the spawn's own agent"
    );
    let task = "-x\n'q' \"q\" $(rm -rf /) `id`\n\u{1b}[31m"
        .as_bytes()
        .to_vec();
    let from = SessionId::new("s-ab12");
    let root = std::env::temp_dir();
    let root = root.ancestors().last().expect("a root").to_string_lossy();
    let order = TaskOrder::of_frame(
        Some(task.clone()),
        aterm_phase::Program::Muse,
        Some("opus".into()),
        Some(&root),
        Some(from.clone()),
    )
    .expect("an order");
    assert_eq!(order.task, task);
    assert_eq!(order.agent, aterm_phase::Program::Muse);
    assert_eq!(order.model.as_deref(), Some("opus"));
    assert_eq!(order.caller, Some(from));
    assert_eq!(order.trusted_cwd.as_deref(), Some(&*root));
    assert_eq!(cfg!(windows), root.ends_with(r":\"), "{root}");
    let unnamed = TaskOrder::of_frame(
        Some(b"t".to_vec()),
        aterm_phase::Program::Claude,
        None,
        Some("/no/such/dir"),
        None,
    )
    .expect("an order");
    assert_eq!(
        unnamed.trusted_cwd, None,
        "a folder that is not there trusts nothing"
    );
}

#[test]
fn the_task_file_is_the_task_a_blank_line_and_the_fact() {
    let fact = fact_line("s-ab12", Autopilot::Land, "claude");
    assert!(!fact.contains('\n'), "one line: {fact}");
    assert!(fact.contains("`s-ab12`"), "names the starter: {fact}");
    assert!(
        fact.contains("Autopilot Land"),
        "names its Autopilot: {fact}"
    );
    assert!(
        fact.contains("`Nothing left:`"),
        "Land ends on the defense: {fact}"
    );
    assert!(fact.contains("`NEEDS YOU:`"), "a need goes on: {fact}");
    assert!(fact.contains("`spawn agent=claude`"), "{fact}");
    let muse = fact_line("s-ab12", Autopilot::Land, "muse");
    assert!(muse.contains("`spawn agent=muse`"), "{muse}");
    assert!(!muse.contains("agent=claude"), "CONTROL: {muse}");
    let cruise = fact_line("s-ab12", Autopilot::Cruise, "claude");
    assert!(cruise.contains("Autopilot Cruise"), "{cruise}");
    assert!(
        !cruise.contains("Nothing left:"),
        "CONTROL: Cruise never ends: {cruise}"
    );
    for fact in [&fact, &cruise] {
        assert!(
            !fact.contains(aterm_agent::supervise::policy::turn_end::FINISHED),
            "requires no output marker: {fact}"
        );
    }
    assert_eq!(
        task_text(b"do it", Some("ctl"), Autopilot::Land, Program::Muse),
        format!("do it\n\n{}\n", fact_line("ctl", Autopilot::Land, "muse")).into_bytes()
    );
    assert_eq!(
        task_text(
            b"two\nlines\n",
            Some("ctl"),
            Autopilot::Cruise,
            Program::Claude
        ),
        format!(
            "two\nlines\n\n{}\n",
            fact_line("ctl", Autopilot::Cruise, "claude")
        )
        .into_bytes()
    );
    assert_eq!(
        task_text(b"do it", None, Autopilot::Land, Program::Claude),
        b"do it\n"
    );
}

#[test]
fn a_line_that_draws_as_a_blank_row_is_dropped_from_an_agy_task() {
    use aterm_phase::agy::fixtures::lab::WORKER_FIRST_PROMPT;
    use aterm_phase::agy::said_tail;
    let task = "First paragraph.\n\u{a0}\n\u{3000}\u{2003}\t\n\u{200b}\n\u{feff}\u{7}\n\
                \u{a0}keep me\nSecond paragraph.\n";
    let text = |agent| {
        String::from_utf8(task_text(task.as_bytes(), None, Autopilot::Land, agent)).expect("utf-8")
    };
    assert_eq!(
        text(Program::Agy),
        "First paragraph.\n\u{a0}keep me\nSecond paragraph.\n"
    );
    for agent in [Program::Claude, Program::Codex, Program::Muse] {
        assert_eq!(
            text(agent),
            task,
            "CONTROL: {agent:?} keeps the task as given"
        );
    }
    let rows = aterm_phase::prompt::fixtures::screen(WORKER_FIRST_PROMPT);
    let act = rows
        .iter()
        .position(|r| r.starts_with("> [aterm harness]"))
        .expect("the Land act");
    let caret = rows.iter().rposition(|r| r == ">").expect("the composer");
    let mut first = rows[..act - 1].to_vec();
    first.extend(rows[caret - 1..].iter().cloned());
    let user = first
        .iter()
        .position(|r| r.starts_with("> FOREVERLAB-WORKER"))
        .expect("the first prompt");
    assert!(first[user + 1].is_empty(), "the measured blank row");
    first[user + 1] = "  \u{a0}".to_string();
    assert!(
        said_tail(&first).is_some_and(|s| s.starts_with("(Started by `s-146719f2750f4a8f6909`")),
        "{:?}",
        said_tail(&first)
    );
}

#[test]
fn an_agy_workers_first_prompt_holds_no_blank_line() {
    use aterm_agent::supervise::policy::needs::needs_you;
    use aterm_phase::agy::fixtures::lab::WORKER_FIRST_PROMPT;
    use aterm_phase::agy::{said_tail, turn_opened_by};
    let text = |task: &[u8], from, agent| {
        String::from_utf8(task_text(task, from, Autopilot::Land, agent)).expect("utf-8")
    };
    let task = "First paragraph.\n\n  \nSecond paragraph,\nits second line.\n\n";
    let folded = "First paragraph.\nSecond paragraph,\nits second line.\n";
    assert_eq!(
        text(task.as_bytes(), Some("s-ab12"), Program::Agy),
        format!("{folded}{}\n", fact_line("s-ab12", Autopilot::Land, "agy"))
    );
    assert_eq!(text(task.as_bytes(), None, Program::Agy), folded);
    for agent in [Program::Claude, Program::Codex, Program::Muse] {
        let fact = fact_line("s-ab12", Autopilot::Land, agent.name());
        assert_eq!(
            text(task.as_bytes(), Some("s-ab12"), agent),
            format!("{task}\n{fact}\n"),
            "CONTROL: {agent:?} keeps the task's blank lines"
        );
    }

    let rows = aterm_phase::prompt::fixtures::screen(WORKER_FIRST_PROMPT);
    let act = rows
        .iter()
        .position(|r| r.starts_with("> [aterm harness]"))
        .expect("the Land act");
    let caret = rows.iter().rposition(|r| r == ">").expect("the composer");
    let mut first = rows[..act - 1].to_vec();
    first.extend(rows[caret - 1..].iter().cloned());
    let user = first
        .iter()
        .position(|r| r.starts_with("> FOREVERLAB-WORKER"))
        .expect("the first prompt");
    let reply = "Implemented the parser module and its unit tests in this turn.";
    let misread = said_tail(&first).expect("words");
    assert!(
        misread.starts_with("(Started by `s-146719f2750f4a8f6909`") && misread.ends_with(reply),
        "{misread}"
    );
    assert!(needs_you(&misread).is_empty(), "{misread}");
    let task = b"FOREVERLAB-WORKER. write the parser for this repository and land it";
    let old = format!(
        "{}\n\n{}",
        std::str::from_utf8(task).expect("utf-8"),
        fact_line("s-146719f2750f4a8f6909", Autopilot::Land, "agy")
    );
    let drawn: Vec<&str> = first[user..user + 5]
        .iter()
        .map(|r| r.trim_start_matches('>').trim())
        .filter(|r| !r.is_empty())
        .collect();
    assert_eq!(
        drawn.join(" "),
        old.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    let now = text(task, Some("s-146719f2750f4a8f6909"), Program::Agy);
    assert!(!now.contains("\n\n"), "{now}");
    assert!(first[user + 1].is_empty(), "the measured blank row");
    let mut one = first.clone();
    one.remove(user + 1);
    assert_eq!(said_tail(&one).as_deref(), Some(reply));
    assert!(needs_you(reply).is_empty());
    assert_eq!(
        turn_opened_by(&one).as_deref(),
        Some(
            now.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .as_str()
        )
    );
}

#[test]
fn a_worker_is_under_land_unless_its_spawn_says_cruise() {
    assert_eq!(WORKER_AUTOPILOT, Autopilot::Land);
    let order = TaskOrder::of_frame(
        Some(b"t".to_vec()),
        aterm_phase::Program::Claude,
        None,
        None,
        None,
    )
    .expect("an order");
    assert_eq!(order.autopilot, Autopilot::Land);
    assert_eq!(order.clone().under(None).autopilot, Autopilot::Land);
    assert_eq!(
        order.under(Some(Autopilot::Cruise)).autopilot,
        Autopilot::Cruise,
        "CONTROL: the spawn's word"
    );
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

    let cancelled = TaskSeed::new(b"never started".to_vec());
    let late = cancelled.clone();
    cancelled.discard();
    let sid = SessionId::new("s-0001");
    assert_eq!(
        late.write_in(&dir, &sid).unwrap_err().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(!dir.join(task_file_name(&sid)).exists());

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

    let running = TaskSeed::new(b"still needed".to_vec());
    let path = running.write_in(&dir, &SessionId::new("s-0003")).unwrap();
    drop(running);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"still needed",
        "a launch with an unobserved outcome keeps it"
    );
}

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

#[test]
fn a_worker_carries_its_starters_configuration_or_uses_the_plain_command() {
    use aterm_agent::harness::worker::{Starter, WORKERS};
    for cli in &WORKERS {
        let exe = PathBuf::from(format!("/opt/twin/{}", cli.word));
        let starter = || Starter {
            exe: exe.clone(),
            env: vec![("HOME".to_string(), "/homes/a".to_string())],
            args: Ok(vec!["--provider".to_string(), "p".to_string()]),
            unset: Vec::new(),
        };
        assert_eq!(
            line_starter(cli, Some(starter())),
            starter(),
            "{}",
            cli.word
        );
        if cli.agent == aterm_phase::Program::Codex {
            // codex-alab's line with no starter is atpkg's managed twin where
            // this machine has one: `WorkerCli::unstarted_by`'s own test.
            continue;
        }
        assert_eq!(
            line_starter(cli, None),
            Starter {
                exe: PathBuf::from(cli.word),
                env: Vec::new(),
                args: Ok(Vec::new()),
                unset: Vec::new(),
            },
            "a starter that runs no {}: its plain word",
            cli.agent.product()
        );
    }
}

#[test]
fn a_workers_line_trusts_only_the_folder_its_shell_stands_in() {
    let at = |dir: &'static str| move || Some(PathBuf::from(dir));
    let unread = || -> Option<PathBuf> { panic!("the shell's directory is not read") };
    assert!(trusted_here(true, Some("/w/proj"), at("/w/proj")));
    assert!(!trusted_here(true, Some("/w/proj"), at("/w")), "moved up");
    assert!(
        !trusted_here(true, Some("/w/proj"), at("/w/proj/sub")),
        "beneath it is not it"
    );
    assert!(!trusted_here(true, Some("/w/proj"), || None), "not read");
    assert!(!trusted_here(true, None, unread), "no cwd=");
    assert!(!trusted_here(false, Some("/w/proj"), unread), "no flag");
    let flagged: Vec<_> = aterm_agent::harness::worker::WORKERS
        .iter()
        .filter(|cli| cli.trust_flag.is_some())
        .map(|cli| cli.word)
        .collect();
    assert_eq!(flagged, ["muse"]);
}

/// A TRUSTED-FOLDERS SIDECAR AN OLDER ATERM WROTE BESIDE A GEMINI CLI WORKER'S TASK FILE GOES
/// WITH ITS WORKER: a session handed across the upgrade can still have one; another
/// worker's stays.
#[cfg(unix)]
#[test]
fn an_older_builds_trust_sidecar_goes_with_its_worker() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-trust-file")
        .tempdir()
        .expect("scratch dir");
    let dir = scratch.path().join("tasks");
    let (gone, kept) = (SessionId::new("s-0d"), SessionId::new("s-0e"));
    let task = TaskSeed::new(b"task".to_vec())
        .write_in(&dir, &gone)
        .expect("written");
    let other_task = TaskSeed::new(b"other task".to_vec())
        .write_in(&dir, &kept)
        .expect("written");
    let (path, other) = (
        dir.join(trust_file_name(&gone)),
        dir.join(trust_file_name(&kept)),
    );
    assert_eq!(
        path,
        dir.join("s-0d-1.trust.json"),
        "the name it was written under"
    );
    for sidecar in [&path, &other] {
        std::fs::write(sidecar, "{}").unwrap();
    }
    retire_task_file_in(&dir, &gone);
    assert!(!task.exists() && !path.exists(), "removed with its worker");
    assert!(
        other_task.exists() && other.exists(),
        "another worker's stays"
    );
}

#[test]
fn a_starter_not_named_yet_is_read_from_its_terminal() {
    use aterm_phase::Program;
    let unread = || -> Option<i32> { panic!("the front group is not read") };
    for agent in [Program::Claude, Program::Codex, Program::Agy, Program::Muse] {
        assert_eq!(
            starter_group(Some(agent), 41, unread, agent),
            Some(StarterGroup::Named(41)),
            "named {agent:?}: the sweep's group, nothing read"
        );
        assert_eq!(
            starter_group(None, 0, || Some(77), agent),
            Some(StarterGroup::Front(77)),
            "not named yet: the terminal's front group"
        );
        assert_eq!(
            starter_group(None, 0, || None, agent),
            None,
            "no front group"
        );
        assert_eq!(
            starter_group(None, 0, || Some(0), agent),
            None,
            "no group is none"
        );
        for other in Program::ALL.into_iter().filter(|p| *p != agent) {
            assert_eq!(
                starter_group(Some(other), 41, unread, agent),
                None,
                "{other:?} under a {agent:?} worker: another agent's program is never followed"
            );
        }
    }
}

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
/// included, and so does a file that is no result of it.
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
/// by one, so no close would.
#[cfg(unix)]
#[test]
fn a_quit_removes_its_workers_task_files_and_no_other() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-task-quit")
        .tempdir()
        .expect("scratch dir");
    let dir = scratch.path().join("tasks");
    let store = crate::session_store::new_store();
    let (worker, shell) = (
        crate::session_store::test_handle(1),
        crate::session_store::test_handle(2),
    );
    store.write().unwrap().register(worker.clone());
    store.write().unwrap().register(shell.clone());
    crate::task_record::mark_session_task(&worker.ctx, 1, &TaskMark::new(TaskState::Working))
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

/// A pooled worker's starter on a row of its own exports the row's login and removes the
/// credential variables its CLI carries in place of it: a Codex worker's exact line removes
/// Codex's own two and leaves `OPENAI_API_KEY`; a Grok Build row also names its own leader socket
/// in the grok home its starter carries, else `~/.grok`, and drops the starter's.
#[test]
fn a_pooled_worker_runs_on_its_rows_login_alone() {
    use aterm_agent::harness::accounts::Cli;
    use aterm_agent::harness::accounts_pool::LoginEnv;
    use aterm_agent::harness::worker::{Admitted, Fresh, Refused, Starter, admit, worker_cli};
    let words = |list: &[&str]| list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let home = Path::new("/Users//u");
    let admitted = |cli, onto: &Starter| -> Result<Admitted, Refused> {
        admit(
            cli,
            onto.clone(),
            Some(home),
            |_| true,
            |_| panic!("a present program is never held"),
            |_| panic!("a present program is never installed"),
        )
    };
    let line = |cli, onto: &Starter, fresh: &Fresh<'_>| {
        let admitted = admitted(cli, onto).map_err(|refused| refused.why().to_string())?;
        aterm_agent::harness::worker::line(cli, Dialect::Zsh, &admitted, fresh)
    };
    let codex = Starter {
        exe: PathBuf::from("/opt/twin/codex"),
        env: Vec::new(),
        args: Ok(words(&["-c", "x=1"])),
        unset: Vec::new(),
    };
    let pick = LoginEnv {
        label: "work".to_string(),
        var: "CODEX_AUTH_PATH",
        value: Some("/c/work/auth.json".to_string()),
    };
    let onto = pool_onto(codex, Cli::Codex, &pick, home).expect("codex row");
    let cli = worker_cli(Program::Codex).expect("codex worker row");
    assert_eq!(
        line(cli, &onto, &Fresh::default()).expect("codex line"),
        concat!(
            " '/usr/bin/env' -u 'CODEX_API_KEY' -u 'CODEX_ACCESS_TOKEN' 'CODEX_AUTH_PATH=/c/work/auth.json'",
            " '/opt/twin/codex' '-c' 'x=1' -- \"$(command cat -- \"$ATERM_TASK_FILE\")\""
        )
    );

    let grok = |env: &[(&str, &str)]| Starter {
        exe: PathBuf::from("/opt/grok/bin/grok"),
        env: env
            .iter()
            .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
            .collect(),
        args: Ok(words(&[
            "--leader-socket",
            "/Users//u/.grok/leader-old.sock",
            "-m",
            "grok-4",
        ])),
        unset: Vec::new(),
    };
    let work = LoginEnv {
        label: "work".to_string(),
        var: "GROK_AUTH_PATH",
        value: Some("/g/work/auth.json".to_string()),
    };
    let onto = pool_onto(grok(&[]), Cli::Grok, &work, home).expect("grok row");
    assert_eq!(
        onto.args,
        Ok(words(&[
            "-m",
            "grok-4",
            "--leader-socket",
            "/Users//u/.grok/leader-work-9ca20914.sock"
        ]))
    );
    assert_eq!(onto.unset, ["GROK_AUTH", "GROK_LEADER_SOCKET"]);
    assert_eq!(
        onto.env,
        [(
            "GROK_AUTH_PATH".to_string(),
            "/g/work/auth.json".to_string()
        )]
    );
    let carried = pool_onto(grok(&[("GROK_HOME", "/g/home")]), Cli::Grok, &work, home)
        .expect("grok row in its starter's home");
    assert_eq!(
        carried.args,
        Ok(words(&[
            "-m",
            "grok-4",
            "--leader-socket",
            "/g/home/leader-work-9ca20914.sock"
        ]))
    );

    // Negative: the default row adds no socket or login (it still drops the starter's socket and
    // removes `GROK_AUTH`), and a grok home that cannot be read names no row's socket.
    let main = LoginEnv {
        label: "main".to_string(),
        value: None,
        ..work.clone()
    };
    let onto = pool_onto(grok(&[]), Cli::Grok, &main, home).expect("default row");
    assert_eq!(onto.args, Ok(words(&["-m", "grok-4"])));
    assert_eq!(onto.unset, ["GROK_AUTH_PATH", "GROK_AUTH"]);
    assert!(onto.env.is_empty(), "{:?}", onto.env);
    assert!(
        pool_onto(grok(&[("GROK_HOME", "rel")]), Cli::Grok, &work, home).is_err(),
        "a relative GROK_HOME"
    );

    // A GROK BUILD WORKER'S LINE RUNS ON ITS ROW'S LOGIN ALONE: the pooled auth file, the row's
    // own leader socket, the starter's inline login and socket variable removed, its own updater
    // off; and the sign-in it is admitted on is that row's file, not the home's.
    let onto = pool_onto(grok(&[]), Cli::Grok, &work, home).expect("grok row");
    let cli = worker_cli(Program::Grok).expect("grok worker row");
    let refused = admitted(cli, &onto).expect_err("no such row file here");
    assert!(
        matches!(&refused, Refused::SignedOut(why) if why.contains("/g/work/auth.json")),
        "{refused:?}"
    );
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let row_login = tmp.path().join("work").join("auth.json");
    std::fs::create_dir_all(row_login.parent().expect("its folder")).expect("row folder");
    std::fs::write(&row_login, "{\"token\":\"w\"}").expect("the row's sign-in");
    let row_login = row_login.to_str().expect("UTF-8").to_string();
    let signed = LoginEnv {
        value: Some(row_login.clone()),
        ..work.clone()
    };
    let onto = pool_onto(grok(&[]), Cli::Grok, &signed, home).expect("grok row");
    let socket =
        aterm_agent::harness::relaunch::grok_leader_socket(&home.join(".grok"), "work", &row_login)
            .expect("the row's socket");
    assert_eq!(
        line(cli, &onto, &Fresh::default()).expect("grok line"),
        format!(
            " '/usr/bin/env' -u 'GROK_AUTH' -u 'GROK_LEADER_SOCKET' 'GROK_AUTH_PATH={row_login}' \
             'GROK_DISABLE_AUTOUPDATER=1' '/opt/grok/bin/grok' '-m' 'grok-4' '--leader-socket' \
             '{}' -- \"$(command cat -- \"$ATERM_TASK_FILE\")\"",
            socket.display()
        )
    );
}

/// A fake store of worker programs: what it holds, holds back, and was asked, in order.
struct FakeStore {
    present: std::cell::Cell<bool>,
    held: Option<&'static str>,
    lays: bool,
    asked: std::cell::RefCell<Vec<String>>,
}

impl ProgramStore for FakeStore {
    fn present(&self, _: &str) -> bool {
        self.asked.borrow_mut().push("present".to_string());
        self.present.get()
    }

    fn held(&self, _: &str) -> Option<String> {
        self.asked.borrow_mut().push("held".to_string());
        self.held.map(str::to_string)
    }

    fn install(&self, program: &str) -> Result<(), String> {
        self.asked.borrow_mut().push(format!("install {program}"));
        if !self.lays {
            return Err("atpkg: grok: vendor head unreachable".to_string());
        }
        self.present.set(true);
        Ok(())
    }
}

/// A WORKER'S LAUNCHER ASKS THE SIGN-IN BEFORE IT INSTALLS, AND SAYS WHAT IT DOES.
/// - Signed out, a Grok worker is refused with `why=signin` and the store is never asked.
///   Installing `grok` would lay `agents/grok` on every shell's `PATH`, signed out.
/// - Signed in, an absent `grok` is installed once by atpkg's verified install, as an absent
///   program of any row atpkg ships is (a fresh Mac before atpkg's update pass lands it). The
///   launch record says it is installing, then installed, and the installing mark stands exactly
///   while the install runs.
/// - CONTROLS: a present program installs nothing and says nothing; a held one (`[packages]
///   exclude`, a removal, a no to installs) installs nothing and is admitted carrying the hold's
///   words; a failed
///   install is `why=install` carrying atpkg's words.
/// - The install is atpkg's `install <program>` verb, waiting its turn behind a running pass.
#[test]
fn a_grok_workers_launcher_asks_the_sign_in_before_it_installs() {
    use aterm_agent::harness::worker::Refused;
    let cli = aterm_agent::harness::worker::worker_cli(Program::Grok).expect("grok row");
    let store = |present, held, lays| FakeStore {
        present: std::cell::Cell::new(present),
        held,
        lays,
        asked: std::cell::RefCell::new(Vec::new()),
    };
    let d = aterm_tempfile::Builder::new()
        .prefix("aterm-grok-launch")
        .tempdir()
        .expect("scratch dir");
    let user = d.path().join("home");
    let grok_home = user.join(".grok");
    std::fs::create_dir_all(&grok_home).expect("~/.grok");
    let notes = std::cell::RefCell::new(Vec::<String>::new());
    let note = |n: String| notes.borrow_mut().push(n);
    let marks = std::cell::Cell::new(0_u32);
    struct Mark<'a>(&'a std::cell::RefCell<Vec<String>>);
    impl Drop for Mark<'_> {
        fn drop(&mut self) {
            self.0.borrow_mut().push("mark dropped".to_string());
        }
    }
    let admit_on = |cli: &aterm_agent::harness::worker::WorkerCli, store: &FakeStore| {
        admit_worker(
            cli,
            cli.plain(),
            Some(&user),
            &note,
            &|| {
                marks.set(marks.get() + 1);
                Mark(&store.asked)
            },
            store,
        )
    };
    let admit = |store: &FakeStore| admit_on(cli, store);

    let signed_out = store(false, None, true);
    let refused = admit(&signed_out).expect_err("signed out");
    assert!(matches!(&refused, Refused::SignedOut(_)), "{refused:?}");
    assert_eq!(refused_why(&refused), crate::task_record::WHY_SIGNIN);
    assert!(
        signed_out.asked.borrow().is_empty(),
        "nothing looked for or installed: {:?}",
        signed_out.asked.borrow()
    );
    assert!(notes.take().is_empty() && marks.get() == 0);

    std::fs::write(grok_home.join("auth.json"), "{\"token\":\"t\"}").expect("a sign-in");
    let absent = store(false, None, true);
    let admitted = admit(&absent).expect("installed");
    assert_eq!(
        *absent.asked.borrow(),
        ["present", "held", "install grok", "mark dropped", "present"],
        "installed once, under the installing mark"
    );
    assert_eq!(marks.get(), 1);
    assert_eq!(
        admitted.readiness(),
        &aterm_agent::harness::worker::Readiness::Installed
    );
    let said = notes.take();
    assert_eq!(said.len(), 2, "installing, then installed: {said:?}");
    assert!(said.iter().all(|n| n.contains("grok")), "{said:?}");

    let there = store(true, None, true);
    assert_eq!(admit(&there).expect("present").kept_off(), None, "CONTROL");
    assert_eq!(*there.asked.borrow(), ["present"]);
    assert!(notes.take().is_empty());

    let hold = "`[packages] exclude` in aterm.toml names it";
    let held = store(false, Some(hold), true);
    let admitted = admit(&held).expect("held: admitted, installing nothing");
    assert_eq!(*held.asked.borrow(), ["present", "held"], "CONTROL: held");
    assert!(
        admitted.kept_off().is_some_and(|said| said.contains(hold)),
        "{admitted:?}"
    );
    assert!(notes.take().is_empty() && marks.get() == 1);

    let failing = store(false, None, false);
    let err = admit(&failing).expect_err("a failed install");
    assert!(err.why().contains("vendor head unreachable"), "{err:?}");
    assert_eq!(refused_why(&err), crate::task_record::WHY_INSTALL);
    assert!(
        failing.asked.borrow().contains(&"mark dropped".to_string()),
        "the mark goes with a failed install too"
    );

    let claude = aterm_agent::harness::worker::worker_cli(Program::Claude).expect("claude row");
    let fresh_mac = store(false, None, true);
    notes.take();
    let admitted = admit_on(claude, &fresh_mac).expect("claude installed");
    assert_eq!(
        *fresh_mac.asked.borrow(),
        [
            "present",
            "held",
            "install claude",
            "mark dropped",
            "present"
        ],
        "a default-set row atpkg's pass has not landed yet is installed too"
    );
    assert_eq!(
        admitted.readiness(),
        &aterm_agent::harness::worker::Readiness::Installed
    );

    let layout = atpkg::store::Layout {
        prefix: PathBuf::from("/p/pkg"),
    };
    let argv: Vec<String> = install_argv("grok", Some(&layout))
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(&argv[..2], ["install", "grok"]);
    assert_eq!(
        &argv[2..4],
        [
            "--wait-lock".to_string(),
            crate::ATPKG_WAIT_LOCK_SECS.to_string()
        ]
    );
    assert_eq!(argv[4], "--progress-file");
    assert_eq!(Path::new(&argv[5]), layout.progress_file());
}

/// A scratch store of worker programs whose holds are [`AtpkgStore`]'s own reading of an
/// aterm.toml's `[packages]` and a store ([`held_in`]), and whose install is a stand-in that lays
/// the program: what a spawn does under a person's offs, with no network and no real atpkg.
struct ConsentStore<'a> {
    cfg: atpkg::config::PackagesConfig,
    layout: &'a atpkg::store::Layout,
    installs: std::cell::Cell<u32>,
}

impl ConsentStore<'_> {
    fn laid(&self, program: &str) -> PathBuf {
        self.layout.prefix.join("laid").join(program)
    }
}

impl ProgramStore for ConsentStore<'_> {
    fn present(&self, program: &str) -> bool {
        self.laid(program).is_file()
    }

    fn held(&self, program: &str) -> Option<String> {
        held_in(program, &self.cfg, Some(self.layout))
    }

    fn install(&self, program: &str) -> Result<(), String> {
        self.installs.set(self.installs.get() + 1);
        let laid = self.laid(program);
        std::fs::create_dir_all(laid.parent().expect("its folder")).map_err(|e| e.to_string())?;
        std::fs::write(laid, "the stand-in's build\n").map_err(|e| e.to_string())
    }
}

/// A SPAWN INSTALLS NO AGENT A PERSON SAID NO TO INSTALLING, BY ATPKG'S OWN READING OF THAT NO:
/// the reading atpkg's update pass completes the default set by
/// ([`atpkg::adoption::unattended_install_off`]).
/// - `[packages] auto_install = false` (Settings ▸ Packages ▸ "Install ALab tools" off), a
///   decline `aterm pkg uninstall --all` recorded on the store, `[packages] enabled = false`
///   (Settings ▸ Packages ▸ "Keep ALab tools up to date" off), a `[packages] prefix` this
///   build ignores and a `[packages] exclude` naming the program each install nothing: the
///   worker is admitted, and its launch note names that off.
/// - CONTROLS: the default table on a fresh store, and an exclude naming another program,
///   install the absent program once, by the stand-in installer, and it is then present.
#[test]
fn a_spawn_installs_no_agent_where_a_person_said_no_to_installs() {
    use aterm_agent::harness::worker::Readiness;
    let cli = aterm_agent::harness::worker::worker_cli(Program::Claude).expect("claude row");
    let program = cli.atpkg_program().expect("atpkg ships claude");
    assert!(
        atpkg::vendor_direct::served_here(program),
        "PRECONDITION: atpkg serves {program} on this machine"
    );
    let d = aterm_tempfile::Builder::new()
        .prefix("aterm-spawn-consent")
        .tempdir()
        .expect("scratch dir");
    let home = d.path().join("home");
    let admit = |case: &str, cfg: atpkg::config::PackagesConfig, declined: bool| {
        let layout = atpkg::store::Layout {
            prefix: d.path().join(case).join("pkg"),
        };
        std::fs::create_dir_all(&layout.prefix).expect("the store");
        if declined {
            std::fs::write(layout.declined(), "# declined\n").expect("the decline marker");
        }
        let store = ConsentStore {
            cfg,
            layout: &layout,
            installs: std::cell::Cell::new(0),
        };
        let admitted = admit_worker(cli, cli.plain(), None, &|_| {}, &|| (), &store)
            .unwrap_or_else(|refused| panic!("{case}: {refused:?}"));
        (admitted, store.installs.get(), store.present(program))
    };
    let parsed = |text: &str| atpkg::config::parse_packages_at(text, false, Some(home.as_path()));

    let other = if program == "codex" {
        "claude"
    } else {
        "codex"
    };
    for (case, cfg) in [
        ("default", parsed("[packages]\n")),
        (
            "other-excluded",
            parsed(&format!("[packages]\nexclude = [\"{other}\"]\n")),
        ),
    ] {
        let (admitted, installs, present) = admit(case, cfg, false);
        assert_eq!(
            (admitted.readiness(), installs, present),
            (&Readiness::Installed, 1, true),
            "CONTROL {case}: the absent program is installed once"
        );
    }

    let foreign = d.path().join("elsewhere");
    let prefix = format!("[packages]\nprefix = \"{}\"\n", foreign.display());
    let excluded = format!("[packages]\nexclude = [\"{program}\"]\n");
    for (case, cfg, declined, words) in [
        (
            "auto-install-off",
            parsed("[packages]\nauto_install = false\n"),
            false,
            "auto_install = false",
        ),
        ("declined", parsed("[packages]\n"), true, "uninstall --all"),
        (
            "updates-off",
            parsed("[packages]\nenabled = false\n"),
            false,
            atpkg::adoption::UPDATES_OFF,
        ),
        ("ignored-prefix", parsed(&prefix), false, "prefix"),
        ("excluded", parsed(&excluded), false, "`[packages] exclude`"),
    ] {
        let (admitted, installs, present) = admit(case, cfg, declined);
        assert_eq!((installs, present), (0, false), "{case}: nothing installed");
        assert!(
            admitted
                .kept_off()
                .is_some_and(|said| said.contains(words) && said.contains(program)),
            "{case}: the launch note names the off ({words:?}): {admitted:?}"
        );
    }
}

/// A LAUNCHER'S INSTALLING MARK LIVES ON ITS SESSION'S TIMELINE WHILE IT STANDS, AND GOES WHEN
/// IT DROPS: the startup deadline reads it to leave an install unjudged.
#[test]
fn an_installing_mark_stands_on_the_timeline_while_it_lives() {
    let h = crate::stub_session(1);
    let installing = || h.ctx.timeline.lock().unwrap().worker_installing();
    assert!(!installing(), "PRECONDITION: no install");
    {
        let _mark = InstallingMark::of(&h.ctx);
        assert!(installing());
    }
    assert!(!installing(), "dropped with its install");
}

#[test]
fn pooled_starter_rotates_children_of_a_pooled_manager_and_preserves_custom_homes() {
    use aterm_agent::harness::accounts::{self, Cli};
    use aterm_agent::harness::accounts_pool::Seat;
    use aterm_agent::harness::codex_app_login::Usage;
    use aterm_agent::harness::worker::Starter;

    const NOW: i64 = 1_792_700_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let (cfg, state, home) = (root.join("cfg"), root.join("state"), root.join("home"));
    for dir in [&cfg, &state, &home] {
        std::fs::create_dir_all(dir).expect("dir");
    }
    let c1 = root.join("logins/codex/codex-1");
    let c2 = root.join("logins/codex/codex-2");
    // `main`, the default row, is signed in on this machine.
    for dir in [&c1, &c2, &home.join(".codex")] {
        std::fs::create_dir_all(dir).expect("login dir");
        std::fs::write(dir.join("auth.json"), "{}").expect("auth");
    }
    std::fs::write(
        cfg.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"codex-1\"\nclass = \"dedicated\"\ndir = \"{}\"\n\n\
             [[account]]\nlabel = \"codex-2\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            c1.display(),
            c2.display()
        ),
    )
    .expect("roster");
    let seat = Seat {
        config_dir: &cfg,
        state: &state,
        home: &home,
        now: NOW,
    };
    let no_usage = |_: Cli, _: &Path, _: Option<&str>, _: i64| -> Option<Usage> { None };
    let alab = aterm_agent::harness::codex_alab::lay_package(&root.join("codex-alab"), true);

    // Wall `main` and `codex-1` so the pool's next usable row is `codex-2`.
    let mut note = None;
    seat.store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |s| {
            let mut ev = accounts::note_wall(s, "main", "seven_day", Some(NOW + 3600), NOW, NOW);
            ev.extend(accounts::note_wall(
                s,
                "codex-1",
                "seven_day",
                Some(NOW + 3600),
                NOW,
                NOW,
            ));
            Ok(ev)
        })
        .expect("wall main + codex-1");

    let starter_on = |pairs: &[(&str, &str)]| Starter {
        exe: alab.clone(),
        env: pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    let c1_auth = c1.join("auth.json").to_string_lossy().into_owned();
    let c2_auth = c2.join("auth.json").to_string_lossy().into_owned();

    // 1. Manager running on pooled row `codex-1` spawns a child worker -> rotates to `codex-2`
    //    with a single replaced `CODEX_AUTH_PATH` entry.
    let rotated = super::pooled_starter_in(
        starter_on(&[("CODEX_AUTH_PATH", &c1_auth)]),
        Cli::Codex,
        &seat,
        &no_usage,
        1,
    )
    .expect("pooled launch");
    assert_eq!(rotated.env, vec![("CODEX_AUTH_PATH".to_string(), c2_auth)]);

    // 2. When `main` (default home, `dir = None`) is pinned, a starter carrying `codex-1` drops
    //    `CODEX_AUTH_PATH` completely so the child worker uses the default login.
    seat.store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |s| {
            s.active = Some("main".to_string());
            s.pinned = true;
            s.proven = true;
            Ok(Vec::new())
        })
        .expect("pin main");
    let to_main = super::pooled_starter_in(
        starter_on(&[("CODEX_AUTH_PATH", &c1_auth)]),
        Cli::Codex,
        &seat,
        &no_usage,
        2,
    )
    .expect("pinned launch");
    assert!(to_main.env.is_empty(), "{:?}", to_main.env);

    // 3. CONTROL: a custom scratch `CODEX_HOME` outside the bound roster, a non-default CLI home
    //    (`GROK_HOME`), or an explicit credential override (`CODEX_API_KEY`, with or without
    //    `CODEX_AUTH_PATH`) is left untouched without reading usage.
    let panic_read = |_: Cli, _: &Path, _: Option<&str>, _: i64| -> Option<Usage> {
        panic!("custom home must not read usage")
    };
    let custom_dir = root.join("custom-scratch-codex").display().to_string();
    let custom = starter_on(&[("CODEX_HOME", &custom_dir)]);
    assert_eq!(
        super::pooled_starter_in(custom.clone(), Cli::Codex, &seat, &panic_read, 3),
        Ok(custom)
    );
    let overridden = starter_on(&[
        ("CODEX_AUTH_PATH", &c1_auth),
        ("CODEX_API_KEY", "sk-override"),
    ]);
    assert_eq!(
        super::pooled_starter_in(overridden.clone(), Cli::Codex, &seat, &panic_read, 4),
        Ok(overridden)
    );
    let default_overridden = starter_on(&[("CODEX_API_KEY", "sk-override")]);
    assert_eq!(
        super::pooled_starter_in(
            default_overridden.clone(),
            Cli::Codex,
            &seat,
            &panic_read,
            5,
        ),
        Ok(default_overridden)
    );
    let custom_grok = starter_on(&[("GROK_HOME", "/g/custom")]);
    assert_eq!(
        super::pooled_starter_in(custom_grok.clone(), Cli::Grok, &seat, &panic_read, 6),
        Ok(custom_grok)
    );
    for unpooled_auth in [
        "auth.json",
        "work/auth.json",
        "/custom/outside/auth.json/",
        "/custom/outside\n1/auth.json",
    ] {
        let unpooled = starter_on(&[("CODEX_AUTH_PATH", unpooled_auth)]);
        assert_eq!(
            super::pooled_starter_in(unpooled.clone(), Cli::Codex, &seat, &panic_read, 6),
            Ok(unpooled),
            "non-admitted CODEX_AUTH_PATH {unpooled_auth:?} must not misidentify as default row `main`"
        );
    }

    seat.store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |s| {
            s.pinned = false;
            Ok(accounts::note_wall(
                s,
                "codex-2",
                "five_hour",
                Some(NOW + 3600),
                NOW,
                NOW,
            ))
        })
        .expect("all accounts limited");
    let held = super::pooled_starter_in(starter_on(&[]), Cli::Codex, &seat, &no_usage, 7);
    assert!(held.unwrap_err().contains("no usable account"));
    std::fs::write(seat.store(Cli::Codex).state_file(), "not JSON").unwrap();
    let held = super::pooled_starter_in(starter_on(&[]), Cli::Codex, &seat, &panic_read, 8);
    assert!(held.unwrap_err().contains("state.json"));
    let bound = super::pooled_starter_in(
        starter_on(&[("CODEX_AUTH_PATH", &c1_auth)]),
        Cli::Codex,
        &seat,
        &panic_read,
        9,
    );
    assert!(bound.unwrap_err().contains("state.json"));
    let outside = starter_on(&[("CODEX_HOME", &custom_dir)]);
    assert_eq!(
        super::pooled_starter_in(outside.clone(), Cli::Codex, &seat, &panic_read, 9),
        Ok(outside)
    );
    let orphan = seat
        .store(Cli::Codex)
        .logins_dir()
        .join("orphaned-renewal")
        .join("auth.json");
    let orphan_text = orphan.display().to_string();
    let owned = super::pooled_starter_in(
        starter_on(&[("CODEX_AUTH_PATH", &orphan_text)]),
        Cli::Codex,
        &seat,
        &panic_read,
        10,
    );
    assert!(owned.unwrap_err().contains("no current account binding"));
    std::fs::write(seat.store(Cli::Codex).logins_file(), "not JSON").unwrap();
    assert!(
        super::pooled_starter_in(
            starter_on(&[("CODEX_AUTH_PATH", &c1_auth)]),
            Cli::Codex,
            &seat,
            &panic_read,
            10
        )
        .is_err()
    );
    let override_key = starter_on(&[("CODEX_API_KEY", "explicit-override")]);
    assert_eq!(
        super::pooled_starter_in(override_key.clone(), Cli::Codex, &seat, &panic_read, 11),
        Ok(override_key)
    );
}

/// A CODEX WORKER ON A ROW OF ITS OWN STARTS ONLY ON A PROGRAM THAT KEEPS `CODEX_AUTH_PATH`'S
/// LOGIN ITS OWN: OpenAI's builds, and codex-alab before the variable, read and refresh
/// `$CODEX_HOME/auth.json` whatever it names (measured 2026-10-27, `codex_alab::auth_path_unread`),
/// so the worker would run on the machine's default login while the pool counted it as the
/// row's. Each such program is refused before anything is typed, whether the row is the pool's
/// pick or one the starter carried; the default row's pick needs no such program.
#[test]
fn a_codex_row_of_its_own_is_refused_on_a_program_that_ignores_codex_auth_path() {
    use aterm_agent::harness::accounts::{self, Cli};
    use aterm_agent::harness::accounts_pool::Seat;
    use aterm_agent::harness::codex_alab::lay_package;
    use aterm_agent::harness::codex_app_login::Usage;
    use aterm_agent::harness::worker::Starter;

    const NOW: i64 = 1_792_700_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let (cfg, state, home) = (root.join("cfg"), root.join("state"), root.join("home"));
    for dir in [&cfg, &state, &home] {
        std::fs::create_dir_all(dir).expect("dir");
    }
    let work = root.join("logins/codex/work");
    std::fs::create_dir_all(&work).expect("login dir");
    std::fs::write(work.join("auth.json"), "{}").expect("auth");
    // The default row is signed in here: its login is the default home's.
    std::fs::create_dir_all(home.join(".codex")).expect("default home");
    std::fs::write(home.join(".codex/auth.json"), "{}").expect("default login");
    std::fs::write(
        cfg.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            work.display()
        ),
    )
    .expect("roster");
    let seat = Seat {
        config_dir: &cfg,
        state: &state,
        home: &home,
        now: NOW,
    };
    let no_usage = |_: Cli, _: &Path, _: Option<&str>, _: i64| -> Option<Usage> { None };
    let mut note = None;
    seat.store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |s| {
            Ok(accounts::note_wall(
                s,
                "main",
                "seven_day",
                Some(NOW + 3600),
                NOW,
                NOW,
            ))
        })
        .expect("wall main so the pool picks `work`");
    let on = |exe: PathBuf| Starter {
        exe,
        env: Vec::new(),
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    let reads = lay_package(&root.join("store/codex/new"), true);
    let before = lay_package(&root.join("store/codex/old"), false);
    let vendor = lay_package(&root.join("store/codex-old/vendor"), true);
    std::fs::remove_file(root.join("store/codex-old/vendor/alab-build.json"))
        .expect("an OpenAI package carries no marker");
    let twin = root.join("agents-codex");
    std::fs::write(
        &twin,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", before.display()),
    )
    .expect("an atpkg launcher of the old build");
    let refused = [
        PathBuf::from("codex"),
        vendor.clone(),
        before.clone(),
        twin.clone(),
    ];
    for exe in &refused {
        super::pooled_starter_in(on(exe.clone()), Cli::Codex, &seat, &no_usage, 1)
            .expect_err("no worker starts on a program that ignores CODEX_AUTH_PATH");
    }
    let work_auth = work.join("auth.json").display().to_string();
    let started = super::pooled_starter_in(on(reads.clone()), Cli::Codex, &seat, &no_usage, 2)
        .expect("a codex-alab that keeps CODEX_AUTH_PATH's login starts on the row");
    assert_eq!(
        started.env,
        vec![("CODEX_AUTH_PATH".to_string(), work_auth.clone())]
    );

    // With rotation off and no pin the pool picks nothing, and a starter that carries the bound
    // row's variable hands it on as it is: that too needs a program that keeps its login.
    std::fs::write(cfg.join("aterm.toml"), "[harness]\naccounts = \"off\"\n").expect("aterm.toml");
    let carrying = |exe: PathBuf| Starter {
        env: vec![("CODEX_AUTH_PATH".to_string(), work_auth.clone())],
        ..on(exe)
    };
    for exe in &refused {
        super::pooled_starter_in(carrying(exe.clone()), Cli::Codex, &seat, &no_usage, 4)
            .expect_err("a carried row of its own is held to the same program");
    }
    let carried = carrying(reads.clone());
    assert_eq!(
        super::pooled_starter_in(carried.clone(), Cli::Codex, &seat, &no_usage, 5),
        Ok(carried)
    );
    std::fs::remove_file(cfg.join("aterm.toml")).expect("rotation back on");
    seat.store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |s| {
            s.active = Some("main".to_string());
            s.pinned = true;
            s.proven = true;
            Ok(Vec::new())
        })
        .expect("pin the default row");
    let default_row = super::pooled_starter_in(on(vendor.clone()), Cli::Codex, &seat, &no_usage, 3)
        .expect("the default row's pick runs on any build");
    assert!(default_row.env.is_empty(), "{:?}", default_row.env);
}

/// UNDER ROTATION OFF WITH NO PIN, A CODEX WORKER WHOSE STARTER CARRIES A ROW'S OWN
/// `CODEX_AUTH_PATH`, ON A PROGRAM THAT IGNORES IT, while a sign-in moved the row that was on
/// this machine's own login onto a login of its own, is refused with a way back that works
/// there: it never says to turn rotation off, which it is already, and a starter that carries no
/// row's variable starts on this machine's own login. Under rotation on with Codex's login kept
/// in the OS keyring, where the pool picks no row either, the same refusal never says rotation
/// is off, and under rotation off with that keyring and an account pinned, where the keyring and
/// not rotation off is why no row is picked, it never says no account is pinned.
#[test]
fn a_carried_codex_row_under_rotation_off_is_told_a_way_back_that_works_there() {
    use aterm_agent::harness::accounts::{self, Cli};
    use aterm_agent::harness::accounts_pool::Seat;
    use aterm_agent::harness::codex_alab::{AUTH_PATH_ENV, lay_package};
    use aterm_agent::harness::codex_app_login::Usage;
    use aterm_agent::harness::worker::Starter;

    const NOW: i64 = 1_792_700_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let (cfg, state, home) = (root.join("cfg"), root.join("state"), root.join("home"));
    for dir in [&cfg, &state, &home.join(".codex")] {
        std::fs::create_dir_all(dir).expect("dir");
    }
    std::fs::write(home.join(".codex/auth.json"), "{}").expect("default login");
    let work = root.join("logins/codex/work");
    let staging = root.join("logins/codex/main+1");
    for dir in [&work, &staging] {
        std::fs::create_dir_all(dir).expect("login dir");
        std::fs::write(dir.join("auth.json"), "{}").expect("auth");
    }
    std::fs::write(
        cfg.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            work.display()
        ),
    )
    .expect("roster");
    std::fs::write(cfg.join("aterm.toml"), "[harness]\naccounts = \"off\"\n").expect("aterm.toml");
    let seat = Seat {
        config_dir: &cfg,
        state: &state,
        home: &home,
        now: NOW,
    };
    let mut note = None;
    seat.store(Cli::Codex)
        .transact_logins(NOW, &mut note, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "main",
                None,
                &staging.display().to_string(),
                None,
                NOW,
            ))
        })
        .expect("a sign-in binds `main` to its own login");
    let no_usage = |_: Cli, _: &Path, _: Option<&str>, _: i64| -> Option<Usage> { None };
    let old = lay_package(&root.join("store/codex/old"), false);
    let on = |env: Vec<(String, String)>| Starter {
        exe: old.clone(),
        env,
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    let work_auth = work.join("auth.json").display().to_string();
    let refused = super::pooled_starter_in(
        on(vec![(AUTH_PATH_ENV.to_string(), work_auth)]),
        Cli::Codex,
        &seat,
        &no_usage,
        1,
    )
    .expect_err("`work`'s turns would run on the default login");
    assert!(
        refused.contains("`main`, the row that login was listed as"),
        "{refused}"
    );
    assert!(
        !refused.contains("[harness] accounts"),
        "rotation is off already: {refused}"
    );
    assert!(
        refused.contains("with rotation off and no account pinned, as here"),
        "{refused}"
    );
    let plain = on(Vec::new());
    assert_eq!(
        super::pooled_starter_in(plain.clone(), Cli::Codex, &seat, &no_usage, 2),
        Ok(plain.clone()),
        "the way back it names starts"
    );
    std::fs::write(cfg.join("aterm.toml"), "").expect("rotation on");
    std::fs::write(
        home.join(".codex/config.toml"),
        "cli_auth_credentials_store = \"auto\"\n",
    )
    .expect("the OS keyring holds Codex's login");
    assert!(seat.keyring_held(Cli::Codex));
    let work_auth = work.join("auth.json").display().to_string();
    let keyring = super::pooled_starter_in(
        on(vec![(AUTH_PATH_ENV.to_string(), work_auth)]),
        Cli::Codex,
        &seat,
        &no_usage,
        3,
    )
    .expect_err("`work`'s turns would run on the default login");
    assert!(
        keyring.contains("the pool picks no row for a Codex launch here"),
        "{keyring}"
    );
    assert!(
        !keyring.contains("rotation off"),
        "rotation is on here: {keyring}"
    );
    assert_eq!(
        super::pooled_starter_in(plain.clone(), Cli::Codex, &seat, &no_usage, 4),
        Ok(plain.clone()),
        "the way back it names starts"
    );
    std::fs::write(cfg.join("aterm.toml"), "[harness]\naccounts = \"off\"\n").expect("aterm.toml");
    seat.store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |s| {
            s.active = Some("work".to_string());
            s.pinned = true;
            Ok(Vec::new())
        })
        .expect("pin `work`");
    assert!(seat.keyring_held(Cli::Codex) && seat.mode() == accounts::Mode::Off);
    let work_auth = work.join("auth.json").display().to_string();
    let pinned = super::pooled_starter_in(
        on(vec![(AUTH_PATH_ENV.to_string(), work_auth)]),
        Cli::Codex,
        &seat,
        &no_usage,
        5,
    )
    .expect_err("`work`'s turns would run on the default login");
    assert!(
        pinned.contains("the pool picks no row for a Codex launch here"),
        "{pinned}"
    );
    assert!(
        !pinned.contains("no account pinned"),
        "an account is pinned: {pinned}"
    );
    assert_eq!(
        super::pooled_starter_in(plain.clone(), Cli::Codex, &seat, &no_usage, 6),
        Ok(plain),
        "the way back it names starts"
    );
}

/// A Codex pool of `main` (this machine's own login, signed in in the default home) and `work`
/// (a row of its own, signed in), whose `main` the usage reader shows at its five-hour limit
/// until `reset` and `work` usable.
struct CodexPool {
    _tmp: aterm_tempfile::TempDir,
    root: PathBuf,
    cfg: PathBuf,
    state: PathBuf,
    home: PathBuf,
    work_auth: String,
}

const POOL_NOW: i64 = 1_792_700_000;

/// What [`CodexPool::launch`]'s usage reader reads for `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MainReads {
    /// Empty: usable.
    Empty,
    /// At its five-hour limit before [`POOL_RESET`].
    Walled,
    /// Nothing (no usage read answers, as for a login that cannot be read).
    Unread,
}
const POOL_RESET: i64 = POOL_NOW + 4 * 3600;

impl CodexPool {
    fn new() -> Self {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical");
        let (cfg, state, home) = (root.join("cfg"), root.join("state"), root.join("home"));
        for dir in [&cfg, &state, &home.join(".codex")] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        std::fs::write(home.join(".codex/auth.json"), "{}").expect("default login");
        let work = root.join("logins/codex/work");
        std::fs::create_dir_all(&work).expect("login dir");
        std::fs::write(work.join("auth.json"), "{}").expect("auth");
        std::fs::write(
            cfg.join("accounts-codex.toml"),
            format!(
                "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
                 [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
                work.display()
            ),
        )
        .expect("roster");
        let work_auth = work.join("auth.json").display().to_string();
        Self {
            _tmp: tmp,
            root,
            cfg,
            state,
            home,
            work_auth,
        }
    }

    fn seat(&self, now: i64) -> aterm_agent::harness::accounts_pool::Seat<'_> {
        aterm_agent::harness::accounts_pool::Seat {
            config_dir: &self.cfg,
            state: &self.state,
            home: &self.home,
            now,
        }
    }

    /// A worker launch of `exe` at `now` through the pool, reading `main` as `main` says, and
    /// `work` empty.
    fn launch(
        &self,
        exe: &Path,
        now: i64,
        main: MainReads,
    ) -> Result<aterm_agent::harness::worker::Starter, String> {
        use aterm_agent::harness::accounts::{self, Cli};
        use aterm_agent::harness::codex_app_login::Usage;
        let default_home = self.home.join(".codex");
        let read = |_: Cli, at: &Path, _: Option<&str>, now: i64| -> Option<Usage> {
            if at == default_home && main == MainReads::Unread {
                return None;
            }
            let walled = main == MainReads::Walled && at == default_home && now < POOL_RESET;
            Some(Usage {
                sample: accounts::Sample {
                    session_pct: Some(if walled { 100 } else { 0 }),
                    session_reset: Some(POOL_RESET),
                    at: now,
                    ..accounts::Sample::default()
                },
                walls: walled
                    .then_some((accounts::SESSION_LIMIT, Some(POOL_RESET)))
                    .into_iter()
                    .collect(),
            })
        };
        let starter = aterm_agent::harness::worker::Starter {
            exe: exe.to_path_buf(),
            env: Vec::new(),
            args: Ok(Vec::new()),
            unset: Vec::new(),
        };
        super::pooled_starter_in(starter, Cli::Codex, &self.seat(now), &read, 1)
    }

    /// The owner's pin (`aterm harness accounts use <label> --cli codex`) at `now`.
    fn pin(&self, label: &str, now: i64) -> Result<(), String> {
        use aterm_agent::harness::accounts::{self, Cli};
        let seat = self.seat(now);
        let roster = seat.bound_roster(Cli::Codex)?;
        let enrolled = accounts::enrolled_for(Cli::Codex, &roster);
        seat.store(Cli::Codex)
            .transact_within(&accounts::LOCK_LADDER, now, &mut None, |s| {
                accounts::pin(s, &roster, &enrolled, label, now).map(|e| vec![e])
            })
            .map(|_| ())
    }

    fn pinned(&self, now: i64) -> bool {
        use aterm_agent::harness::accounts::Cli;
        self.seat(now)
            .store(Cli::Codex)
            .load(now)
            .expect("state")
            .0
            .pinned
    }
}

fn auth_path_of(starter: &aterm_agent::harness::worker::Starter) -> Option<&str> {
    starter
        .env
        .iter()
        .find(|(name, _)| name == "CODEX_AUTH_PATH")
        .map(|(_, value)| value.as_str())
}

/// ON A CODEX PROGRAM THAT KEEPS NO ROW'S LOGIN ITS OWN, A WALLED DEFAULT ROW IS THE ANSWER, NOT A
/// PIN THE NEXT USAGE READ ENDS: the pool never picks `work` there (its turns would run on the
/// default login), so the launch is refused with `main`'s own limit and reset, and the same
/// answer comes back after the sample goes stale and is read again. The owner's pin on `main`
/// still starts it (a pin overrides the budget) and is charged to `main`, the row it runs under;
/// that pin ends at the next read of the wall, and the answer is the first one again.
#[test]
fn a_walled_default_on_a_program_that_ignores_codex_auth_path_refuses_alike_across_usage_reads() {
    use aterm_agent::harness::accounts_pool::USAGE_FRESH_S;
    use aterm_agent::harness::usage::rfc3339_utc;
    let pool = CodexPool::new();
    let old = aterm_agent::harness::codex_alab::lay_package(&pool.root.join("store/old"), false);

    let first = pool
        .launch(&old, POOL_NOW, MainReads::Walled)
        .expect_err("no Codex account runs here while `main` is walled");
    assert!(first.contains("five_hour"), "{first}");
    assert!(first.contains(&rfc3339_utc(POOL_RESET)), "{first}");
    assert!(first.contains("aterm pkg update codex"), "{first}");
    assert!(
        !first.contains("accounts use"),
        "no pin is offered: {first}"
    );

    pool.pin("main", POOL_NOW + 1)
        .expect("the owner pins `main`");
    let pinned = pool
        .launch(&old, POOL_NOW + 2, MainReads::Walled)
        .expect("the owner's pin on the default row starts it");
    assert_eq!(auth_path_of(&pinned), None, "{:?}", pinned.env);

    let stale = POOL_NOW + 2 + USAGE_FRESH_S + 1;
    let again = pool
        .launch(&old, stale, MainReads::Walled)
        .expect_err("the wall read again ends the pin");
    assert!(!pool.pinned(stale), "the read ended the pin");
    assert_eq!(again, first, "the same honest answer, never `work`");
    assert_eq!(
        pool.launch(&old, stale + 1, MainReads::Walled),
        Err(first),
        "and again, while the wall stands"
    );
}

/// A SIGNED-OUT DEFAULT ROW ON SUCH A PROGRAM NAMES CODEX'S OWN SIGN-IN IN THE DEFAULT HOME
/// (`aterm ai login codex`), never the pool's staged sign-in of it (which would move it onto a
/// login of its own that build never runs) nor a pin of it (which the pool refuses for a
/// signed-out row), however usable `work` is.
#[test]
fn a_signed_out_default_on_a_program_that_ignores_codex_auth_path_names_its_sign_in() {
    use aterm_agent::harness::accounts::{self, Cli};
    let pool = CodexPool::new();
    let old = aterm_agent::harness::codex_alab::lay_package(&pool.root.join("store/old"), false);
    pool.seat(POOL_NOW)
        .store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, POOL_NOW, &mut None, |s| {
            Ok(accounts::note_auth(
                s,
                "main",
                accounts::Auth::SignedOut,
                POOL_NOW,
            ))
        })
        .expect("sign `main` out");
    let refused = pool
        .launch(&old, POOL_NOW + 10, MainReads::Unread)
        .expect_err("no Codex account runs here while `main` is signed out");
    assert!(refused.contains("`aterm ai login codex`"), "{refused}");
    assert!(!refused.contains("harness accounts login"), "{refused}");
    assert!(!refused.contains("accounts use"), "{refused}");
    assert!(
        pool.pin("main", POOL_NOW + 11).is_err(),
        "the pin is refused"
    );

    // No login file in the default home is no login of `main` here either, whatever the
    // records say: the same sign-in, never a start on a home with nothing to sign in with.
    pool.seat(POOL_NOW + 12)
        .store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, POOL_NOW + 12, &mut None, |s| {
            Ok(accounts::note_auth(
                s,
                "main",
                accounts::Auth::Ok,
                POOL_NOW + 12,
            ))
        })
        .expect("clear `main`'s sign-out");
    std::fs::remove_file(pool.home.join(".codex/auth.json")).expect("drop the default login");
    let missing = pool
        .launch(&old, POOL_NOW + 13, MainReads::Empty)
        .expect_err("no Codex account runs here while `main` has no login");
    assert!(missing.contains("`aterm ai login codex`"), "{missing}");
    assert!(!missing.contains("harness accounts login"), "{missing}");
    assert!(!missing.contains("accounts use"), "{missing}");
}

/// ON SUCH A PROGRAM A SPAWN REFUSED FOR THE DEFAULT ROW'S WALL IS HELD UNTIL THAT WALL ENDS, NOT
/// STALLED AFTER ITS RETRIES: `work` is usable, but that build never runs it, so the pool's
/// opening for the worker's starter ([`super::pool_opening`], the one `pooled_starter_held`
/// waits on) is `main`'s (the instant the refusal names), and the launch starts on `main`, the
/// default login, once that instant passes.
#[test]
fn a_spawn_refused_for_a_walled_default_on_a_program_that_ignores_codex_auth_path_is_held() {
    use aterm_agent::harness::usage::rfc3339_utc;
    use std::cell::Cell;
    let pool = CodexPool::new();
    let old = aterm_agent::harness::codex_alab::lay_package(&pool.root.join("store/old"), false);
    let starter = aterm_agent::harness::worker::Starter {
        exe: old.clone(),
        env: Vec::new(),
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    let codex = aterm_agent::harness::worker::worker_cli(Program::Codex).expect("codex row");
    let clock = Cell::new(POOL_NOW);
    let refusals = RefCell::new(Vec::new());
    let waits = RefCell::new(Vec::new());
    let got = pool_attempts(
        || {
            pool.launch(&old, clock.get(), MainReads::Walled)
                .inspect_err(|why| refusals.borrow_mut().push(why.clone()))
        },
        || super::pool_opening(&pool.seat(clock.get()), &starter, codex),
        || Some(clock.get()),
        |wait| {
            waits.borrow_mut().push(wait.clone());
            if let PoolWait::Hold { until, .. } = wait {
                clock.set(clock.get().max(*until) + 1);
            }
            waits.borrow().len() < 16
        },
    );
    let waits = waits.take();
    let [PoolWait::Hold { until, .. }] = waits.as_slice() else {
        panic!("one hold, no retries: {waits:?}");
    };
    assert!(*until >= POOL_RESET, "held until `main`'s reset: {until}");
    let refusals = refusals.take();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(
        refusals[0].contains(&format!("until {}", rfc3339_utc(*until))),
        "the hold ends when the refusal says: {}",
        refusals[0]
    );
    let started = got.expect("the launch starts once `main` serves");
    assert_eq!(auth_path_of(&started), None, "on `main`: {:?}", started.env);
}

/// THE OWNER'S PIN ON A ROW OF ITS OWN IS JUDGED ON THE USAGE THE LAUNCH READS, NOT ON THE
/// RECORDS BEFORE IT: with `work` pinned and nothing yet on record for `main`, the read shows
/// `main` walled, so the refusal names that wall, and since the pin on `work` refuses every
/// launch after it too, it names that pin's end and offers the pin of `main` for once that wall
/// ends (an earlier one the next read of the wall would end).
#[test]
fn a_pin_on_a_row_of_its_own_on_a_program_that_ignores_codex_auth_path_reads_the_default_first() {
    use aterm_agent::harness::accounts::Cli;
    use aterm_agent::harness::codex_alab::{DefaultRow, UNPIN_FIX, unread_fix};
    use aterm_agent::harness::usage::rfc3339_utc;
    let pool = CodexPool::new();
    let old = aterm_agent::harness::codex_alab::lay_package(&pool.root.join("store/old"), false);
    pool.pin("work", POOL_NOW).expect("the owner pins `work`");
    let refused = pool
        .launch(&old, POOL_NOW + 1, MainReads::Walled)
        .expect_err("a pin on a row of its own does not run on an older build");
    assert!(refused.contains("five_hour"), "{refused}");
    let Ok(DefaultRow::Walled { until, .. }) =
        aterm_agent::harness::accounts_pool::default_row_now(&pool.seat(POOL_NOW + 1), Cli::Codex)
    else {
        panic!("the read put `main`'s wall on record: {refused}");
    };
    let pin = unread_fix(Some("main"));
    assert!(
        refused.contains(&pin) && refused.contains(&rfc3339_utc(until)),
        "the pin of `main`, for once the wall ends: {refused}"
    );
    assert!(refused.contains(UNPIN_FIX), "the pin's end: {refused}");
}

/// CONTROL: A PROGRAM THAT KEEPS `CODEX_AUTH_PATH`'S LOGIN ITS OWN still rotates past the walled
/// default onto `work`. On one that does not, the owner's pin on `work` is refused, and since
/// `main` can serve, the answer is the pin of `main`.
#[test]
fn a_program_that_keeps_codex_auth_path_still_rotates_to_work() {
    let pool = CodexPool::new();
    let reads = aterm_agent::harness::codex_alab::lay_package(&pool.root.join("store/new"), true);
    let old = aterm_agent::harness::codex_alab::lay_package(&pool.root.join("store/old"), false);
    let started = pool
        .launch(&reads, POOL_NOW, MainReads::Walled)
        .expect("a codex-alab that keeps the row's login starts on it");
    assert_eq!(auth_path_of(&started), Some(pool.work_auth.as_str()));

    let later = POOL_RESET + 3600;
    pool.pin("work", later).expect("the owner pins `work`");
    let refused = pool
        .launch(&old, later + 1, MainReads::Empty)
        .expect_err("a pin on a row of its own does not run on an older build");
    assert!(
        refused.contains("aterm harness accounts use main --cli codex"),
        "{refused}"
    );
}

#[test]
fn a_relative_login_file_is_not_rotated_as_the_default_worker() {
    use aterm_agent::harness::accounts::{self, Cli};
    use aterm_agent::harness::accounts_pool::Seat;
    use aterm_agent::harness::worker::Starter;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir(&cfg).unwrap();
    let seat = Seat {
        config_dir: &cfg,
        state: &tmp.path().join("state"),
        home: tmp.path(),
        now: 1_792_700_000,
    };
    for cli in [Cli::Codex, Cli::Grok] {
        std::fs::write(
            cfg.join(cli.accounts().roster_file),
            "[[account]]\nlabel='main'\nclass='dedicated'\n",
        )
        .unwrap();
        let store = seat.store(cli);
        store
            .transact_within(&accounts::LOCK_LADDER, seat.now, &mut None, |state| {
                state.active = Some("main".into());
                state.pinned = true;
                Ok(Vec::new())
            })
            .unwrap();
        let files = [
            cfg.join(cli.accounts().roster_file),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
        ];
        for broken in [false, true] {
            if broken {
                std::fs::write(&files[0], "[[broken").unwrap();
                std::fs::write(store.state_file(), "not JSON").unwrap();
            }
            let before = files.each_ref().map(|path| std::fs::read(path).ok());
            for value in ["auth.json", "./auth.json", "nested/auth.json"] {
                let starter = Starter {
                    exe: PathBuf::from("/opt/fixture"),
                    env: vec![(cli.accounts().seam.var().into(), value.into())],
                    args: Ok(Vec::new()),
                    unset: Vec::new(),
                };
                let no_usage =
                    |_: Cli,
                     _: &Path,
                     _: Option<&str>,
                     _: i64|
                     -> Option<aterm_agent::harness::codex_app_login::Usage> {
                        panic!("an unbound file must not poll pool usage")
                    };
                let actual =
                    super::pooled_starter_in(starter.clone(), cli, &seat, &no_usage, 0).unwrap();
                assert_eq!(actual, starter, "{cli:?}: {value}, broken={broken}");
            }
            assert_eq!(
                files.each_ref().map(|path| std::fs::read(path).ok()),
                before
            );
        }
    }
}

#[test]
fn a_refused_pooled_launch_is_tried_again_and_held_while_every_row_is_at_its_limit() {
    use std::cell::Cell;
    // Another process's turn at the accounts lock passes: the launch is tried again.
    let tries = Cell::new(0);
    let waits = RefCell::new(Vec::new());
    let got = pool_attempts(
        || {
            tries.set(tries.get() + 1);
            if tries.get() < 3 {
                Err(aterm_agent::harness::accounts::LOCK_HELD.to_string())
            } else {
                Ok("started")
            }
        },
        || None,
        || Some(1_792_700_000),
        |wait| {
            waits.borrow_mut().push(wait.clone());
            true
        },
    );
    assert_eq!(got, Ok("started"));
    assert_eq!(
        waits.take(),
        [
            PoolWait::Retry(POOL_RETRY_PAUSE),
            PoolWait::Retry(POOL_RETRY_PAUSE * 2)
        ]
    );

    // Every row at its limit, the first opening in three days, and nothing changes: the launch
    // is held, never counted against the retries, until that row opens, looking at the pool
    // again at least every POOL_HOLD_LOOK.
    let start = 1_792_700_000_i64;
    let until = start + 3 * 86_400;
    let clock = Cell::new(start);
    let tries = Cell::new(0);
    let looks = RefCell::new(Vec::new());
    let walled = || clock.get() < until;
    let got = pool_attempts(
        || {
            tries.set(tries.get() + 1);
            if walled() {
                Err("codex account pool has no usable account".to_string())
            } else {
                Ok(clock.get())
            }
        },
        || walled().then_some(until),
        || Some(clock.get()),
        |wait| hold_one_look(wait, until, &clock, &looks),
    );
    assert_eq!(got, Ok(until), "started when the first row opened");
    assert_eq!(tries.get(), 2);
    assert!(looks.borrow().iter().all(|look| *look <= POOL_HOLD_LOOK));

    // CONTROL: a refusal that stands stalls the launch once the retries are spent, and a
    // session that ended stops a hold at once.
    let tries = Cell::new(0);
    let standing = pool_attempts(
        || -> Result<(), String> {
            tries.set(tries.get() + 1);
            Err("roster unreadable".to_string())
        },
        || None,
        || Some(start),
        |_| true,
    );
    assert_eq!(standing, Err("roster unreadable".to_string()));
    assert_eq!(tries.get(), 1 + POOL_RETRIES);
    let ended = pool_attempts(
        || -> Result<(), String> { Err("walled".to_string()) },
        || Some(until),
        || Some(start),
        |_| false,
    );
    assert_eq!(ended, Err("walled".to_string()));
}

/// One look of a held launch on a test clock: the hold names `until`, and its wait passes `look`.
fn hold_one_look(
    wait: &PoolWait,
    until: i64,
    clock: &std::cell::Cell<i64>,
    looks: &RefCell<Vec<Duration>>,
) -> bool {
    let PoolWait::Hold {
        until: held, look, ..
    } = wait
    else {
        panic!("a held launch pauses for no retry: {wait:?}");
    };
    assert_eq!(*held, until);
    looks.borrow_mut().push(*look);
    clock.set(clock.get() + i64::try_from(look.as_secs()).expect("seconds"));
    true
}

#[test]
fn a_held_pooled_launch_starts_once_a_row_frees_partway_through_its_hold() {
    use std::cell::Cell;
    // Every row is at a weekly wall that ends in three days; two and a half minutes in, the
    // owner signs in another row (or pins one, or switches rotation off), and the pool serves.
    let start = 1_792_700_000_i64;
    let until = start + 3 * 86_400;
    let freed = start + 150;
    let clock = Cell::new(start);
    let tries = Cell::new(0);
    let looks = RefCell::new(Vec::new());
    let serves = || clock.get() >= freed;
    let got = pool_attempts(
        || {
            tries.set(tries.get() + 1);
            if serves() {
                Ok(clock.get())
            } else {
                Err("codex account pool has no usable account".to_string())
            }
        },
        || (!serves()).then_some(until),
        || Some(clock.get()),
        |wait| hold_one_look(wait, until, &clock, &looks),
    );
    let started = got.expect("the launch starts");
    let look = i64::try_from(POOL_HOLD_LOOK.as_secs()).expect("seconds");
    assert!(
        (freed..freed + look).contains(&started),
        "started at {started}: the row served from {freed}, the wall ends at {until}"
    );
    assert_eq!(tries.get(), 2, "tried at the refusal and once a row served");
}
