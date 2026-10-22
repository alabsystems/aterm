// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_phase::Program;

/// A worker's frame announces 0 to 64 KiB; a length past that, or none at
/// all, is refused unread (the connection then closes), naming the cap and
/// the spawn's own agent word ([`worker_word`]: a word no worker goes by is
/// named `claude`, as before the line is parsed whole).
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
    for word in ["claude", "gemini", "agy", "muse"] {
        assert_eq!(worker_word(Some(word)), word);
    }
    for none in [Some("codex"), Some("bash"), Some(""), None] {
        assert_eq!(worker_word(none), "claude", "CONTROL: {none:?}");
    }
}

/// The order a worker's spawn makes of its frame: no frame, an empty task
/// and a NUL byte are refused by name; a task with every other byte is kept
/// as given, and the cwd named is the canonical trusted folder.
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
    // A drive's root on Windows (`C:\`), which `canonicalize` answers
    // verbatim (`\\?\C:\`): trusted as a person spells it.
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

/// The task file is the task, its last line ended, a blank line, then the
/// one line of fact: it names the starter and the worker's Autopilot, and the
/// `Nothing left:` defense that ends a Land worker's work, without the retired
/// token. NEGATIVE CONTROL: a Cruise worker's fact names no defense (no
/// report ends Cruise). The fact names the spawn's own agent word.
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
    // With no starter to name, the task alone, its last line ended.
    assert_eq!(
        task_text(b"do it", None, Autopilot::Land, Program::Claude),
        b"do it\n"
    );
}

/// A LINE THAT DRAWS AS A BLANK ROW IS A BLANK LINE OF AN AGY WORKER'S TASK
/// ([`without_blank_lines`]; the review of b9/spawn-residuals, F3): agy's
/// reader reads a row of Unicode white space alone as blank (a no-break
/// space, an ideographic space, an em space), and a zero-width or control
/// character draws nothing beside it — so such a line ends agy's message as
/// an empty one does, the fact line under it read as agy's reply, its
/// `NEEDS YOU:` with it. Each is dropped. The reader's side is measured:
/// agy's screen with a no-break-space row between the task and the fact
/// reads the fact as the reply. NEGATIVE CONTROLS: a line with a word beside
/// its no-break space is kept, and every other worker keeps the task as
/// given.
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
    for agent in [Program::Claude, Program::Gemini, Program::Muse] {
        assert_eq!(
            text(agent),
            task,
            "CONTROL: {agent:?} keeps the task as given"
        );
    }
    // agy's reader: the measured first turn with its blank row a no-break
    // space reads the fact under it as the reply, as the empty row does.
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

/// AN AGY WORKER'S FIRST PROMPT HOLDS NO BLANK LINE ([`task_text`],
/// `WorkerCli::blank_lines`). agy draws a message's blank line as a blank
/// row, the rows under it two columns in as its reply's
/// ([`WORKER_FIRST_PROMPT`], measured in `lab forever`'s L15): the worker's
/// first turn read the fact line as its reply — its `NEEDS YOU:` a need
/// nobody said, raised as the turn ended (the manager's `await task` woke
/// on it), and the fact in the worker's result. Its task file drops the
/// task's blank lines and puts the fact on the line under it: the same
/// message drawn with no blank row is one message, and the reply under it
/// is read alone, with no need. NEGATIVE CONTROLS: the measured screen reads
/// the fact as the reply (its need, raised in `l15a`, now read as the line's
/// mention); every other worker's task file
/// keeps the task as given and the blank line before the fact (their screens
/// mark a reply's head: Claude Code's `⏺`, Gemini CLI's `✦`, Muse Code's
/// `◆`).
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
    for agent in [Program::Claude, Program::Gemini, Program::Muse] {
        let fact = fact_line("s-ab12", Autopilot::Land, agent.name());
        assert_eq!(
            text(task.as_bytes(), Some("s-ab12"), agent),
            format!("{task}\n{fact}\n"),
            "CONTROL: {agent:?} keeps the task's blank lines"
        );
    }

    // The worker's first turn as it ended: its rows up to the Land act's
    // rule, then the composer.
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
    // NEGATIVE CONTROL: as measured, the fact line read as the reply. Its
    // `NEEDS YOU:` raised the need in `l15a`; the needs reader now reads a
    // token `a line starting` names as the line's mention
    // (`needs::names_a_head`, the review of b9/spawn-residuals), so the fact
    // read as the reply names none — the misread itself stands.
    let misread = said_tail(&first).expect("words");
    assert!(
        misread.starts_with("(Started by `s-146719f2750f4a8f6909`") && misread.ends_with(reply),
        "{misread}"
    );
    assert!(needs_you(&misread).is_empty(), "{misread}");
    // That message is the old task file drawn: the task, a blank row, the
    // fact's rows.
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
    // The task file now: no blank line, so the same message is drawn with no
    // blank row, one message over the reply.
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

