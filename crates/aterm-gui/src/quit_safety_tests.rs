// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn quit_always_prompts_and_notes_a_running_job() {
    for reopens in [false, true] {
        let idle = confirm_prompt(true, false, reopens).expect("a whole-app quit always confirms");
        assert_eq!(idle.proceed, "Quit");
        let busy = confirm_prompt(true, true, reopens).expect("a whole-app quit always confirms");
        assert_eq!(busy.proceed, "Quit");
        assert_ne!(
            idle.body, busy.body,
            "the busy-quit copy must mention the running process"
        );
        assert!(busy.body.contains("process"), "{}", busy.body);
    }
}

#[test]
fn window_close_confirms_only_while_busy() {
    assert!(
        confirm_prompt(false, false, true).is_none(),
        "an idle window/tab close needs no confirmation"
    );
    let busy = confirm_prompt(false, true, true).expect("a busy window/tab close confirms");
    assert_eq!(busy.proceed, "Close");
}

/// The Windows policy (WT convention): a single idle tab never prompts, the
/// last-window quit included.
#[cfg(windows)]
#[test]
fn windows_single_idle_tab_closes_without_a_prompt() {
    assert!(confirm_prompt_windows(false, 1, false, true).is_none());
    assert!(
        confirm_prompt_windows(true, 1, false, true).is_none(),
        "an idle one-tab last window must quit instantly on Windows"
    );
    assert!(confirm_prompt_windows(false, 0, false, true).is_none());
}

/// Busy always prompts, with the shared copy: "Quit" for a whole-app exit,
/// "Close" otherwise.
#[cfg(windows)]
#[test]
fn windows_busy_prompts_with_the_real_verbs() {
    let busy_close = confirm_prompt_windows(false, 1, true, true).expect("busy close confirms");
    assert_eq!(busy_close.proceed, "Close");
    let busy_quit = confirm_prompt_windows(true, 1, true, true).expect("busy quit confirms");
    assert_eq!(busy_quit.proceed, "Quit");
}

/// Closing multiple tabs prompts even when idle: quit copy when the gesture exits
/// the app, the multi-tab close copy when another window survives.
#[cfg(windows)]
#[test]
fn windows_multi_tab_close_prompts_when_idle() {
    let quit = confirm_prompt_windows(true, 4, false, true).expect("multi-tab quit confirms");
    assert_eq!(quit.proceed, "Quit");
    let close = confirm_prompt_windows(false, 4, false, true).expect("multi-tab close confirms");
    assert_eq!(close.proceed, "Close");
    assert!(
        close.title.contains("all tabs"),
        "the non-quit multi-tab prompt must say it closes every tab: {}",
        close.title
    );
}

#[test]
fn foreground_job_detection() {
    assert!(
        !foreground_is_job_since(1234, 1234, None),
        "the shell's own pgrp at the prompt is idle"
    );
    assert!(
        foreground_is_job_since(5678, 1234, None),
        "a different foreground pgrp is a running job"
    );
    assert!(
        foreground_is_job_since(5678, 1234, Some(u64::MAX)),
        "tcgetpgrp names the foreground job: the shell's idle word never masks it"
    );
    #[cfg(not(windows))]
    {
        assert!(
            !foreground_is_job_since(0, 1234, None),
            "no foreground pgrp is idle"
        );
        assert!(
            !foreground_is_job_since(-1, 1234, None),
            "a tcgetpgrp error must not wedge the close"
        );
    }
}

const EXECUTING: ShellWord = ShellWord {
    executing: true,
    idle_since_ms: None,
};

#[test]
fn a_running_block_is_busy_without_a_foreground_child() {
    assert!(
        foreground_busy(-1, -1, EXECUTING),
        "the shell said a command is executing: busy, whatever the PTY says"
    );
    assert!(
        !foreground_busy(-1, -1, ShellWord::default()),
        "no block and no foreground evidence is idle"
    );
    assert!(foreground_busy(-1, 0, EXECUTING));
}

