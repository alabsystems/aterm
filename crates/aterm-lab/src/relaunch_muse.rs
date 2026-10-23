// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live `muse-relaunch` lab lane exercising Muse Code crash/exit/restore relaunch behavior.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::door::Door;
use crate::relaunch_check::{
    cwd_of, exit_of, host_said, newest_dump, relaunch_rows, request_with, said, send_when_free,
    shown, unobserved, wait_for,
};
use crate::restore::{self, Verdict};
use crate::run::{Run, is_run};
use crate::stop;
use crate::util::now_ms;
use crate::{Fail, LAB, Res};

/// Marker word remembered in the initial phase-1 turn.
pub const WORD: &str = "PELICAN-7731";

/// Marker word remembered by the fresh Muse session in phase 5.
pub const WORD2: &str = "OSPREY-2208";

/// Well-formed session UUID that no session holds.
pub const UNKNOWN: &str = "0badbeef-0000-4000-8000-000000000000";

/// Harness continuation prompt prefix typed after a process crash.
pub const CONTINUED_CRASH: &str = "[aterm harness] Relaunched: Muse Code exited";

/// Harness continuation prompt substring typed after `aterm` itself exited.
pub const CONTINUED_HOST: &str = "aterm ended while this session ran";

fn could_not(m: &str) -> Fail {
    Fail::exit(3, format!("muse-relaunch: COULD NOT RUN: {m}"))
}

/// Returns the session ID following `resume` in `words`.
#[must_use]
pub fn resumed(words: &[String]) -> Option<&str> {
    let at = words.iter().position(|w| w == "resume")?;
    words.get(at + 1).map(String::as_str)
}

/// The Muse Code processes under the lab's instance, `(pid, words)`.
fn muses(run: &Run) -> Vec<(i32, Vec<String>)> {
    crate::relaunch_check::running(run, &run.muse_bin)
}

/// Parses `pid=<pid>` from a `.session.lock` body.
#[must_use]
pub fn lock_pid(body: &str) -> Option<i64> {
    body.lines()
        .find_map(|l| l.trim().strip_prefix("pid="))
        .and_then(|p| p.parse().ok())
}

/// Every session-depth lock under the scratch HOME's data home
/// (`sessions/YYYY/MM/DD/<uuid>/.session.lock`), `(session, lock)`.
fn locks(run: &Run) -> Vec<(String, PathBuf)> {
    let root = run.w.join("home/.local/share/muse/sessions");
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect()
    };
    dirs(&root)
        .iter()
        .filter(|y| y.file_name().is_some_and(|n| n.len() == 4))
        .flat_map(|y| dirs(y))
        .flat_map(|m| dirs(&m))
        .flat_map(|d| dirs(&d))
        .filter_map(|s| {
            let session = s.file_name()?.to_string_lossy().into_owned();
            Some((session, s.join(".session.lock")))
        })
        .collect()
}

/// The session whose lock names `pid`.
fn session_of(run: &Run, pid: i32) -> Option<String> {
    locks(run).into_iter().find_map(|(session, lock)| {
        (lock_pid(&crate::util::scratch_text(&lock)) == Some(i64::from(pid))).then_some(session)
    })
}

/// Shell `export` commands configuring a relaunched Muse Code to reach the stub.
#[must_use]
pub fn shell_exports(run: &Run) -> String {
    use crate::util::shell_quote as q;
    let w = run.w.to_string_lossy();
    let home = format!("{w}/home");
    let shim = run.run.join("shim");
    let px = q(&run.proxy);
    let stub = format!("http://127.0.0.1:{}", run.port);
    let mut s = String::from("PROMPT='lab$ '\n");
    s.push_str(
        "unset OPENAI_API_KEY OPENAI_BASE_URL ANTHROPIC_API_KEY ANTHROPIC_BASE_URL \
         OPENROUTER_API_KEY OPENROUTER_BASE_URL CODEX_HOME CLAUDE_CONFIG_DIR MUSE_MODEL\n",
    );
    s.push_str(&format!(
        "export PATH={}:\"$PATH\" HOME={} TMPDIR={} XDG_STATE_HOME={} XDG_CACHE_HOME={}\n",
        q(&shim.to_string_lossy()),
        q(&home),
        q(&format!("{w}/tmp")),
        q(&format!("{home}/.local/state")),
        q(&format!("{home}/.cache"))
    ));
    s.push_str(&format!(
        "export HTTP_PROXY={px} HTTPS_PROXY={px} http_proxy={px} https_proxy={px} ALL_PROXY={px} all_proxy={px}\n"
    ));
    s.push_str("export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost\n");
    s.push_str(&format!(
        "export LAB_SECURITY_LOG={} LAB_OPEN_LOG={} BROWSER={}\n",
        q(&format!("{w}/security.log")),
        q(&format!("{w}/open.log")),
        q(&shim.join("open").to_string_lossy())
    ));
    s.push_str(&format!(
        "export META_API_KEY={} TBH_AUTH_BASE_URL={stub} TBH_MINT_BASE_URL={stub}\n",
        crate::stub::LAB_CODEX_KEY
    ));
    s.push_str(
        "export TBH_DISABLE_TELEMETRY=1 TBH_DISABLE_FEATURE_CONFIG=1 TBH_DISABLE_DEFAULT_PLUGIN_INSTALL=1\n",
    );
    s
}

