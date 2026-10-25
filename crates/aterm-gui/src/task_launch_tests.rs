// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_phase::Program;

#[test]
fn a_task_frame_is_at_most_64_kib() {
    assert_eq!(frame_len("0", "claude"), Ok(0));
    assert_eq!(frame_len("65536", "claude"), Ok(TASK_MAX));
    assert_eq!(
        frame_len("65537", "claude"),
        Err("ERR spawn agent=claude: the task is 65537 bytes (max 65536)\n".to_string())
    );
    assert_eq!(
        frame_len("65537", worker_word(Some("gemini"))),
        Err("ERR spawn agent=gemini: the task is 65537 bytes (max 65536)\n".to_string())
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
    for word in ["claude", "codex", "gemini", "agy", "muse"] {
        assert_eq!(worker_word(Some(word)), word);
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
        aterm_phase::Program::Gemini,
        Some("opus".into()),
        Some(&root),
        Some(from.clone()),
    )
    .expect("an order");
    assert_eq!(order.task, task);
    assert_eq!(order.agent, aterm_phase::Program::Gemini);
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
    let gemini = fact_line("s-ab12", Autopilot::Land, "gemini");
    assert!(gemini.contains("`spawn agent=gemini`"), "{gemini}");
    assert!(!gemini.contains("agent=claude"), "CONTROL: {gemini}");
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
        task_text(b"do it", Some("ctl"), Autopilot::Land, Program::Gemini),
        format!("do it\n\n{}\n", fact_line("ctl", Autopilot::Land, "gemini")).into_bytes()
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
    for agent in [
        Program::Claude,
        Program::Codex,
        Program::Gemini,
        Program::Muse,
    ] {
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
    for agent in [
        Program::Claude,
        Program::Codex,
        Program::Gemini,
        Program::Muse,
    ] {
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
    let seed = TaskSeed::new(b"the task\n".to_vec()).with_trust_file("/w/proj");
    let path = seed.write_in(&dir, &sid).expect("written");
    assert_eq!(path, dir.join("s-0123abcd-1.txt"));
    assert_eq!(std::fs::read(&path).unwrap(), b"the task\n");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&dir), mode(&path)), (0o700, 0o600));
    let trust = seed.trust_file().expect("its sidecar was written with it");
    assert_eq!(mode(&trust), 0o600);
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
    assert!(!trust.exists(), "the same discard owns its sidecar");
    seed.discard();

    let cancelled = TaskSeed::new(b"never started".to_vec()).with_trust_file("/w/late");
    let late = cancelled.clone();
    cancelled.discard();
    let sid = SessionId::new("s-0001");
    assert_eq!(
        late.write_in(&dir, &sid).unwrap_err().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(!dir.join(task_file_name(&sid)).exists());
    assert!(!dir.join(trust_file_name(&sid)).exists());

    let racing = TaskSeed::new(b"cancelled during write".to_vec()).with_trust_file("/w/racing");
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
    assert!(
        !dir.join(trust_file_name(&sid)).exists(),
        "nor can its late sidecar outlive cancellation"
    );
    assert!(racing.write_in(&dir, &sid).is_err(), "discard stays sticky");

    let running = TaskSeed::new(b"still needed".to_vec()).with_trust_file("/w/running");
    let path = running.write_in(&dir, &SessionId::new("s-0003")).unwrap();
    let trust = running.trust_file().expect("running sidecar");
    drop(running);
    assert_eq!(std::fs::read(&path).unwrap(), b"still needed");
    assert!(
        trust.exists(),
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

/// A GEMINI CLI WORKER'S TRUSTED-FOLDERS FILE IS ITS OWN AND PRIVATE: 0600
/// in a 0700 directory, under its sid, never written over, trusting its one
/// folder (`aterm_agent::harness::worker::trusted_folders_text`).
#[cfg(unix)]
#[test]
fn a_gemini_workers_trust_file_is_private_and_goes_with_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-trust-file")
        .tempdir()
        .expect("scratch dir");
    let dir = scratch.path().join("tasks");
    let (gone, kept) = (SessionId::new("s-0d"), SessionId::new("s-0e"));
    let seed = TaskSeed::new(b"task".to_vec()).with_trust_file("/w/proj");
    seed.write_in(&dir, &gone).expect("written");
    let path = seed.trust_file().expect("sidecar");
    assert_eq!(path, dir.join("s-0d-1.trust.json"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        aterm_agent::harness::worker::trusted_folders_text("/w/proj")
    );
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&dir), mode(&path)), (0o700, 0o600));
    assert!(
        TaskSeed::new(b"other".to_vec())
            .with_trust_file("/")
            .write_in(&dir, &gone)
            .is_err(),
        "never over one"
    );
    assert!(
        std::fs::read_to_string(&path).unwrap().contains("/w/proj"),
        "kept"
    );
    let other = TaskSeed::new(b"other task".to_vec()).with_trust_file("/w/other");
    other.write_in(&dir, &kept).expect("written");
    let other = other.trust_file().expect("other sidecar");

    let occupied = SessionId::new("s-0f");
    let sentinel = dir.join(trust_file_name(&occupied));
    std::fs::write(&sentinel, "existing").unwrap();
    let blocked = TaskSeed::new(b"untrusted task".to_vec()).with_trust_file("/");
    let task = blocked
        .write_in(&dir, &occupied)
        .expect("task still starts");
    assert_eq!(blocked.trust_file(), None, "no owned sidecar was written");
    blocked.discard();
    assert!(!task.exists());
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "existing");
    retire_task_file_in(&dir, &gone);
    assert!(!path.exists(), "removed with its worker");
    assert!(other.exists(), "another worker's stays");
}