#[test]
fn the_guard_is_the_union_of_the_pty_and_the_block() {
    assert!(
        foreground_busy(5678, 1234, ShellWord::default()),
        "a foreground job with no block state is still a job"
    );
    assert!(
        !foreground_busy(1234, 1234, ShellWord::default()),
        "the shell's own pgrp at an idle prompt is idle"
    );
    assert!(
        foreground_busy(1234, 1234, EXECUTING),
        "an executing block wins over an idle-looking pgrp (an in-shell loop)"
    );
}

fn nonced_terminal() -> (aterm_core::terminal::Terminal, String) {
    let nonce = [0x5Au8; 32];
    let hex = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    term.authorize_shell_integration(nonce);
    term.set_require_shell_integration_nonce(true);
    (term, hex)
}

fn now_ms() -> u64 {
    aterm_time::since_epoch()
        .expect("clock after 1970")
        .as_millis() as u64
}

#[test]
fn the_shells_idle_word_needs_its_own_completed_command() {
    let (mut term, id) = nonced_terminal();
    term.process(format!("\x1b]133;A;id={id}\x07PS> \x1b]133;B;id={id}\x07").as_bytes());
    assert_eq!(
        shell_word(&term),
        ShellWord::default(),
        "a first prompt has reported no command yet: silence, not a word"
    );
    term.process(format!("ping -t 127.0.0.1\n\x1b]133;C;id={id}\x07").as_bytes());
    assert_eq!(shell_word(&term), EXECUTING);
    let before = now_ms();
    term.process(format!("\x1b]133;D;0;id={id}\x07").as_bytes());
    let after = now_ms();
    let word = shell_word(&term);
    assert!(!word.executing);
    let since = word.idle_since_ms.expect("a whole command earns the word");
    assert!(
        (before..=after).contains(&since),
        "stamped at the 133;D: {before} <= {since} <= {after}"
    );
    term.process(format!("\x1b]133;A;id={id}\x07PS> \x1b]133;B;id={id}\x07").as_bytes());
    assert_eq!(shell_word(&term).idle_since_ms, Some(since));
}

#[test]
fn unsigned_marks_never_earn_the_shells_idle_word() {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07true\n\x1b]133;C\x07\x1b]133;D;0\x07");
    assert_eq!(
        term.completed_command_seq(),
        1,
        "precondition: it completed"
    );
    assert_eq!(shell_word(&term), ShellWord::default());
}

#[test]
fn a_prompt_only_integration_never_earns_the_shells_idle_word() {
    let (mut term, id) = nonced_terminal();
    for _ in 0..3 {
        term.process(
            format!("\x1b]133;A;id={id}\x07C:\\> \x1b]133;B;id={id}\x07ping -t 127.0.0.1\n")
                .as_bytes(),
        );
    }
    term.process(format!("\x1b]133;D;0;id={id}\x07").as_bytes());
    assert_eq!(
        term.completed_command_seq(),
        0,
        "no C, so no completed command"
    );
    assert_eq!(shell_word(&term), ShellWord::default());
}

fn born(created_ms: u64, image: &str) -> ProcessFacts {
    ProcessFacts {
        created_ms,
        image: image.to_string(),
    }
}

fn facts_of(table: &[(u32, ProcessFacts)]) -> impl Fn(u32) -> Option<ProcessFacts> + '_ {
    move |pid| {
        table
            .iter()
            .find(|(listed, _)| *listed == pid)
            .map(|(_, facts)| facts.clone())
    }
}

const SHELL_BORN: u64 = 1_790_549_217_000;

#[test]
fn a_child_older_than_its_shell_is_a_recycled_pid_not_a_job() {
    let links = ChildLinks::from_links(vec![(17636, 22432)]);
    let table = [
        (17636, born(SHELL_BORN, "pwsh.exe")),
        (22432, born(SHELL_BORN - 39_000, "aterm-gui.exe")),
    ];
    assert!(!links.has_job(17636, None, facts_of(&table)));
    let table = [
        (17636, born(SHELL_BORN, "pwsh.exe")),
        (22432, born(SHELL_BORN + 5_000, "ping.exe")),
    ];
    assert!(links.has_job(17636, None, facts_of(&table)));
}