/// Returns the `atpkg` shim script that execs `bin`.
#[must_use]
pub fn twin_script(bin: &str) -> String {
    format!(
        "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n# aterm-lab muse-relaunch: the lab's build as the managed twin (crates/aterm-lab)\nexec '{}' \"$@\"\n",
        bin.replace('\'', "'\\''")
    )
}

/// The workspace a session's own log names (`workspace_root`, its newest).
fn workspace_of(run: &Run, session: &str) -> Option<String> {
    let lock = locks(run)
        .into_iter()
        .find(|(s, _)| s == session)
        .map(|(_, l)| l)?;
    let log = crate::util::scratch_text(&lock.with_file_name("session.jsonl"));
    workspace_in(&log)
}

/// Returns the newest `"workspace_root"` path recorded in `log`.
#[must_use]
pub fn workspace_in(log: &str) -> Option<String> {
    let key = "\"workspace_root\":\"";
    log.rmatch_indices(key).next().and_then(|(at, _)| {
        let rest = &log[at + key.len()..];
        rest.split('"').next().map(str::to_string)
    })
}

/// A Muse resuming `session` under the instance other than every pid of
/// `not`.
fn resuming(run: &Run, session: &str, not: &[i32]) -> Option<(i32, Vec<String>)> {
    muses(run)
        .into_iter()
        .find(|(pid, w)| !not.contains(pid) && resumed(w) == Some(session))
}