#[test]
fn a_starter_not_named_yet_is_read_from_its_terminal() {
    use aterm_phase::Program;
    let unread = || -> Option<i32> { panic!("the front group is not read") };
    for agent in [
        Program::Claude,
        Program::Codex,
        Program::Gemini,
        Program::Agy,
        Program::Muse,
    ] {
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
    use aterm_agent::harness::worker::{Fresh, Starter, line, worker_cli};
    let words = |list: &[&str]| list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let home = Path::new("/Users//u");
    let codex = Starter {
        exe: PathBuf::from("/opt/twin/codex"),
        env: Vec::new(),
        args: Ok(words(&["-c", "x=1"])),
        unset: Vec::new(),
    };
    let pick = LoginEnv {
        label: "work".to_string(),
        var: "CODEX_HOME",
        value: Some("/c/work".to_string()),
    };
    let onto = pool_onto(codex, Cli::Codex, &pick, home).expect("codex row");
    let cli = worker_cli(Program::Codex).expect("codex worker row");
    assert_eq!(
        line(cli, Dialect::Zsh, &onto, &Fresh::default()).expect("codex line"),
        concat!(
            " '/usr/bin/env' -u 'CODEX_API_KEY' -u 'CODEX_ACCESS_TOKEN' 'CODEX_HOME=/c/work'",
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
    assert_eq!(onto.unset, ["GROK_AUTH"]);
    assert!(onto.env.is_empty(), "{:?}", onto.env);
    assert!(
        pool_onto(grok(&[("GROK_HOME", "rel")]), Cli::Grok, &work, home).is_err(),
        "a relative GROK_HOME"
    );
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
    for dir in [&c1, &c2] {
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
    let no_usage = |_: Cli, _: &Path, _: i64| -> Option<Usage> { None };

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
        exe: PathBuf::from("/opt/twin/codex"),
        env: pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    let c1_str = c1.to_string_lossy().into_owned();
    let c2_str = c2.to_string_lossy().into_owned();

    // 1. Manager running on pooled row `codex-1` spawns a child worker -> rotates to `codex-2`
    //    with a single replaced `CODEX_HOME` entry.
    let rotated = super::pooled_starter_in(
        starter_on(&[("CODEX_HOME", &c1_str)]),
        Cli::Codex,
        &seat,
        &no_usage,
        1,
    )
    .expect("pooled launch");
    assert_eq!(rotated.env, vec![("CODEX_HOME".to_string(), c2_str)]);

    // 2. When `main` (default home, `dir = None`) is pinned, a starter carrying `codex-1` drops
    //    `CODEX_HOME` completely so the child worker runs in the default home.
    seat.store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |s| {
            s.active = Some("main".to_string());
            s.pinned = true;
            s.proven = true;
            Ok(Vec::new())
        })
        .expect("pin main");
    let to_main = super::pooled_starter_in(
        starter_on(&[("CODEX_HOME", &c1_str)]),
        Cli::Codex,
        &seat,
        &no_usage,
        2,
    )
    .expect("pinned launch");
    assert!(to_main.env.is_empty(), "{:?}", to_main.env);

    // 3. CONTROL: a custom scratch `CODEX_HOME` outside the bound roster, a non-default CLI home
    //    (`GROK_HOME`), or an explicit credential override (`CODEX_API_KEY`, with or without
    //    `CODEX_HOME`) is left untouched without reading usage.
    let panic_read =
        |_: Cli, _: &Path, _: i64| -> Option<Usage> { panic!("custom home must not read usage") };
    let custom_dir = root.join("custom-scratch-codex").display().to_string();
    let custom = starter_on(&[("CODEX_HOME", &custom_dir)]);
    assert_eq!(
        super::pooled_starter_in(custom.clone(), Cli::Codex, &seat, &panic_read, 3),
        Ok(custom)
    );
    let overridden = starter_on(&[("CODEX_HOME", &c1_str), ("CODEX_API_KEY", "sk-override")]);
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
    let outside = starter_on(&[("CODEX_HOME", &custom_dir)]);
    assert_eq!(
        super::pooled_starter_in(outside.clone(), Cli::Codex, &seat, &panic_read, 9),
        Ok(outside)
    );
    let orphan = seat.store(Cli::Codex).logins_dir().join("orphaned-renewal");
    let orphan_text = orphan.display().to_string();
    let owned = super::pooled_starter_in(
        starter_on(&[("CODEX_HOME", &orphan_text)]),
        Cli::Codex,
        &seat,
        &panic_read,
        10,
    );
    assert!(owned.unwrap_err().contains("no current account binding"));
    std::fs::write(seat.store(Cli::Codex).logins_file(), "not JSON").unwrap();
    assert!(
        super::pooled_starter_in(
            starter_on(&[("CODEX_HOME", &c1_str)]),
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