#[test]
fn an_unreadable_process_is_not_counted() {
    let links = ChildLinks::from_links(vec![(100, 200)]);
    let shell_only = [(100, born(SHELL_BORN, "pwsh.exe"))];
    assert!(!links.has_job(100, None, facts_of(&shell_only)));
    let child_only = [(200, born(SHELL_BORN + 1_000, "ping.exe"))];
    assert!(
        !links.has_job(100, None, facts_of(&child_only)),
        "with no shell birth to compare against, no candidate is ours"
    );
}

#[test]
fn a_shell_with_no_candidate_opens_no_handle() {
    let links = ChildLinks::from_links(vec![(7, 8), (9, 10)]);
    let reads = std::cell::Cell::new(0);
    let busy = links.has_job(100, None, |_| {
        reads.set(reads.get() + 1);
        Some(born(SHELL_BORN, "x.exe"))
    });
    assert!(!busy);
    assert_eq!(reads.get(), 0, "no candidate, no handle");
}

#[test]
fn the_shell_a_launcher_started_is_not_a_job_but_its_children_are() {
    let launcher = 26612;
    let bash = 16752;
    let links = ChildLinks::from_links(vec![(launcher, bash)]);
    let mut table = vec![
        (launcher, born(SHELL_BORN, "bash.exe")),
        (bash, born(SHELL_BORN + 30, "bash.exe")),
    ];
    assert!(
        !links.has_job(launcher, None, facts_of(&table)),
        "an idle Git Bash prompt is idle"
    );
    let links = ChildLinks::from_links(vec![(launcher, bash), (bash, 4040)]);
    table.push((4040, born(SHELL_BORN + 60_000, "ping.exe")));
    assert!(links.has_job(launcher, None, facts_of(&table)));
    table.last_mut().expect("ping").1.created_ms = SHELL_BORN + 10;
    assert!(!links.has_job(launcher, None, facts_of(&table)));
}

#[test]
fn a_nested_shell_typed_later_is_a_job() {
    let links = ChildLinks::from_links(vec![(300, 301)]);
    let table = [
        (300, born(SHELL_BORN, "pwsh.exe")),
        (301, born(SHELL_BORN + LAUNCHER_WINDOW_MS + 1, "pwsh.exe")),
    ];
    assert!(links.has_job(300, None, facts_of(&table)));
}

#[test]
fn after_the_shells_idle_word_only_a_newer_child_is_a_job() {
    let since = SHELL_BORN + 20_000;
    let links = ChildLinks::from_links(vec![(500, 501)]);
    let background = [
        (500, born(SHELL_BORN, "pwsh.exe")),
        (501, born(SHELL_BORN + 10_000, "notepad.exe")),
    ];
    assert!(
        links.has_job(500, None, facts_of(&background)),
        "precondition: without the word, the child walk counts it"
    );
    assert!(!links.has_job(500, Some(since), facts_of(&background)));
    let unannounced = [
        (500, born(SHELL_BORN, "pwsh.exe")),
        (501, born(since + 5_000, "ping.exe")),
    ];
    assert!(links.has_job(500, Some(since), facts_of(&unannounced)));
    let links = ChildLinks::from_links(vec![(600, 601), (601, 602)]);
    let mut table = vec![
        (600, born(SHELL_BORN, "bash.exe")),
        (601, born(SHELL_BORN + 25, "bash.exe")),
        (602, born(SHELL_BORN + 10_000, "sleep.exe")),
    ];
    assert!(!links.has_job(600, Some(since), facts_of(&table)));
    table[2].1.created_ms = since + 1;
    assert!(links.has_job(600, Some(since), facts_of(&table)));
}

#[test]
fn shell_executing_follows_the_133_marks() {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    assert!(!shell_executing(&term), "no shell integration: idle");
    term.process(b"\x1b]133;A\x07PS> ");
    assert!(!shell_executing(&term), "prompt only");
    term.process(b"\x1b]133;B\x07Start-Sleep 60");
    assert!(
        !shell_executing(&term),
        "entering the command line, not running it"
    );
    term.process(b"\n\x1b]133;C\x07");
    assert!(
        shell_executing(&term),
        "133;C opened, no 133;D yet: executing"
    );
    term.process(b"\x1b]133;D;0\x07");
    assert!(!shell_executing(&term), "133;D closed the command: idle");
    term.process(b"\x1b]133;A\x07PS> ");
    assert!(!shell_executing(&term));
}