/// One `muse-relaunch` run on the lab `tag`, which `cmd_muse_relaunch`
/// brought up.
#[allow(clippy::too_many_lines)]
fn run_check(tag: &str, armed: bool) -> Result<Verdict, Fail> {
    let mut run = Run::load(tag)?;
    let aterm = restore::seam_checked(&run)?;
    crate::muse::need_muse(&run).map_err(|f| could_not(&f.msg.unwrap_or_default()))?;
    println!(
        "muse-relaunch: lab '{tag}', Muse Code {} ({}), {aterm}",
        run.muse_ver, run.muse_bin
    );
    let mut failed = Vec::new();
    let mut unseen = Vec::new();
    let pkg = run.w.join("home/Library/Application Support/aterm/pkg");
    let twin = pkg.join("agents/muse");
    crate::sys::put(
        &twin,
        0o755,
        &crate::sys::PutSrc::Bytes(twin_script(&run.muse_bin).as_bytes()),
    )
    .map_err(|e| could_not(&format!("the twin: {e}")))?;
    let rc = run.w.join("home/.zshrc");
    crate::sys::put(
        &rc,
        0o644,
        &crate::sys::PutSrc::Bytes(shell_exports(&run).as_bytes()),
    )
    .map_err(|e| could_not(&format!("the shell startup: {e}")))?;
    let door = Door::new(&run)?;
    let rc_s = rc.to_string_lossy().replace('\'', "'\\''");
    door.ctl(&["send", &format!(". '{rc_s}'")])?;
    door.ctl(&["key", "enter"])?;
    stop::sleep(Duration::from_millis(500))?;
    println!("  twin      {} -> {}", twin.display(), run.muse_bin);

    crate::muse::launch(
        &run,
        &crate::muse::Launch {
            no_wait: true,
            args: vec!["--trust-workspace".to_string()],
            ..crate::muse::Launch::default()
        },
    )?;
    run = Run::load(tag)?;
    let door = Door::new(&run)?;
    if !door.seen("^❯", 60)? {
        return Err(could_not("Muse Code never drew its composer"));
    }
    crate::verbs::turn(&door, &format!("remember the word {WORD}"))?;
    if !door.seen(r"\(reply 1\)", 60)? {
        return Err(could_not("the stub's first answer never showed"));
    }
    let Some((pid1, session)) = wait_for(30, || {
        muses(&run)
            .into_iter()
            .find_map(|(pid, _)| session_of(&run, pid).map(|s| (pid, s)))
    })?
    else {
        return Err(could_not(
            "no Muse Code holds a session lock under the scratch HOME",
        ));
    };
    let workspace = workspace_of(&run, &session);
    println!(
        "  launched  Muse pid {pid1}, session {session}, workspace {}",
        workspace.as_deref().unwrap_or("(unread)")
    );
    let t_j = now_ms();
    let journal = wait_for(30, || restore::desk_naming(&run, &session))?;
    println!(
        "  journal   {}",
        journal.map_or_else(
            || "NO crash journal named the session within 30 s".to_string(),
            |_| format!("names the session within {}", restore::since(t_j, now_ms()))
        )
    );
    let before = newest_dump(&run);
    let t_crash = now_ms();
    let _ = crate::sys::kill(pid1, libc::SIGKILL);
    let back = wait_for(90, || resuming(&run, &session, &[pid1]))?;
    let crashed_exit = exit_of(&door, "muse.sh");
    println!(
        "  crash     SIGKILL pid {pid1}: its block's exit {}",
        shown(crashed_exit)
    );
    let Some((pid2, words)) = back else {
        for row in restore::ledger_since(&run, t_crash) {
            println!("  typed     {row}");
        }
        said(
            false,
            "1. the crash relaunched on its session within 90 s",
            &mut failed,
        );
        return Ok(Verdict::Failed);
    };
    let cwd2 = cwd_of(pid2);
    println!(
        "  relaunch  Muse pid {pid2} after {} in {}: {} {}",
        restore::since(t_crash, now_ms()),
        cwd2.as_deref().unwrap_or("(unread)"),
        run.muse_bin,
        words.join(" ")
    );
    let continued = wait_for(120, || request_with(&run, before, &[CONTINUED_CRASH, WORD]))?;
    let typed = restore::ledger_since(&run, t_crash);
    for row in &typed {
        println!("  typed     {row}");
    }
    let line = typed.join("\n");
    let on_twin = line.contains("agents/muse'") && !line.contains("/store/muse/");
    println!(
        "  continued {}",
        continued.map_or_else(
            || "NO request carried the continuation and the earlier turn within 120 s".to_string(),
            |n| format!("req-{n:05} carried the harness's continuation and `{WORD}`")
        )
    );
    said(
        crashed_exit == Some(137)
            && on_twin
            && workspace.is_some()
            && cwd2 == workspace
            && continued.is_some(),
        "1. a SIGKILLed Muse is relaunched on its session, in its workspace, on the twin; the stub sees the prior turns",
        &mut failed,
    );

    door.await_idle(2000, 60)?;
    let before = newest_dump(&run);
    let t2 = now_ms();
    let _ = crate::sys::kill(pid2, libc::SIGKILL);
    let back = wait_for(120, || resuming(&run, &session, &[pid1, pid2]))?;
    let relaunch_line = format!("'resume' '{session}'");
    let resumed_exit = exit_of(&door, &relaunch_line);
    println!(
        "  crash 2   SIGKILL the resumed Muse pid {pid2}: its relaunch line's block exit {}",
        shown(resumed_exit)
    );
    let pid3 = back.as_ref().map(|(p, _)| *p);
    if let Some(p) = pid3 {
        println!(
            "  relaunch  Muse pid {p} after {}",
            restore::since(t2, now_ms())
        );
    }
    let continued2 = wait_for(120, || request_with(&run, before, &[CONTINUED_CRASH, WORD]))?;
    said(
        resumed_exit == Some(137) && pid3.is_some() && continued2.is_some(),
        "2. a RESUMED Muse's SIGKILL is read off its block (137) and relaunched again",
        &mut failed,
    );
    let Some(pid3) = pid3 else {
        return Ok(Verdict::Failed);
    };

    door.await_idle(2000, 60)?;
    stop::sleep(Duration::from_secs(2))?;
    let t3 = now_ms();
    let typed_exit = send_when_free(&door, "/exit")?;
    let gone = crate::procs::wait_gone(pid3, Duration::from_secs(20));
    stop::sleep(Duration::from_secs(2))?;
    let clean_exit = exit_of(&door, &relaunch_line);
    let decided3 = wait_for(130, || {
        host_said(&run, t3, &["step=ended:graceful-exit", "step=adopted"])
    })?;
    let t_decided3 = now_ms();
    let again = resuming(&run, &session, &[pid1, pid2, pid3]);
    let relaunched_rows = relaunch_rows(&run, t3);
    println!(
        "  exit      `/exit` typed={typed_exit}, pid {pid3} gone={gone}, its block's exit {}; the host's decision after {}: {}; a Muse since: {}; relaunch rows since: {relaunched_rows}",
        shown(clean_exit),
        restore::since(t3, t_decided3),
        decided3.as_deref().map_or("NONE within 130 s", |l| l
            .split_once("step=")
            .map_or(l, |(_, s)| s)),
        again
            .as_ref()
            .map_or_else(|| "none".to_string(), |(p, _)| p.to_string())
    );
    said(
        typed_exit
            && gone
            && clean_exit == Some(0)
            && decided3
                .as_deref()
                .is_some_and(|l| l.contains("step=ended:graceful-exit"))
            && again.is_none()
            && relaunched_rows == 0,
        "3. a clean /exit (0) is never relaunched: the host decided `ended:graceful-exit`",
        &mut failed,
    );

    let t4 = now_ms();
    crate::muse::launch(
        &run,
        &crate::muse::Launch {
            no_wait: true,
            args: vec!["resume".to_string(), UNKNOWN.to_string()],
            ..crate::muse::Launch::default()
        },
    )?;
    stop::sleep(Duration::from_secs(4))?;
    let refused_exit = exit_of(&door, "muse.sh");
    let said_refused = door.out(&["text"]).contains("retained session not found");
    let host4 = wait_for(30, || {
        host_said(
            &run,
            t4,
            &[
                "read at the exit: refused",
                "not relaunched",
                "step=ended:",
                "step=refused:",
                "step=adopted",
            ],
        )
    })?;
    let refused4 = host4
        .as_deref()
        .is_some_and(|l| l.contains("refused-start") || l.contains("read at the exit: refused"));
    let unread4 = host4
        .as_deref()
        .is_none_or(|l| l.contains("nothing-recorded"));
    let any = muses(&run)
        .into_iter()
        .find(|(p, _)| ![pid1, pid2, pid3].contains(p));
    let rows4 = relaunch_rows(&run, t4);
    println!(
        "  refused   `resume {UNKNOWN}`: `retained session not found` shown={said_refused}, its block's exit {}; the host: {}; a Muse since: {}; relaunch rows since: {rows4}",
        shown(refused_exit),
        host4.as_deref().map_or("no line names it", |l| l
            .split_once(": harness @")
            .map_or(l, |(_, s)| s)),
        any.as_ref()
            .map_or_else(|| "none".to_string(), |(p, _)| p.to_string())
    );
    let what4 = "4. a person's refused start (1) is never retried as a crash";
    if !(said_refused && refused_exit == Some(1) && any.is_none() && rows4 == 0) {
        said(false, what4, &mut failed);
    } else if refused4 {
        said(true, what4, &mut failed);
    } else if unread4 {
        unobserved(
            what4,
            "it ended before the host read it (no line, or `nothing-recorded`: the exit table \
             never ran); the refusal table is unit-tested",
            &mut unseen,
        );
    } else {
        said(false, what4, &mut failed);
    }

    let known5 = [pid1, pid2, pid3];
    crate::muse::launch(
        &run,
        &crate::muse::Launch {
            no_wait: true,
            args: vec!["--trust-workspace".to_string()],
            ..crate::muse::Launch::default()
        },
    )?;
    run = Run::load(tag)?;
    let door = Door::new(&run)?;
    let Some((pid6, session2)) = wait_for(60, || {
        muses(&run).into_iter().find_map(|(pid, _)| {
            (!known5.contains(&pid))
                .then(|| session_of(&run, pid).filter(|s| *s != session))
                .flatten()
                .map(|s| (pid, s))
        })
    })?
    else {
        return Err(could_not("the fresh Muse Code holds no session lock"));
    };
    door.seen("^❯", 60)?;
    let before5 = newest_dump(&run);
    crate::verbs::turn(&door, &format!("remember the word {WORD2}"))?;
    let answered = wait_for(60, || request_with(&run, before5, &[WORD2]))?;
    door.await_idle(2000, 60)?;
    let named = wait_for(30, || restore::desk_naming(&run, &session2))?;
    let log2 = locks(&run)
        .into_iter()
        .find(|(s, _)| *s == session2)
        .map(|(_, l)| l.with_file_name("session.jsonl"));
    let aside = log2
        .as_ref()
        .is_some_and(|log| std::fs::rename(log, log.with_file_name("session.jsonl.aside")).is_ok());
    println!(
        "  fresh     Muse pid {pid6}, session {session2}: its turn reached the stub={}, the journal names it={}, its session.jsonl moved aside={aside}",
        answered.is_some(),
        named.is_some()
    );
    let t5 = now_ms();
    let _ = crate::sys::kill(pid6, libc::SIGKILL);
    let decided5 = wait_for(130, || {
        host_said(
            &run,
            t5,
            &[
                "step=refused:",
                "step=adopted",
                "step=ended:",
                "step=failed:",
            ],
        )
    })?;
    let t_decided5 = now_ms();
    let line5 = format!("'resume' '{session2}'");
    let refused_line = exit_of(&door, &line5);
    let rows5 = relaunch_rows(&run, t5);
    stop::sleep(Duration::from_secs(75))?;
    let rows5_later = relaunch_rows(&run, t5);
    let decided_word = decided5
        .as_deref()
        .and_then(|l| l.split_once("step=").map(|(_, s)| s.to_string()));
    for row in restore::ledger_since(&run, t5) {
        println!("  typed     {row}");
    }
    println!(
        "  refused   SIGKILL pid {pid6}: the host's decision after {}: {}; the relaunch line's block exit {}; relaunch rows: {rows5}, and {rows5_later} after 75 s more",
        restore::since(t5, t_decided5),
        decided_word.as_deref().unwrap_or("NONE within 130 s"),
        shown(refused_line)
    );
    said(
        aside
            && decided_word.as_deref() == Some("refused:resume-refused")
            && refused_line == Some(1)
            && rows5 == 1
            && rows5_later == 1,
        "5. a relaunch Muse refused (1) is typed once and never again (`refused:resume-refused`)",
        &mut failed,
    );
    if let Some(log) = &log2 {
        let _ = std::fs::rename(log.with_file_name("session.jsonl.aside"), log);
    }

    let Some(proj) = workspace.clone() else {
        return Ok(Verdict::Failed);
    };
    let script = run.run.join("muse-resume.sh");
    crate::sys::put(
        &script,
        0o755,
        &crate::sys::PutSrc::Bytes(
            crate::muse::launcher(
                &run,
                &[],
                &[
                    "--trust-workspace".to_string(),
                    "resume".to_string(),
                    session.clone(),
                ],
            )
            .as_bytes(),
        ),
    )
    .map_err(|e| could_not(&format!("the resume launcher: {e}")))?;
    let q = |p: &str| format!("'{}'", p.replace('\'', "'\\''"));
    send_when_free(&door, &format!("mkdir -p {0}/sub && cd {0}/sub", q(&proj)))?;
    stop::sleep(Duration::from_secs(1))?;
    send_when_free(
        &door,
        &format!(
            "( cd {} && exec sh {} )",
            q(&proj),
            q(&script.to_string_lossy())
        ),
    )?;
    let Some((pid4, _)) = wait_for(30, || resuming(&run, &session, &[pid1, pid2, pid3, pid6]))?
    else {
        said(
            false,
            "6. the session resumed by hand from a subdirectory",
            &mut failed,
        );
        return Ok(Verdict::Failed);
    };
    door.seen("^❯", 60)?;
    door.await_idle(2000, 30)?;
    let before = newest_dump(&run);
    let t5 = now_ms();
    let _ = crate::sys::kill(pid4, libc::SIGKILL);
    let back = wait_for(130, || {
        resuming(&run, &session, &[pid1, pid2, pid3, pid4, pid6])
    })?;
    let pid5 = back.as_ref().map(|(p, _)| *p);
    let cwd5 = pid5.and_then(cwd_of);
    let continued5 = wait_for(120, || request_with(&run, before, &[CONTINUED_CRASH, WORD]))?;
    let typed5 = restore::ledger_since(&run, t5);
    for row in &typed5 {
        println!("  typed     {row}");
    }
    let cd_typed = typed5
        .iter()
        .any(|r| r.contains(&format!("cd -- {} && ", q(&proj))));
    println!(
        "  cd        shell in {proj}/sub: the line cds first={cd_typed}; relaunched pid {} in {}",
        pid5.map_or_else(|| "none".to_string(), |p| p.to_string()),
        cwd5.as_deref().unwrap_or("(unread)")
    );
    said(
        cd_typed && cwd5.as_deref() == Some(proj.as_str()) && continued5.is_some(),
        "6. relaunched from a shell in another directory, the line cds to the session's workspace",
        &mut failed,
    );
    let Some(pid5) = pid5 else {
        return Ok(Verdict::Failed);
    };

    door.await_idle(2000, 60)?;
    let pid_row = format!("pid = {pid5}");
    let journal = wait_for(30, || {
        restore::desk_naming(&run, &session)
            .filter(|p| crate::util::scratch_text(p).contains(&pid_row))
    })?;
    println!(
        "  restore   the journal names Muse pid {pid5} and its session: {}",
        journal.is_some()
    );
    let before = newest_dump(&run);
    let t_end = restore::end_aterm(&run, &door, "crash", &session)?;
    let (t_launch, _) = restore::relaunch_instance(&mut run, "crash", armed, t_end)?;
    let back = wait_for(120, || {
        resuming(&run, &session, &[pid1, pid2, pid3, pid4, pid5, pid6])
    })?;
    for row in restore::ledger_since(&run, t_launch) {
        println!("  typed     {row}");
    }
    let continued6 = wait_for(120, || request_with(&run, before, &[CONTINUED_HOST, WORD]))?;
    println!(
        "  restored  {}; continued: {}",
        back.as_ref().map_or_else(
            || "NO Muse resumed the session".to_string(),
            |(p, w)| format!(
                "Muse pid {p} after {}: {}",
                restore::since(t_end, now_ms()),
                w.join(" ")
            )
        ),
        continued6.map_or_else(|| "none".to_string(), |n| format!("req-{n:05}"))
    );
    said(
        journal.is_some() && back.is_some() && continued6.is_some(),
        "7. a window restored after aterm crashed brings the Muse tab back on its session",
        &mut failed,
    );
    let Some((pid7, _)) = back else {
        return Ok(Verdict::Failed);
    };

    let door = Door::new(&run)?;
    door.await_idle(2000, 60)?;
    let pid_row = format!("pid = {pid7}");
    let journal8 = wait_for(30, || {
        restore::desk_naming(&run, &session)
            .filter(|p| crate::util::scratch_text(p).contains(&pid_row))
    })?;
    let before8 = newest_dump(&run);
    let t_quit = restore::end_aterm(&run, &door, "quit", &session)?;
    let (t_launch8, _) = restore::relaunch_instance(&mut run, "quit", armed, t_quit)?;
    let back8 = wait_for(120, || {
        resuming(&run, &session, &[pid1, pid2, pid3, pid4, pid5, pid6, pid7])
    })?;
    for row in restore::ledger_since(&run, t_launch8) {
        println!("  typed     {row}");
    }
    let continued8 = wait_for(120, || request_with(&run, before8, &[CONTINUED_HOST, WORD]))?;
    println!(
        "  restored  after the quit: the journal named pid {pid7}={}; {}; continued: {}",
        journal8.is_some(),
        back8.as_ref().map_or_else(
            || "NO Muse resumed the session".to_string(),
            |(p, w)| format!(
                "Muse pid {p} after {}: {}",
                restore::since(t_quit, now_ms()),
                w.join(" ")
            )
        ),
        continued8.map_or_else(|| "none".to_string(), |n| format!("req-{n:05}"))
    );
    said(
        journal8.is_some() && back8.is_some() && continued8.is_some(),
        "8. a window restored after aterm was quit brings the Muse tab back on its session",
        &mut failed,
    );
    if !failed.is_empty() {
        println!("muse-relaunch: FAIL: {}", failed.join("; "));
        Ok(Verdict::Failed)
    } else if unseen.is_empty() {
        println!("muse-relaunch: PASS");
        Ok(Verdict::Resumed)
    } else {
        println!("muse-relaunch: PASS but UNOBSERVED: {}", unseen.join("; "));
        Ok(Verdict::Resumed)
    }
}