/// A worker's order is under Land unless the spawn said otherwise
/// ([`WORKER_AUTOPILOT`], [`TaskOrder::under`]). NEGATIVE CONTROL: a spawn's
/// `autopilot=cruise` is kept.
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

    // Cancellation may answer the main thread before the spawner writes.
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

    // Or discard can win after the actual exclusive write but before its
    // path is published. Interpose it deterministically on the real writer;
    // no new thread, wait or program is involved.
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

    // Dropping the last seed is not cancellation: a successfully spawned
    // shell may not have reached its first prompt/read of the task file yet.
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

/// A worker carries its starter's program, homes and config flags (agy's
/// data directories, Muse Code's provider) when found, and otherwise starts
/// the agent's plain command under the shell's configuration.
#[test]
fn a_worker_carries_its_starters_configuration_or_uses_the_plain_command() {
    use aterm_agent::harness::worker::{Starter, WORKERS};
    for cli in &WORKERS {
        let exe = PathBuf::from(format!("/opt/twin/{}", cli.word));
        let starter = || Starter {
            exe: exe.clone(),
            env: vec![("HOME".to_string(), "/homes/a".to_string())],
            args: Ok(vec!["--provider".to_string(), "p".to_string()]),
        };
        assert_eq!(
            line_starter(cli, Some(starter())),
            starter(),
            "{}",
            cli.word
        );
        assert_eq!(
            line_starter(cli, None),
            Starter {
                exe: PathBuf::from(cli.word),
                env: Vec::new(),
                args: Ok(Vec::new()),
            },
            "a starter that runs no {}: its plain word",
            cli.agent.product()
        );
    }
}

/// THE LINE TRUSTS ITS FOLDER FOR THIS RUN ONLY WHERE THE SHELL STANDS IN
/// THE ONE ITS STARTER TRUSTED: Muse Code's `--trust-workspace` rides the
/// line only where the agent has the flag, the spawn named a `cwd=`, and the
/// shell's own directory, read at its prompt, is exactly that folder.
/// Gemini CLI has none: its `--skip-trust` trusts every folder for every
/// Gemini CLI the worker starts ([`TaskSeed::with_trust_file`] trusts its one).
/// NEGATIVE CONTROLS: an rc file's `cd` elsewhere (or into a folder beneath
/// it), a directory not read, no `cwd=`, and an agent with no such flag
/// (Claude Code, Gemini CLI, agy) each trust nothing — and the directory is
/// not even read where nothing could be trusted.
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
/// folder (`aterm_agent::harness::worker::trusted_folders_text`). The close
/// that removes the task file removes it ([`retire_task_file_in`]).
/// NEGATIVE CONTROL: another worker's stays.
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

    // A sidecar this seed did not create is neither adopted nor removed.
    // The old optional-file failure behavior remains an untrusted launch.
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
    for agent in [
        Program::Claude,
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