#[test]
fn a_command_that_clears_the_scrollback_is_still_executing() {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    term.process(b"\x1b]133;A\x07PS> \x1b]133;B\x07Clear-Host; Start-Sleep 60\n\x1b]133;C\x07");
    assert!(shell_executing(&term), "precondition: executing");
    term.process(b"\x1b[H\x1b[2J\x1b[3J");
    assert_eq!(
        term.current_block().map(|b| b.state),
        Some(aterm_types::BlockState::Executing),
        "ED 3 keeps the running block"
    );
    assert!(
        shell_executing(&term),
        "the command is still running: ED 3 is output, not the end of the command"
    );
    let mut carry = term.checkpoint();
    carry.shell.current_block = None;
    let mut adopted = aterm_core::terminal::Terminal::new(24, 80);
    adopted.restore_checkpoint(&carry);
    assert!(
        adopted.current_block().is_none(),
        "precondition: the carry held no block"
    );
    assert!(
        shell_executing(&adopted),
        "the open C..D phase is the reading, block or no block"
    );
    adopted.process(b"\x1b]133;D;0\x07");
    assert!(!shell_executing(&adopted), "and its 133;D ends it");
    term.process(b"\x1b]133;D;0\x07");
    assert!(
        !shell_executing(&term),
        "133;D after the clear still ends the command"
    );
}

#[cfg(windows)]
#[test]
fn windows_invalid_shell_pid_is_idle() {
    assert!(
        !foreground_is_job_since(-1, 0, None),
        "an unknown shell pid must not wedge the close"
    );
    assert!(!foreground_is_job_since(-1, -5, None));
}

/// The real walk, end to end: the live table, real handles, real
/// creation times.
#[cfg(windows)]
#[test]
fn windows_child_walk_detects_a_live_child() {
    let mut child = aterm_uds::spawnfd::command("ping")
        .args(["-n", "30", "127.0.0.1"])
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn ping");
    let me = std::process::id();
    let mine = windows_jobs::process_facts(me).expect("this process reads");
    let ping = windows_jobs::process_facts(child.id()).expect("its child reads");
    assert_eq!(ping.image, "ping.exe");
    assert!(ping.created_ms >= mine.created_ms, "{ping:?} vs {mine:?}");
    assert!(
        foreground_is_job_since(-1, me as i32, None),
        "a live child process must read as a running job"
    );
    assert!(
        !foreground_is_job_since(-1, child.id() as i32, None),
        "a process with no children is idle"
    );
    assert!(
        !foreground_is_job_since(-1, me as i32, Some(ping.created_ms + 60_000)),
        "a child older than the shell's idle word is not a foreground job"
    );
    let _ = child.kill();
    let _ = child.wait();
}

/// FILETIME arithmetic and the image-name read, pinned to known values.
#[cfg(windows)]
#[test]
fn windows_process_facts_units() {
    use super::windows_jobs::{filetime_to_unix_ms, image_file_name};
    let ticks = 116_444_736_000_000_000 + 1_790_467_200_000 * 10_000;
    assert_eq!(filetime_to_unix_ms(ticks), 1_790_467_200_000);
    assert_eq!(
        filetime_to_unix_ms(0),
        0,
        "before 1970 saturates, never wraps"
    );
    let path: Vec<u16> = r"C:\Program Files\Git\bin\..\usr\bin\BASH.exe"
        .encode_utf16()
        .collect();
    assert_eq!(image_file_name(&path), "bash.exe");
    let bare: Vec<u16> = "pwsh.exe".encode_utf16().collect();
    assert_eq!(image_file_name(&bare), "pwsh.exe");
}

/// Which snapshot entries are candidates at all — the one filter the
/// capture applies before any handle is opened.
#[cfg(windows)]
#[test]
fn the_capture_books_only_candidate_children() {
    use super::windows_jobs::entry_counts_as_child;
    assert!(
        entry_counts_as_child(4321, 1234, false),
        "an ordinary child counts"
    );
    assert!(
        !entry_counts_as_child(1234, 1234, false),
        "a process that parents itself is not its own job"
    );
    assert!(
        !entry_counts_as_child(4321, 1234, true),
        "the console host is plumbing, never a user job"
    );
}