/// Entry point for `muse-relaunch [tag] [--keep]`.
pub fn cmd_muse_relaunch(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("muse-relaunch: {m}"));
    let mut tag = None;
    let mut keep = false;
    for a in args {
        match a.as_str() {
            "--keep" => keep = true,
            "-h" | "--help" => {
                print!("{}", crate::USAGE);
                return Ok(0);
            }
            s if s.starts_with('-') => return Err(usage(&format!("unknown option '{s}'"))),
            s => tag = Some(s.to_string()),
        }
    }
    let tag = tag.unwrap_or_else(|| format!("muse-relaunch-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    if is_run(&tag) {
        return Err(usage(&format!(
            "lab '{tag}' is up: muse-relaunch brings up a lab of its own (another tag, or {LAB} down {tag})"
        )));
    }
    stop::install();
    let mut u = crate::lab::UpOpts::from_env();
    u.restore = true;
    u.trust = true;
    u.lifeline = !keep;
    u.harness_keys = format!("{}\ncontinue = false", u.harness_keys);
    if let Err(f) = crate::lab::up(&tag, &u) {
        if let Some(m) = &f.msg {
            eprintln!("aterm-lab: {m}");
        }
        if f.stopped.is_some() {
            return Err(f);
        }
        eprintln!("aterm-lab: muse-relaunch: COULD NOT RUN: the lab did not come up");
        return Ok(3);
    }
    let verdict = run_check(&tag, !keep);
    if keep {
        println!("muse-relaunch: lab '{tag}' left up ({LAB} down {tag})");
    } else {
        let _ = crate::lab::down(&tag, false);
    }
    match verdict? {
        Verdict::Resumed | Verdict::Held => Ok(0),
        Verdict::Failed => Ok(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relaunch_check::words_of;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    /// A Muse's command line is read by its program's path, spaces and all;
    /// the session it resumes off its words.
    #[test]
    fn a_relaunched_muse_names_the_session_it_resumes() {
        let bin = "/Users//u/Library/Application Support/aterm/pkg/store/muse/2001004010006427002/bin/muse";
        let cmd = format!("{bin} --base-url http://127.0.0.1:9 --trust-workspace resume 01a1-s");
        let w = words_of(&cmd, bin).expect("muse's");
        assert_eq!(resumed(&w), Some("01a1-s"));
        assert_eq!(words_of("/bin/zsh -l", bin), None);
        assert_eq!(words_of(&format!("{bin}-x resume 01a1-s"), bin), None);
        assert_eq!(resumed(&words("--base-url u")), None);
        assert_eq!(resumed(&words("resume")), None);
    }

    /// A session log's newest workspace. NEGATIVE CONTROL: a log naming none.
    #[test]
    fn the_newest_workspace_a_log_names() {
        let log = r#"{"workspace_root":"/a"}
{"x":1,"workspace_root":"/private/tmp/at.x/proj","y":2}"#;
        assert_eq!(workspace_in(log).as_deref(), Some("/private/tmp/at.x/proj"));
        assert_eq!(workspace_in("{}"), None);
    }

    /// A lock names its holder's pid; a truncated one names none.
    #[test]
    fn a_lock_names_its_holder() {
        assert_eq!(lock_pid("pid=42\nhost=h\n"), Some(42));
        assert_eq!(lock_pid(""), None);
    }

    /// The shell gets the launcher's environment but the data and config
    /// homes, which the relaunch line carries; the twin execs the build.
    #[test]
    fn the_shell_reaches_the_stub_and_the_twin_execs_the_build() {
        let run = Run {
            tag: "t".into(),
            run: "/s/runs/t".into(),
            w: "/s/runs/t/scratch".into(),
            port: "5".into(),
            proxy: "http://127.0.0.1:6".into(),
            ..Run::default()
        };
        let s = shell_exports(&run);
        for want in [
            "META_API_KEY=",
            "TBH_AUTH_BASE_URL=http://127.0.0.1:5",
            "HTTPS_PROXY=http://127.0.0.1:6",
            "HOME=/s/runs/t/scratch/home",
        ] {
            assert!(s.contains(want), "{want}: {s}");
        }
        assert!(
            !s.contains("XDG_DATA_HOME") && !s.contains("XDG_CONFIG_HOME"),
            "{s}"
        );
        let t = twin_script("/a b/muse");
        assert!(
            t.starts_with("#!/bin/sh\n") && t.contains("exec '/a b/muse' \"$@\"\n"),
            "{t}"
        );
    }
}
