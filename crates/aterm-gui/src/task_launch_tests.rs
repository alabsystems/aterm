// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_phase::Program;

#[path = "account_worker_env_tests.rs"]
mod account_worker_env;

#[test]
fn a_task_frame_is_at_most_64_kib() {
    let claude = spawn_form_word(None, None);
    assert_eq!(
        claude, "agent=claude",
        "a spawn naming no agent is Claude Code's"
    );
    assert_eq!(frame_len("0", &claude), Ok(0));
    assert_eq!(frame_len("65536", &claude), Ok(TASK_MAX));
    assert_eq!(
        frame_len("65537", &claude),
        Err("ERR spawn agent=claude: the task is 65537 bytes (max 65536)\n".to_string())
    );
    assert_eq!(
        frame_len("65537", &spawn_form_word(Some("muse"), None)),
        Err("ERR spawn agent=muse: the task is 65537 bytes (max 65536)\n".to_string())
    );
    assert_eq!(
        frame_len("65537", &spawn_form_word(None, Some("/opt/my%20run"))),
        Err("ERR spawn program=/opt/my%20run: the task is 65537 bytes (max 65536)\n".to_string()),
        "a program worker's names its program as the wire spells it"
    );
    assert_eq!(program_form("/opt/my run"), "program=/opt/my%20run");
    assert!(
        frame_len("99999999999999999999999", &claude)
            .is_err_and(|e| e.contains("bytes (max 65536)"))
    );
    for bad in ["", "-1", "1k", "0x10"] {
        assert!(
            frame_len(bad, &claude).is_err_and(
                |e| e.starts_with("ERR usage: spawn") && e.contains(&format!("`len={bad}`"))
            ),
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
    let run = crate::program_worker::ProgramRun {
        program: "/bin/sh".to_string(),
        args: Vec::new(),
    };
    assert_eq!(
        TaskOrder::of_program_frame(Some(Vec::new()), run.clone(), None, None).unwrap_err(),
        "ERR spawn program=/bin/sh: the task is empty\n",
        "a program worker's refusal names its program"
    );
    let program =
        TaskOrder::of_program_frame(Some(b"echo hi\n\nexit 3".to_vec()), run.clone(), None, None)
            .expect("an order");
    assert_eq!(
        (program.agent, program.program, program.fact_line),
        (aterm_phase::Program::Generic, Some(run), false),
        "no fact line: the task is the program's input"
    );
    assert_eq!(
        task_text(&program.task, None, program.autopilot, program.agent),
        b"echo hi\n\nexit 3\n",
        "its task as given, blank lines and all, its last line ended"
    );
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
    let fact = fact_line("s-ab12", Autopilot::Land, Program::Claude);
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
    assert!(
        fact.contains(aterm_primer::LAND_CHECK_ANSWER),
        "and on the land check's answer, the primer's words: {fact}"
    );
    assert!(fact.contains("`NEEDS YOU:`"), "a need goes on: {fact}");
    assert!(fact.contains("`spawn agent=claude`"), "{fact}");
    let muse = fact_line("s-ab12", Autopilot::Land, Program::Muse);
    assert!(muse.contains("`spawn agent=muse`"), "{muse}");
    assert!(!muse.contains("agent=claude"), "CONTROL: {muse}");
    // THE CHECK ONLY WHERE ATERM TYPES IT (the post-landing review, 2026-11-01): codex-alab
    // lands under its own supervision, and nothing types it aterm's land check.
    for agent in [Program::Codex, Program::Grok] {
        let theirs = fact_line("s-ab12", Autopilot::Land, agent);
        assert!(
            theirs.contains("`Nothing left:`") && !theirs.contains(aterm_primer::LAND_CHECK_ANSWER),
            "{agent:?}: no land check promised: {theirs}"
        );
    }
    for agent in [Program::Agy, Program::Muse] {
        let carried = fact_line("s-ab12", Autopilot::Land, agent);
        assert!(
            carried.contains(aterm_primer::LAND_CHECK_ANSWER),
            "CONTROL: {agent:?}: {carried}"
        );
    }
    let cruise = fact_line("s-ab12", Autopilot::Cruise, Program::Claude);
    assert!(cruise.contains("Autopilot Cruise"), "{cruise}");
    assert!(
        !cruise.contains("Nothing left:") && !cruise.contains(aterm_primer::LAND_CHECK_ANSWER),
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
        format!(
            "do it\n\n{}\n",
            fact_line("ctl", Autopilot::Land, Program::Muse)
        )
        .into_bytes()
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
            fact_line("ctl", Autopilot::Cruise, Program::Claude)
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
        format!(
            "{folded}{}\n",
            fact_line("s-ab12", Autopilot::Land, Program::Agy)
        )
    );
    assert_eq!(text(task.as_bytes(), None, Program::Agy), folded);
    for agent in [Program::Claude, Program::Codex, Program::Muse] {
        let fact = fact_line("s-ab12", Autopilot::Land, agent);
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
    let drawn: Vec<&str> = first[user..user + 5]
        .iter()
        .map(|r| r.trim_start_matches('>').trim())
        .filter(|r| !r.is_empty())
        .collect();
    let drawn = drawn.join(" ");
    // The screen was measured before the fact line named the land check (slice 13a): it drew the
    // task, then that older line, which opens as today's does.
    let fact = fact_line("s-146719f2750f4a8f6909", Autopilot::Land, Program::Agy);
    let mode = "under Autopilot Land:";
    let opens = &fact[..fact.find(mode).expect("the mode") + mode.len()];
    assert!(
        drawn.starts_with(&format!(
            "{} {opens}",
            std::str::from_utf8(task).expect("utf-8")
        )),
        "{drawn}"
    );
    let now = text(task, Some("s-146719f2750f4a8f6909"), Program::Agy);
    assert!(!now.contains("\n\n"), "{now}");
    assert!(first[user + 1].is_empty(), "the measured blank row");
    let mut one = first.clone();
    one.remove(user + 1);
    assert_eq!(said_tail(&one).as_deref(), Some(reply));
    assert!(needs_you(reply).is_empty());
    assert_eq!(turn_opened_by(&one).as_deref(), Some(drawn.as_str()));
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
                args: cli.unnamed_posture(),
                unset: Vec::new(),
            },
            "a starter that runs no {}: its plain word, at its CLI's floor",
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
    retire_files_in(&dir, std::slice::from_ref(&gone), Retire::All);
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
    retire_files_in(&dir, std::slice::from_ref(&gone), Retire::All);
    assert!(!path.exists(), "removed");
    for r in &results {
        assert!(!r.exists(), "{} removed", r.display());
    }
    assert!(other.exists(), "another session's stays");
    for o in &others {
        assert!(o.exists(), "{} stays", o.display());
    }
    retire_files_in(&dir, std::slice::from_ref(&gone), Retire::All);
    retire_files_in(
        &scratch.path().join("absent"),
        std::slice::from_ref(&gone),
        Retire::All,
    );
    assert!(other.exists());
}

/// A WORKER aterm CLOSED KEEPS ITS RESULTS WHILE ITS RETIRED ROW STANDS: its close removes its task
/// file alone, and its row's end removes the results, and only its own. CONTROL: a person's close
/// takes both at once (`a_closed_workers_task_files_are_removed_and_no_other`).
#[cfg(unix)]
#[test]
fn a_reaped_workers_results_outlive_its_task_file_until_its_row_goes() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-task-reaped")
        .tempdir()
        .expect("scratch dir");
    let dir = scratch.path().join("tasks");
    let gone = SessionId::new("s-0c");
    let path = TaskSeed::new(b"one".to_vec())
        .write_in(&dir, &gone)
        .expect("written");
    let results = [
        touch(&dir, "s-0c-3.result.md"),
        touch(&dir, "s-0c-3.result.src"),
    ];
    let other = touch(&dir, "s-0f-3.result.md");
    retire_files_in(&dir, std::slice::from_ref(&gone), Retire::TaskFile);
    assert!(!path.exists(), "its task file goes with its tab");
    assert!(results.iter().all(|r| r.exists()), "its results stay");
    retire_files_in(&dir, std::slice::from_ref(&gone), Retire::Results);
    assert!(
        results.iter().all(|r| !r.exists()),
        "its row went: so do they"
    );
    assert!(other.exists(), "another session's stays");
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
    // The row's socket in a grok home, joined as the host joins a path.
    let socket_in = |grok_home: &Path| {
        grok_home
            .join("leader-work-9ca20914.sock")
            .display()
            .to_string()
    };
    let onto = pool_onto(grok(&[]), Cli::Grok, &work, home).expect("grok row");
    assert_eq!(
        onto.args,
        Ok(words(&[
            "-m",
            "grok-4",
            "--leader-socket",
            socket_in(&home.join(".grok")).as_str()
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
    let grok_home = crate::test_host::abs("/g/home");
    let carried = pool_onto(
        grok(&[("GROK_HOME", grok_home.as_str())]),
        Cli::Grok,
        &work,
        home,
    )
    .expect("grok row in its starter's home");
    assert_eq!(
        carried.args,
        Ok(words(&[
            "-m",
            "grok-4",
            "--leader-socket",
            socket_in(Path::new(&grok_home)).as_str()
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
    let prefix = format!(
        "[packages]\nprefix = {}\n",
        crate::test_host::toml(&foreign)
    );
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

/// A DEV-LINKED PROGRAM IS ONE A WORKER CAN RUN, so its launch asks atpkg for nothing: atpkg's
/// install leaves a link as it stands and says so with success, and a launcher that looked only for
/// a build then refused a `codex` that runs (m111, 2026-11-06: `stalled why=install`). A link whose
/// tool is gone runs nothing, nor does one that exposes no tool of that name, since the line types
/// that word, so the launch asks atpkg and refuses on what that leaves. NEGATIVE CONTROL: a look at
/// the active build alone reads no link. CONTROL: a store with neither holds no such program, and
/// another program's link is no link of this one.
#[cfg(unix)]
#[test]
fn a_dev_linked_program_is_one_a_worker_can_run() {
    use aterm_agent::harness::worker::Readiness;
    use std::os::unix::fs::PermissionsExt as _;

    struct LinkStore<'a> {
        layout: &'a atpkg::store::Layout,
        installs: std::cell::Cell<u32>,
    }
    impl ProgramStore for LinkStore<'_> {
        fn present(&self, program: &str) -> bool {
            runnable_in(self.layout, program)
        }
        fn held(&self, _: &str) -> Option<String> {
            None
        }
        fn install(&self, _: &str) -> Result<(), String> {
            self.installs.set(self.installs.get() + 1);
            Ok(())
        }
    }

    let cli = aterm_agent::harness::worker::worker_cli(Program::Claude).unwrap();
    let program = cli.atpkg_program().unwrap();
    let d = aterm_tempfile::Builder::new()
        .prefix("aterm-spawn-dev-link")
        .tempdir()
        .unwrap();
    let layout = atpkg::store::Layout {
        prefix: d.path().join("pkg"),
    };
    std::fs::create_dir(&layout.prefix).unwrap();
    assert!(!runnable_in(&layout, program), "CONTROL: nothing is laid");

    let checkout = d.path().join("checkout");
    std::fs::create_dir_all(checkout.join("target/release")).unwrap();
    let relative = PathBuf::from("target/release").join(program);
    let tool = checkout.join(&relative);
    std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    atpkg::link(&layout, program, &checkout, &[relative]).unwrap();
    assert!(runnable_in(&layout, program));
    assert!(
        !runnable_in(&layout, "codex"),
        "CONTROL: another program's link is no link of this one"
    );

    let store = LinkStore {
        layout: &layout,
        installs: std::cell::Cell::new(0),
    };
    let admitted = admit_worker(cli, cli.plain(), None, &|_| {}, &|| (), &store).unwrap();
    assert_eq!(admitted.readiness(), &Readiness::Present);
    assert_eq!(store.installs.get(), 0, "a link needs no install");

    std::fs::remove_file(&tool).unwrap();
    assert!(!runnable_in(&layout, program), "its tool is gone");
    let refused = admit_worker(cli, cli.plain(), None, &|_| {}, &|| (), &store)
        .map(|_| ())
        .expect_err("atpkg's install left nothing to run");
    assert_eq!(store.installs.get(), 1, "it asked atpkg: {refused:?}");

    let other = d.path().join("other");
    std::fs::create_dir_all(other.join("target/release")).unwrap();
    let relative = PathBuf::from("target/release/not-the-word");
    let tool = other.join(&relative);
    std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    atpkg::link(&layout, "codex", &other, &[relative]).unwrap();
    assert!(
        !runnable_in(&layout, "codex"),
        "a live link of no tool the line can type"
    );
}

#[test]
fn a_spawn_installs_nothing_when_removal_evidence_is_unreadable() {
    use aterm_agent::harness::worker::Readiness;
    let cli = aterm_agent::harness::worker::worker_cli(Program::Claude).unwrap();
    let program = cli.atpkg_program().unwrap();
    let d = aterm_tempfile::Builder::new()
        .prefix("aterm-spawn-removals")
        .tempdir()
        .unwrap();
    let layout = atpkg::store::Layout {
        prefix: d.path().join("pkg"),
    };
    std::fs::create_dir(&layout.prefix).unwrap();
    std::fs::write(layout.removed(), [0xff]).unwrap();
    let store = ConsentStore {
        cfg: Default::default(),
        layout: &layout,
        installs: std::cell::Cell::new(0),
    };
    let admitted = admit_worker(cli, cli.plain(), None, &|_| {}, &|| (), &store).unwrap();
    assert_ne!(admitted.readiness(), &Readiness::Installed);
    assert!(
        admitted
            .kept_off()
            .is_some_and(|why| why.contains("removal decisions are unreadable"))
    );
    assert_eq!(store.installs.get(), 0);
    assert!(!store.present(program));
}

/// A LAUNCHER'S LOOK AGAIN AFTER A REDRAW REFUSED ITS LINE ([`await_prompt`] with
/// [`PROMPT_SETTLE`]) takes a prompt only once one screen generation has held for the settle, so a
/// prompt that keeps repainting gets no attempt between its repaints; its first look, with no
/// settle, takes the prompt it finds.
#[test]
fn a_prompt_is_taken_once_one_screen_generation_has_held_for_the_settle() {
    use crate::control::ScreenGen;
    let ms = Duration::from_millis;
    let (shown, redrawn) = (
        ScreenGen { epoch: 0, seq: 7 },
        ScreenGen { epoch: 0, seq: 8 },
    );
    let t0 = Instant::now();
    let mut held = Held::default();
    assert_eq!(held.look(shown, t0, PROMPT_SETTLE), PROMPT_SETTLE);
    let t1 = t0 + PROMPT_SETTLE - ms(1);
    assert_eq!(
        held.look(shown, t1, PROMPT_SETTLE),
        ms(1),
        "not yet held for the settle"
    );
    assert_eq!(
        held.look(redrawn, t1, PROMPT_SETTLE),
        PROMPT_SETTLE,
        "a redraw counts again from its look"
    );
    assert_eq!(
        held.look(redrawn, t1 + PROMPT_SETTLE, PROMPT_SETTLE),
        Duration::ZERO,
        "held for the settle"
    );
    assert_eq!(
        Held::default().look(shown, t0, Duration::ZERO),
        Duration::ZERO,
        "CONTROL: a first look takes the prompt it finds"
    );
}

/// A WORKER'S LINE IS TRIED AGAIN ONLY WHERE NOTHING WAS WRITTEN ([`launch_line_typed`]): the
/// fence's refusals, which it makes before a byte is written, and input queued before the line
/// that had not all gone (a report on Windows; third review of the turn-deadline work,
/// 2026-11-06), are [`Untyped::Unsettled`]; a line
/// written whole or in part is typed and never tried again, and a person's use or a write the
/// sink did not take (a short write among them) stalls the task: the first says a person came, the
/// second names a cause no shell or person made ([`Untyped::Blocked`]).
#[test]
fn only_a_press_that_wrote_nothing_is_tried_again() {
    use crate::control::GuardedInput;
    use crate::input::Delivery;
    for unsettled in [
        GuardedInput::Changed,
        GuardedInput::Skipped,
        GuardedInput::Queued,
    ] {
        assert_eq!(
            launch_line_typed(unsettled),
            Err(Untyped::Unsettled),
            "{unsettled:?}"
        );
    }
    for written in [Delivery::Full, Delivery::PartialInDoubt { accepted: 3 }] {
        assert!(
            launch_line_typed(GuardedInput::Pressed(written)).is_ok(),
            "{written:?} is typed, never typed again"
        );
    }
    assert!(
        matches!(
            launch_line_typed(GuardedInput::Person),
            Err(Untyped::Refused(_))
        ),
        "CONTROL: a person's use stalls the task"
    );
    for blocked in [
        GuardedInput::Pressed(Delivery::BusyZero),
        GuardedInput::Pressed(Delivery::Failed),
    ] {
        assert!(
            matches!(launch_line_typed(blocked), Err(Untyped::Blocked(_))),
            "CONTROL: {blocked:?} stalls the task, naming why"
        );
    }
}

/// A LAUNCHER'S INSTALLING MARK LIVES ON ITS SESSION'S TIMELINE WHILE IT STANDS, AND GOES WHEN
/// IT DROPS: the startup deadline reads it to judge an install by its work, not by its own 120 s.
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

/// A LAUNCHER'S INSTALL NAMES ITS CHILD ON THE TIMELINE: the process whose pass the startup
/// deadline reads as this worker's work, which is the pid a stand-in atpkg sees as its own under
/// the very spawn and QoS wrapper an install runs, since the file's `pid` is atpkg's own. It goes
/// with the mark. CONTROL: a mark names no child before its install spawns one.
#[cfg(unix)]
#[test]
fn a_launchers_install_names_its_child_on_the_timeline() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-gui-install-child-")
        .tempdir()
        .unwrap();
    let said = dir.path().join("pid");
    let stub = dir.path().join("atpkg-fake");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\necho $$ > '{}'\n", said.display()),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    aterm_uds::deadline::output_until(
        &mut crate::qos::atpkg_command(&stub),
        Instant::now() + aterm_spec::wait::FIRST_EXEC,
    )
    .unwrap_or_else(|e| panic!("the stub's first exec within its hang detector: {e}"));
    let layout = atpkg::store::Layout {
        prefix: dir.path().to_path_buf(),
    };
    let h = crate::stub_session(1);
    let child = || h.ctx.timeline.lock().unwrap().worker_install_child();
    {
        let _mark = InstallingMark::of(&h.ctx);
        assert_eq!(child(), None, "CONTROL: nothing spawned yet");
        install_with(&stub, &layout, "codex", &h.ctx).expect("the stub exits clean");
        let pid = std::fs::read_to_string(&said).expect("the stub ran");
        assert_eq!(child(), Some(pid.trim().parse().expect("a pid")));
    }
    assert_eq!(child(), None, "gone with its mark");
}

#[test]
fn a_pool_hold_mark_stands_on_the_timeline_while_it_lives_and_is_no_install() {
    let h = crate::stub_session(1);
    let hold = || h.ctx.timeline.lock().unwrap().worker_pool_hold().cloned();
    let ended = || h.ctx.timeline.lock().unwrap().worker_pool_hold_ended();
    assert_eq!((hold(), ended()), (None, None), "PRECONDITION: no hold");
    {
        let mark = PoolHoldMark::of(&h.ctx);
        assert_eq!(hold(), None, "nothing is held until it says what for");
        mark.until(1_794_170_393, "no account of its pool can take it now");
        assert_eq!(hold().map(|hold| hold.until), Some(1_794_170_393));
        assert!(
            !h.ctx.timeline.lock().unwrap().worker_installing(),
            "the deadline waits out an install, and judges a hold"
        );
        assert_eq!(ended(), None, "it stands");
    }
    assert_eq!(hold(), None, "dropped with its hold");
    assert!(ended().is_some(), "when it ended is kept");
}

/// A CLAUDE CODE WORKER SPAWNED FROM A WALLED TAB starts on the row a wall would move it to: the
/// starter on the default login (`main`, walled) gets `work`'s login seam; a fresh starter's row,
/// `[harness] accounts = "off"`, and a starter on a home of its own keep the starter as it is.
/// The configured mode reaches the pick: a low `main` moves its worker under the default
/// `"early"` and keeps it under `"wall"` (`accounts::worker_row`'s own test holds the pin, the
/// canary and the bound). NEGATIVE CONTROL: before, every Claude Code worker kept its starter's
/// login, so a worker of a Limited tab started on the same wall; and the pick that read no mode
/// moved a low tab's worker under `"wall"` too.
#[test]
fn a_claude_worker_of_a_walled_tab_starts_on_the_row_a_wall_moves_to() {
    use aterm_agent::harness::accounts::{self, Cli};
    use aterm_agent::harness::accounts_pool::Seat;
    use aterm_agent::harness::worker::Starter;

    const NOW: i64 = 1_792_700_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let (cfg, state, home) = (root.join("cfg"), root.join("state"), root.join("home"));
    for dir in [&cfg, &state, &home] {
        std::fs::create_dir_all(dir).expect("dir");
    }
    let work = root.join("logins/claude/work");
    std::fs::create_dir_all(&work).expect("login dir");
    std::fs::write(work.join(".claude.json"), "{}").expect("login");
    std::fs::write(
        cfg.join(accounts::ACCOUNTS_FILE),
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
    let starter_on = |pairs: &[(&str, &str)]| Starter {
        exe: PathBuf::from("/opt/claude"),
        env: pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    let plain = starter_on(&[]);
    assert_eq!(
        super::claude_starter_in(plain.clone(), &seat, 1),
        plain,
        "CONTROL: a fresh row keeps the starter's login"
    );

    let mut note = None;
    seat.store(Cli::Claude)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |s| {
            Ok(accounts::note_wall(
                s,
                "main",
                "five_hour",
                Some(NOW + 3600),
                NOW,
                NOW,
            ))
        })
        .expect("wall main");
    let var = Cli::Claude.accounts().seam.var();
    let moved = super::claude_starter_in(plain.clone(), &seat, 2);
    assert_eq!(
        moved.env,
        vec![(var.to_string(), work.display().to_string())],
        "the walled default login's worker starts on `work`"
    );

    let custom = starter_on(&[("CLAUDE_CONFIG_DIR", "/elsewhere/.claude")]);
    assert_eq!(
        super::claude_starter_in(custom.clone(), &seat, 3),
        custom,
        "a home of its own is left alone"
    );
    seat.store(Cli::Claude)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |s| {
            s.accounts.remove("main");
            Ok(accounts::note_sample(
                s,
                "main",
                accounts::Sample {
                    session_pct: Some(95),
                    session_reset: Some(NOW + 3 * 3600),
                    at: NOW - 60,
                    ..accounts::Sample::default()
                },
                NOW,
            ))
        })
        .expect("main low");
    assert_eq!(
        super::claude_starter_in(plain.clone(), &seat, 5).env,
        vec![(var.to_string(), work.display().to_string())],
        "\"early\" leaves a low row ahead of its wall"
    );
    std::fs::write(cfg.join("aterm.toml"), "[harness]\naccounts = \"wall\"\n").expect("cfg");
    assert_eq!(
        super::claude_starter_in(plain.clone(), &seat, 6),
        plain,
        "under \"wall\" a row that still runs keeps its worker"
    );
    std::fs::write(cfg.join("aterm.toml"), "[harness]\naccounts = \"off\"\n").expect("cfg");
    assert_eq!(
        super::claude_starter_in(plain.clone(), &seat, 4),
        plain,
        "`[harness] accounts = \"off\"` keeps it"
    );
}

/// A CLAIM WORKER'S WALL IS THE ROW IT WOULD START ON ([`super::claude_worker_wall`]): the
/// default login (`main`) at a usage limit with no row to move to reads its `until`, and so
/// does a roster whose other row the accounts are told not to move to (`accounts = "off"`).
/// NEGATIVE CONTROLS: a fresh `main` reads no wall, and a wall the launch moves the worker past
/// (to `work`) reads none either, as [`super::claude_starter_in`] would start it there.
#[test]
fn a_claim_workers_wall_is_the_wall_of_the_row_it_would_start_on() {
    use aterm_agent::harness::accounts::{self, Cli};
    use aterm_agent::harness::accounts_pool::Seat;
    use aterm_agent::harness::worker::Starter;

    const NOW: i64 = 1_792_700_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let (cfg, state, home) = (root.join("cfg"), root.join("state"), root.join("home"));
    for dir in [&cfg, &state, &home] {
        std::fs::create_dir_all(dir).expect("dir");
    }
    std::fs::write(
        cfg.join(accounts::ACCOUNTS_FILE),
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
    )
    .expect("roster");
    let seat = Seat {
        config_dir: &cfg,
        state: &state,
        home: &home,
        now: NOW,
    };
    let plain = Starter {
        exe: PathBuf::from("/opt/claude"),
        env: Vec::new(),
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    assert_eq!(
        super::claude_worker_wall(&plain, &seat),
        None,
        "CONTROL: a fresh login runs"
    );

    let until = NOW + 3600;
    let mut note = None;
    seat.store(Cli::Claude)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |s| {
            Ok(accounts::note_wall(
                s,
                "main",
                "five_hour",
                Some(until),
                NOW,
                NOW,
            ))
        })
        .expect("wall main");
    assert_eq!(
        super::claude_worker_wall(&plain, &seat),
        Some(until + accounts::RESET_GRACE_S),
        "the one login is walled, to its reset and the grace after it"
    );

    let work = root.join("logins/claude/work");
    std::fs::create_dir_all(&work).expect("login dir");
    std::fs::write(work.join(".claude.json"), "{}").expect("login");
    std::fs::write(
        cfg.join(accounts::ACCOUNTS_FILE),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            work.display()
        ),
    )
    .expect("roster with work");
    assert_eq!(
        super::claude_worker_wall(&plain, &seat),
        None,
        "CONTROL: the launch moves the worker to `work`, which runs"
    );
    std::fs::write(cfg.join("aterm.toml"), "[harness]\naccounts = \"off\"\n").expect("cfg");
    assert_eq!(
        super::claude_worker_wall(&plain, &seat),
        Some(until + accounts::RESET_GRACE_S),
        "a launch that moves no worker starts it on the wall"
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
    // `main`, the default row, is signed in on this machine.
    for dir in [&c1, &c2, &home.join(".codex")] {
        std::fs::create_dir_all(dir).expect("login dir");
        std::fs::write(dir.join("auth.json"), "{}").expect("auth");
    }
    std::fs::write(
        cfg.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"codex-1\"\nclass = \"dedicated\"\ndir = {}\n\n\
             [[account]]\nlabel = \"codex-2\"\nclass = \"dedicated\"\ndir = {}\n",
            crate::test_host::toml(&c1),
            crate::test_host::toml(&c2)
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
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {}\n",
            crate::test_host::toml(&work)
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
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {}\n",
            crate::test_host::toml(&work)
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
    assert!(seat.keyring_held(Cli::Codex) && seat.mode_for(Cli::Codex) == accounts::Mode::Off);
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
                 [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {}\n",
                crate::test_host::toml(&work)
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
    use aterm_agent::harness::accounts_pool::{Reading, UsageObservation, UsageReader};
    let pool = CodexPool::new();
    let old = aterm_agent::harness::codex_alab::lay_package(&pool.root.join("store/old"), false);
    let native =
        aterm_agent::harness::codex_alab::lay_package(&pool.root.join("store/native"), true);
    let default = pool.home.join(".codex");
    std::fs::write(
        default.join("auth.json"),
        r#"{"tokens":{"access_token":"synthetic-rejected-token"}}"#,
    )
    .unwrap();
    struct Rejected<'a> {
        default: &'a Path,
        user_home: &'a Path,
    }
    impl UsageReader for Rejected<'_> {
        fn read(
            &self,
            _: Cli,
            at: &Path,
            _: Option<&str>,
            now: i64,
            use_token: aterm_agent::harness::provider_read::Use,
        ) -> Reading {
            if use_token
                .begin_home(
                    aterm_agent::harness::provider_read::Read::CodexEndpoint,
                    at,
                    now,
                )
                .is_err()
            {
                return Reading::Unread;
            }
            if at == self.default {
                Reading::SignedOut
            } else {
                Reading::Unread
            }
        }
        fn read_observed(
            &self,
            cli: Cli,
            at: &Path,
            expected: Option<&str>,
            now: i64,
            use_token: aterm_agent::harness::provider_read::Use,
        ) -> UsageObservation {
            UsageObservation::from_fixture(
                self.read(cli, at, expected, now, use_token),
                at,
                self.user_home,
            )
            .expect("owned fixture credentials")
        }
    }
    let reader = Rejected {
        default: &default,
        user_home: &pool.home,
    };
    // Seed through the real poll writer on a program that can use either row.
    let seeded = aterm_agent::harness::accounts_pool::launch_env_for(
        &pool.seat(POOL_NOW),
        Cli::Codex,
        &reader,
        Some(&native),
    )
    .expect("record the contextual provider rejection");
    assert_eq!(
        seeded.as_ref().map(|login| login.label.as_str()),
        Some("work")
    );
    let refused = pool
        .launch(&old, POOL_NOW + 10, MainReads::Unread)
        .expect_err("no Codex account runs here while `main` is signed out");
    assert!(refused.contains("`aterm ai login codex`"), "{refused}");
    assert!(!refused.contains("harness accounts login"), "{refused}");
    assert!(!refused.contains("accounts use"), "{refused}");
    let path = pool.cfg.join("accounts-codex.toml");
    let roster = accounts::Roster::load_for(Cli::Codex, &path).unwrap();
    assert!(
        pool.seat(POOL_NOW + 11)
            .store(Cli::Codex)
            .select_account_for(Cli::Codex, &roster, &path, "main", POOL_NOW + 11)
            .is_err(),
        "the rejected login cannot be pinned"
    );
    assert!(!pool.pinned(POOL_NOW + 11));

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
    use aterm_agent::harness::codex_alab::{DefaultRow, unpin_fix, unread_fix};
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
    assert!(refused.contains(&unpin_fix()), "the pin's end: {refused}");
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

#[test]
fn an_agy_worker_launch_is_held_while_every_model_pool_is_spent_and_starts_on_refill() {
    use aterm_agent::harness::agy_quota::{Pick, Rotation, Standing};
    use aterm_agent::harness::agy_quota_store::{Source, Store, launch_opening, rotate_launch};
    use std::cell::Cell;

    struct AgyClockSource<'a> {
        clock: &'a Cell<i64>,
        until: i64,
        reads: Cell<u32>,
    }

    impl Source for AgyClockSource<'_> {
        fn read(&self, _: &aterm_agent::harness::provider_read::Permit) -> Result<String, String> {
            self.reads.set(self.reads.get() + 1);
            let third_5h = if self.clock.get() >= self.until {
                1.0
            } else {
                0.0
            };
            Ok(format!(
                r#"{{"status":"SUCCESS","command":{{"name":"usage","data":{{"groups":[{{"name":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro","buckets":[{{"id":"gemini-weekly","window":"weekly","remaining_fraction":0,"reset_time":"2026-10-28T00:19:03Z"}}]}},{{"name":"Claude and GPT models","description":"Models within this group: Claude Opus, Claude Sonnet, GPT-OSS","buckets":[{{"id":"3p-weekly","window":"weekly","remaining_fraction":0.8,"reset_time":"2026-10-31T00:49:13Z"}},{{"id":"3p-5h","window":"5h","remaining_fraction":{third_5h},"reset_time":"2026-10-24T05:49:13Z"}}]}}]}}}}}}"#
            ))
        }

        fn default_model(
            &self,
            _: &aterm_agent::harness::provider_read::Permit,
        ) -> Result<String, String> {
            Ok("gemini-4-argon-high".to_owned())
        }
    }

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let store = Store::new(tmp.path(), "home");
    let rotation = Rotation::default();
    let until = aterm_types::rfc3339::parse_utc_fractional("2026-10-24T05:49:13Z").expect("reset");
    let start = until - 2_400;
    let clock = Cell::new(start);
    let source = AgyClockSource {
        clock: &clock,
        until,
        reads: Cell::new(0),
    };
    let looks = RefCell::new(Vec::new());
    let got = agy_pick_attempts(
        |now| rotate_launch(None, &store, &source, &rotation, now).map(|l| l.pick),
        |now| launch_opening(None, &store, &rotation, now),
        || Some(clock.get()),
        |wait| hold_one_look(wait, until, &clock, &looks),
    )
    .expect("the launch starts once the 5h Claude/GPT bucket refills");
    assert_eq!(
        got,
        Some(Pick::Rotate {
            from: "gemini-4-argon-high".to_owned(),
            from_standing: Standing::Spent,
            to: "claude-opus-5-5-medium".to_owned(),
            to_standing: Standing::Open,
        })
    );
    assert_eq!(clock.get(), until);
    assert_eq!(
        source.reads.get(),
        2,
        "agy is read once at the refusal and once when the hold reaches `until`"
    );

    // A refill whose server reading lags at `until` holds through `until + RESET_SETTLE_S` and
    // starts once the settled read sees the refilled bucket.
    let lag_store = Store::new(tmp.path(), "lag");
    let settled = until + aterm_agent::harness::agy_quota_store::RESET_SETTLE_S;
    clock.set(start);
    let lag_source = AgyClockSource {
        clock: &clock,
        until: settled,
        reads: Cell::new(0),
    };
    let got_lag = agy_pick_attempts(
        |now| rotate_launch(None, &lag_store, &lag_source, &rotation, now).map(|l| l.pick),
        |now| launch_opening(None, &lag_store, &rotation, now),
        || Some(clock.get()),
        |wait| {
            let target = if clock.get() < until { until } else { settled };
            hold_one_look(wait, target, &clock, &looks)
        },
    )
    .expect("the launch holds across RESET_SETTLE_S and starts once settled");
    assert_eq!(
        got_lag
            .and_then(|p| p.model().map(str::to_owned))
            .as_deref(),
        Some("claude-opus-5-5-medium")
    );
    assert_eq!(clock.get(), settled);
    assert_eq!(lag_source.reads.get(), 3);

    // CONTROL: a wait with no refill timestamp retries and stalls.
    let tries = Cell::new(0);
    let stalled = agy_pick_attempts(
        |_| {
            tries.set(tries.get() + 1);
            Some(Pick::Wait {
                model: "gemini-4-argon-high".to_owned(),
                until: None,
            })
        },
        |_| None,
        || Some(start),
        |wait| matches!(wait, PoolWait::Retry(_)),
    );
    assert!(
        stalled
            .as_ref()
            .is_err_and(|e| e.contains("waits: every candidate's pool is spent")),
        "{stalled:?}"
    );
    assert_eq!(tries.get(), 1 + POOL_RETRIES);
}

#[test]
fn native_google_worker_installs_before_preparing_its_guard() {
    use aterm_agent::harness::worker::{Readiness, Refused};
    let cli = aterm_agent::harness::worker::worker_cli(Program::Agy).unwrap();
    for (present, lays) in [(false, true), (true, true), (false, false)] {
        let store = FakeStore {
            present: std::cell::Cell::new(present),
            held: None,
            lays,
            asked: std::cell::RefCell::new(Vec::new()),
        };
        let prepared = std::cell::Cell::new(false);
        let alive = std::cell::Cell::new(false);
        struct Guard<'a>(&'a std::cell::Cell<bool>);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let mut expected = cli.plain();
        expected.exe = "/verified/native/agy".into();
        expected.env = vec![("HOME".into(), "/verified/native/home".into())];
        expected.args = Ok(vec!["--gemini_dir=/verified/native/home/.gemini".into()]);
        expected.unset = vec!["GEMINI_API_KEY"];
        let result = admit_prepared_worker(
            cli,
            cli.plain(),
            None,
            &|_| {},
            &|| (),
            &store,
            |admitted| {
                admitted.prepare(cli, None, |_| {
                    assert!(
                        store.present.get(),
                        "preparation ran before native installation"
                    );
                    store.asked.borrow_mut().push("prepare".into());
                    prepared.set(true);
                    alive.set(true);
                    Ok((expected.clone(), Guard(&alive)))
                })
            },
        );
        if !lays {
            assert!(matches!(result, Err(Refused::Program(_))));
            assert!(!prepared.get());
            continue;
        }
        let (admitted, guard) = result.unwrap();
        assert_eq!(admitted.starter(), &expected);
        assert_eq!(
            admitted.readiness(),
            if present {
                &Readiness::Present
            } else {
                &Readiness::Installed
            }
        );
        assert!(prepared.get() && alive.get());
        assert_eq!(
            *store.asked.borrow(),
            if present {
                vec!["present", "prepare"]
            } else {
                vec!["present", "held", "install agy", "present", "prepare"]
            }
        );
        drop(guard);
        assert!(!alive.get());
    }
}

/// An admission's reason atpkg keeps a program off this Mac reaches the session's timeline, where
/// the startup deadline reads it (`harness_host::kept_off_held`). CONTROL: a program that is there
/// clears what an earlier admission left.
#[test]
fn an_admissions_kept_off_reason_reaches_the_timeline() {
    let agy = aterm_agent::harness::worker::worker_cli(Program::Agy).unwrap();
    let h = crate::session_store::test_handle(3);
    let take = |store: &FakeStore| {
        take_admission(
            &h.ctx,
            3,
            Program::Agy,
            admit_prepared_worker(agy, agy.plain(), None, &|_| {}, &|| (), store, |starter| {
                Ok((starter, ()))
            }),
            || panic!("only a refusal reads the member"),
        )
        .expect("admitted");
    };
    let kept_off = || {
        let timeline = h.ctx.timeline.lock().unwrap();
        timeline.worker_kept_off().map(str::to_owned)
    };
    take(&FakeStore {
        present: std::cell::Cell::new(false),
        held: Some("explicit package hold"),
        lays: true,
        asked: std::cell::RefCell::new(Vec::new()),
    });
    assert_eq!(kept_off().as_deref(), Some("explicit package hold"));
    take(&FakeStore {
        present: std::cell::Cell::new(true),
        held: None,
        lays: true,
        asked: std::cell::RefCell::new(Vec::new()),
    });
    assert_eq!(kept_off(), None, "CONTROL: it is there");
}

/// A preparation that fails for any other reason is no sign-out: only a login a person must sign
/// back in reads `why=signin`, so a dispatcher that reads it asks for no sign-in that mends nothing.
#[test]
fn a_preparation_that_is_no_sign_out_does_not_read_as_one() {
    use aterm_agent::harness::worker::Refused;
    let agy = aterm_agent::harness::worker::worker_cli(Program::Agy).unwrap();
    let store = FakeStore {
        present: std::cell::Cell::new(true),
        held: None,
        lays: true,
        asked: std::cell::RefCell::new(Vec::new()),
    };
    let prepared = |refused: Refused| {
        admit_prepared_worker(agy, agy.plain(), None, &|_| {}, &|| (), &store, |_| {
            Err::<(_, ()), _>(refused)
        })
        .expect_err("its preparation refused")
    };

    let unavailable = prepared("Native Google pool location is unavailable".into());
    assert_eq!(
        unavailable,
        Refused::Preparation("Native Google pool location is unavailable".to_owned())
    );
    assert_eq!(refused_why(&unavailable), "startup");

    let signed_out = prepared(Refused::SignedOut("the login was refused".to_owned()));
    assert_eq!(
        refused_why(&signed_out),
        crate::task_record::WHY_SIGNIN,
        "CONTROL: a real sign-out"
    );
}

#[test]
fn prepared_worker_retains_holds_and_rechecks_replacement_sign_in() {
    use aterm_agent::harness::worker::Refused;
    let agy = aterm_agent::harness::worker::worker_cli(Program::Agy).unwrap();
    let held = FakeStore {
        present: std::cell::Cell::new(false),
        held: Some("explicit package hold"),
        lays: true,
        asked: std::cell::RefCell::new(Vec::new()),
    };
    let (admitted, ()) =
        admit_prepared_worker(agy, agy.plain(), None, &|_| {}, &|| (), &held, |admitted| {
            admitted.prepare(agy, None, |starter| Ok((starter, ())))
        })
        .unwrap();
    assert!(
        admitted
            .kept_off()
            .unwrap()
            .contains("explicit package hold")
    );
    assert_eq!(*held.asked.borrow(), ["present", "held"]);

    let grok = aterm_agent::harness::worker::worker_cli(Program::Grok).unwrap();
    let root = aterm_tempfile::tempdir().unwrap();
    let auth = root.path().join(".grok/auth.json");
    std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
    let store = FakeStore {
        present: std::cell::Cell::new(true),
        held: None,
        lays: true,
        asked: std::cell::RefCell::new(Vec::new()),
    };
    let result = admit_prepared_worker(
        grok,
        grok.plain(),
        Some(root.path()),
        &|_| {},
        &|| (),
        &store,
        |_| -> Result<(_, ()), Refused> { panic!("signed-out starter reached preparation") },
    );
    assert!(matches!(result, Err(Refused::SignedOut(_))));
    assert!(store.asked.borrow().is_empty());
    std::fs::write(&auth, r#"{"token":"synthetic"}"#).unwrap();
    let dropped = std::cell::Cell::new(false);
    struct Guard<'a>(&'a std::cell::Cell<bool>);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
    let result = admit_prepared_worker(
        grok,
        grok.plain(),
        Some(root.path()),
        &|_| {},
        &|| (),
        &store,
        |admitted| {
            admitted.prepare(grok, Some(root.path()), |starter| {
                std::fs::remove_file(&auth).unwrap();
                Ok((starter, Guard(&dropped)))
            })
        },
    );
    assert!(matches!(result, Err(Refused::SignedOut(_))));
    assert!(
        dropped.get(),
        "refused replacement retained its authentication guard"
    );
}