fn key(seq: u64) -> JobEvidenceKey {
    JobEvidenceKey {
        content_seq: seq,
        alt_screen: false,
        output_ns: 0,
        input: None,
    }
}

fn same_age(_: u32) -> Option<ProcessFacts> {
    Some(born(SHELL_BORN, ""))
}

fn job_of(shell: u32) -> impl FnOnce(&ChildLinks) -> bool {
    move |links| links.has_job(shell, None, same_age)
}

#[test]
fn one_capture_answers_every_session_in_a_sweep() {
    let mut probe = JobProbe::default();
    let now = Instant::now();
    let mut captures = 0usize;
    for session in 0..8u64 {
        let capture = || {
            captures += 1;
            ChildLinks::default()
        };
        let busy =
            probe.child_verdict(session, key(0), now, capture, job_of(1000 + session as u32));
        assert!(!busy, "no pid has a child in this fixture");
    }
    assert_eq!(
        captures, 1,
        "a sweep must buy ONE system process-table capture, not one per session"
    );
    assert_eq!(probe.capture_count(), 1);
}

#[test]
fn a_settled_window_buys_one_capture_per_ceiling() {
    let mut probe = JobProbe::default();
    let t0 = Instant::now();
    let mut captures = 0usize;
    let sweep = |probe: &mut JobProbe, at: Instant, captures: &mut usize| {
        for session in 0..8u64 {
            let capture = || {
                *captures += 1;
                ChildLinks::default()
            };
            probe.child_verdict(session, key(0), at, capture, job_of(1000 + session as u32));
        }
    };
    let mut at = t0;
    let mut sweeps = 0usize;
    while at.duration_since(t0) < JOB_PROBE_MAX_AGE {
        sweep(&mut probe, at, &mut captures);
        sweeps += 1;
        at += Duration::from_millis(250);
    }
    assert!(
        sweeps >= 8,
        "the fixture must actually run sweeps: {sweeps}"
    );
    assert_eq!(
        captures, 1,
        "{sweeps} settled sweeps of 8 sessions must share ONE capture"
    );
    sweep(&mut probe, t0 + JOB_PROBE_MAX_AGE, &mut captures);
    assert_eq!(captures, 2, "the ceiling refreshes once, for every session");
}

#[test]
fn moved_evidence_forces_a_fresh_capture() {
    let mut probe = JobProbe::default();
    let t0 = Instant::now();
    let mut captures = 0usize;
    let idle = |captures: &mut usize| {
        *captures += 1;
        ChildLinks::default()
    };
    assert!(!probe.child_verdict(7, key(0), t0, || idle(&mut captures), job_of(1234)));
    assert_eq!(captures, 1);
    let t1 = t0 + Duration::from_millis(250);
    assert!(!probe.child_verdict(7, key(0), t1, || idle(&mut captures), job_of(1234)));
    assert_eq!(captures, 1, "a settled session buys nothing");
    let t2 = t1 + Duration::from_millis(250);
    let capture = || {
        captures += 1;
        ChildLinks::from_links(vec![(1234, 4321)])
    };
    let busy = probe.child_verdict(7, key(1), t2, capture, job_of(1234));
    assert!(
        busy,
        "the fresh capture must be what the verdict is read from"
    );
    assert_eq!(captures, 2, "moved evidence buys exactly one fresh capture");
}

#[test]
fn the_verdict_is_the_child_walk_of_the_shared_capture() {
    let mut probe = JobProbe::default();
    let now = Instant::now();
    let links = ChildLinks::from_links(vec![(42, 500), (99, 501)]);
    assert!(probe.child_verdict(1, key(0), now, || links.clone(), job_of(42)));
    assert!(probe.child_verdict(2, key(0), now, || links.clone(), job_of(99)));
    assert!(!probe.child_verdict(3, key(0), now, || links.clone(), job_of(7)));
}

#[test]
fn forget_drops_the_retired_sessions_evidence() {
    let mut probe = JobProbe::default();
    let t0 = Instant::now();
    let mut captures = 0usize;
    let mut capture = || {
        captures += 1;
        ChildLinks::default()
    };
    probe.child_verdict(5, key(0), t0, &mut capture, job_of(1234));
    assert_eq!(probe.capture_count(), 1);
    probe.forget(5);
    let t1 = t0 + Duration::from_millis(250);
    probe.child_verdict(5, key(0), t1, &mut capture, job_of(1234));
    assert_eq!(
        captures, 2,
        "a forgotten session has no remembered evidence, so it re-probes"
    );
}

#[test]
fn a_live_foreground_pgrp_never_buys_a_capture() {
    let mut probe = JobProbe::default();
    let now = Instant::now();
    let mut captures = 0usize;
    let mut capture = || {
        captures += 1;
        ChildLinks::default()
    };
    assert!(
        probe.is_job_with(1, 4321, 1234, key(0), now, &mut capture),
        "a group in front that is not the shell's is a job"
    );
    assert!(
        !probe.is_job_with(1, 1234, 1234, key(0), now, &mut capture),
        "the shell's own group in front is idle"
    );
    let busy = probe.is_job_with(1, -1, 0, key(0), now, &mut capture);
    assert!(!busy, "an unknown shell pid is idle, and buys nothing");
    assert_eq!(captures, 0);
    assert_eq!(probe.capture_count(), 0);
}

/// The program probe on real processes: a stand-in shell (`sh -c`, which
/// runs its programs in its own group, as PowerShell does) running a
/// program it started since an instant is seen; the same program before
/// the instant is not; and one the shell put in a group of its own (`set
/// -m`, as zsh, bash and fish do) is the foreground word's, never the
/// probe's.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_probe_sees_a_program_the_shell_runs_in_its_own_group() {
    let now_ms = || {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("after the epoch")
                .as_millis(),
        )
        .expect("fits")
    };
    let since = now_ms().saturating_sub(100);
    let home = aterm_tempfile::tempdir().expect("scratch home");
    let shell = |program: &std::path::Path, script: &str| {
        aterm_spec::host::shell_command(program, home.path())
            .args(["-c", script])
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
    };
    struct Started(Vec<aterm_uds::spawnfd::Child>);
    impl Started {
        fn hold(&mut self, child: aterm_uds::spawnfd::Child) -> i32 {
            let pid = i32::try_from(child.id()).expect("pid fits");
            self.0.push(child);
            pid
        }
    }
    impl Drop for Started {
        fn drop(&mut self) {
            for shell in &mut self.0 {
                let programs = i32::try_from(shell.id())
                    .ok()
                    .and_then(session_root)
                    .map(|root| root.children)
                    .unwrap_or_default();
                for program in programs {
                    // SAFETY: SIGKILL to a `sleep` this test's shell forked.
                    unsafe { libc::kill(program, libc::SIGKILL) };
                }
                let _ = shell.kill();
                let _ = shell.wait();
            }
        }
    }
    let mut started = Started(Vec::new());
    let bash = aterm_spec::host::test_shell("bash").unwrap_or_else(|| {
        aterm_spec::host::could_not_run(aterm_spec::host::tool("bash").refusal())
    });
    let job_control = started.hold(
        shell(&bash, "set -m; sleep 120 & wait")
            .unwrap_or_else(|error| panic!("spawn {}: {error}", bash.display())),
    );
    let own_group =
        started.hold(shell(std::path::Path::new("/bin/sh"), "sleep 120; :").expect("spawn sh"));
    let forks = |pid: i32| {
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(5),
            || {
                session_root(pid)
                    .filter(|root| !root.children.is_empty())
                    .map(drop)
            },
        )
        .expect("the shell forked its program");
    };
    forks(own_group);
    assert!(shell_runs_program_since(own_group, since), "started since");
    assert!(
        !shell_runs_program_since(own_group, now_ms() + 3_600_000),
        "started before the instant"
    );
    forks(job_control);
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(5),
        || {
            session_root(job_control)
                .filter(|root| {
                    !root.children.is_empty()
                        && root.children.iter().all(|&program| {
                            // SAFETY: getpgid reads a process's group.
                            let group = unsafe { libc::getpgid(program) };
                            group == program
                        })
                })
                .map(drop)
        },
    )
    .expect("bash moved its program to a group of its own");
    assert!(
        !shell_runs_program_since(job_control, since),
        "a program in a group of its own"
    );
    assert!(!shell_runs_program_since(-1, since), "no shell");
}
