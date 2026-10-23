// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live Gemini CLI relaunch lab (`gemini-relaunch [tag] [--keep]`).

use std::path::PathBuf;
use std::time::Duration;

use crate::door::Door;
use crate::gemini::Agent;
use crate::pyjson::J;
use crate::relaunch_check::{
    block_exit, cwd_of, host_said, newest_dump, relaunch_rows, request_with, said, send_when_free,
    shown, wait_for,
};
use crate::restore::{self, Verdict};
use crate::run::{Run, is_run};
use crate::stop;
use crate::util::{now_ms, shell_quote as q};
use crate::{Fail, LAB, Res};

/// The words each phase's turn asks Gemini CLI to remember.
pub const WORD: &str = "HERON-5150";
/// Phase 4's.
pub const WORD2: &str = "PLOVER-2214";
/// Phase 6's.
pub const WORD3: &str = "GANNET-7702";
/// Phases 7 and 8's.
pub const WORD4: &str = "AVOCET-3391";

/// Continuation prompt typed to Gemini CLI after a crash (`relaunch::resumed_prompt_as`).
pub const CONTINUED_CRASH: &str = "[aterm harness] Relaunched: Gemini CLI exited";

/// …and after aterm's own end.
pub const CONTINUED_HOST: &str = "aterm ended while this session ran";

fn could_not(m: &str) -> Fail {
    Fail::exit(3, format!("gemini-relaunch: COULD NOT RUN: {m}"))
}

/// Whether `w` is Gemini CLI's script: a path named `gemini`, or the bundle.
#[must_use]
pub fn is_script(w: &str) -> bool {
    w.ends_with("/gemini") || w.ends_with("/@google/gemini-cli/bundle/gemini.js")
}

/// A Gemini CLI PARENT's words after its script, from its command line (`node <script> [args]`).
#[must_use]
pub fn parent_words(cmd: &str) -> Option<Vec<String>> {
    let mut it = cmd.split_whitespace();
    let node = it.next()?;
    let script = it.next()?;
    (node.ends_with("/node") && is_script(script)).then(|| it.map(str::to_string).collect())
}

/// The session a Gemini CLI's words resume (`--resume`/`-r <uuid>`, any spelling).
#[must_use]
pub fn resumed(words: &[String]) -> Option<&str> {
    let mut found = None;
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        i += 1;
        match w.split_once('=') {
            Some(("--resume" | "-r", v)) => found = Some(v),
            _ if w == "--resume" || w == "-r" => {
                found = words.get(i).map(String::as_str);
                i += 1;
            }
            _ => {}
        }
    }
    found
}

/// The Gemini CLI parents under the lab's instance, `(pid, words)`.
fn parents(run: &Run) -> Vec<(i32, Vec<String>)> {
    crate::procs::descendants(run.instance_pid)
        .into_iter()
        .filter_map(|pid| parent_words(&crate::procs::command_line(pid)).map(|w| (pid, w)))
        .collect()
}

/// The child a Gemini CLI parent `pid` started.
fn child_of(pid: i32) -> Option<i32> {
    crate::procs::table()
        .into_iter()
        .find(|(_, ppid)| *ppid == pid)
        .map(|(child, _)| child)
}

/// A Gemini CLI parent resuming `session` under the instance other than every pid of `not`.
fn resuming(run: &Run, session: &str, not: &[i32]) -> Option<(i32, Vec<String>)> {
    parents(run)
        .into_iter()
        .find(|(pid, w)| !not.contains(pid) && resumed(w) == Some(session))
}

/// Gemini CLI's scratch `HOME`.
fn gemini_home(run: &Run) -> PathBuf {
    run.w.join(Agent::Gemini.home())
}

/// Read `(session, startTime)` headers under `.gemini/tmp/*/chats/session-*.jsonl`.
fn headers(run: &Run) -> Vec<(String, String)> {
    let tmp = gemini_home(run).join(".gemini/tmp");
    let mut out = Vec::new();
    for slug in std::fs::read_dir(&tmp).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(slug.path().join("chats"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = f.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("session-") && name.ends_with(".jsonl")) {
                continue;
            }
            out.extend(header_lines(&crate::util::scratch_text(&f.path())));
        }
    }
    out
}

/// The session headers of one session file's words, `(session, startTime)`.
#[must_use]
pub fn header_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let j = J::parse(l.trim()).ok()?;
            j.get("projectHash")?;
            Some((
                j.get("sessionId")?.as_str()?.to_string(),
                j.get("startTime")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Session of the newest header begun at or after `since`.
fn newest_session(run: &Run, since: &str) -> Option<String> {
    headers(run)
        .into_iter()
        .filter(|(_, at)| at.as_str() >= since)
        .max_by(|a, b| a.1.cmp(&b.1))
        .map(|(s, _)| s)
}

/// When `session`'s first header was begun (its `startTime`).
fn session_begun(run: &Run, session: &str) -> Option<String> {
    headers(run)
        .into_iter()
        .filter(|(s, _)| s == session)
        .map(|(_, at)| at)
        .min()
}

/// Now as the CLI stamps its headers (`2026-10-18T07:00:00.000Z`).
fn iso_now() -> String {
    let ms = now_ms();
    let iso = crate::util::utc_iso(ms.div_euclid(1000));
    format!(
        "{}{:03}Z",
        iso.strip_suffix("000Z").unwrap_or(&iso),
        ms.rem_euclid(1000)
    )
}

/// Shell environment for Gemini CLI against the stub ([`crate::gemini::launcher`]).
#[must_use]
pub fn shell_exports(run: &Run) -> String {
    let w = run.w.to_string_lossy();
    let shim = run.run.join("shim");
    let gbin = run.run.join("gbin");
    let mut s = String::from("PROMPT='lab$ '\n");
    s.push_str(
        "unset GOOGLE_API_KEY GOOGLE_GENAI_USE_VERTEXAI GOOGLE_CLOUD_PROJECT \
         GOOGLE_APPLICATION_CREDENTIALS HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY \
         all_proxy\n",
    );
    s.push_str(&format!(
        "export PATH={}:{}:\"$PATH\" HOME={} TMPDIR={}\n",
        q(&gbin.to_string_lossy()),
        q(&shim.to_string_lossy()),
        q(&format!("{w}/home")),
        q(&format!("{w}/tmp"))
    ));
    s.push_str(&format!(
        "export GEMINI_API_KEY={} GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:{} NO_BROWSER=true\n",
        crate::gemini::LAB_GEMINI_KEY,
        run.port
    ));
    s.push_str(&format!(
        "export BROWSER={} LAB_OPEN_LOG={} LAB_SECURITY_LOG={}\n",
        q(&shim.join("open").to_string_lossy()),
        q(&format!("{w}/open.log")),
        q(&format!("{w}/security.log"))
    ));
    s
}

/// The lab's `gemini` wrapper.
#[must_use]
pub fn wrapper(bin: &str, elsewhere: Option<&str>) -> String {
    let cd = elsewhere.map_or_else(String::new, |d| format!("cd {} || exit 1\n", q(d)));
    format!(
        "#!/bin/sh\n# aterm-lab gemini-relaunch: the shell's gemini\n{cd}exec {} \"$@\"\n",
        q(bin)
    )
}

/// The line a launch types: the scratch `HOME` Gemini CLI is seeded under, then `gemini` by name.
fn launch_line(run: &Run, args: &[&str]) -> String {
    let mut line = format!("HOME={} gemini", q(&gemini_home(run).to_string_lossy()));
    for a in args {
        line.push(' ');
        line.push_str(&q(a));
    }
    line
}

/// Put the wrapper at `<run>/gbin/gemini`, out of the jail's reach; `down` removes it.
fn put_wrapper(run: &Run, elsewhere: Option<&str>) -> Result<(), Fail> {
    crate::sys::put(
        &run.run.join("gbin/gemini"),
        0o755,
        &crate::sys::PutSrc::Bytes(wrapper(&run.gemini_bin, elsewhere).as_bytes()),
    )
    .map_err(|e| could_not(&format!("the gemini wrapper: {e}")))
}

/// `@<sid>` for a tab, or nothing for the lab's own.
fn at(sid: Option<&str>) -> Vec<String> {
    sid.map(|s| vec![format!("@{s}")]).unwrap_or_default()
}

/// `door.ctl` with the tab first.
fn tab_ctl(door: &Door<'_>, sid: Option<&str>, args: &[&str]) -> Result<crate::door::Reply, Fail> {
    let mut all: Vec<String> = at(sid);
    all.extend(args.iter().map(|a| (*a).to_string()));
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    Ok(door.ctl(&refs)?)
}

/// The tab's screen.
fn text_of(door: &Door<'_>, sid: Option<&str>) -> String {
    let mut all: Vec<String> = at(sid);
    all.push("text".to_string());
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    door.out(&refs)
}

/// The exit of the newest finished block of a tab whose command holds `cmd`.
fn exit_in(door: &Door<'_>, sid: Option<&str>, cmd: &str) -> Option<i64> {
    let mut all: Vec<String> = at(sid);
    all.extend(["blocks", "8", "--json"].map(str::to_string));
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    let reply = door.out(&refs);
    block_exit(reply.strip_prefix("OK ").unwrap_or(&reply), cmd)
}

/// Type `line` and Enter into a tab (no turn guard: a shell line).
fn type_line(door: &Door<'_>, sid: Option<&str>, line: &str) -> Result<(), Fail> {
    tab_ctl(door, sid, &["send", line])?;
    stop::sleep(Duration::from_millis(400))?;
    tab_ctl(door, sid, &["key", "enter"])?;
    Ok(())
}

/// Launch Gemini CLI by name in a tab with `args`.
fn launch(
    run: &Run,
    door: &Door<'_>,
    sid: Option<&str>,
    known: &[i32],
    args: &[&str],
) -> Result<i32, Fail> {
    let before: Vec<i32> = parents(run).into_iter().map(|(p, _)| p).collect();
    type_line(door, sid, &launch_line(run, args))?;
    let pid = wait_for(60, || {
        parents(run)
            .into_iter()
            .find(|(p, _)| !before.contains(p) && !known.contains(p))
            .map(|(p, _)| p)
    })?
    .ok_or_else(|| could_not("no Gemini CLI started"))?;
    if wait_for(60, || {
        text_of(door, sid)
            .contains("Type your message")
            .then_some(())
    })?
    .is_none()
    {
        return Err(could_not("Gemini CLI never drew its composer"));
    }
    stop::sleep(Duration::from_secs(1))?;
    Ok(pid)
}

/// A turn typed into a tab's Gemini CLI, its answer seen at the stub.
fn turn(run: &Run, door: &Door<'_>, sid: Option<&str>, text: &str) -> Result<(), Fail> {
    let before = newest_dump(run);
    let sent = match sid {
        None => crate::verbs::turn(door, text)?,
        Some(_) => {
            tab_ctl(door, sid, &["send", text])?;
            stop::sleep(Duration::from_millis(700))?;
            tab_ctl(door, sid, &["key", "enter"])?;
            true
        }
    };
    if !sent || wait_for(60, || request_with(run, before, &[text]))?.is_none() {
        return Err(could_not(&format!(
            "the turn `{text}` never reached the stub"
        )));
    }
    let _ = tab_ctl(door, sid, &["await", "idle", "2000", "timeout=60000"]);
    Ok(())
}

/// The host's decision since `t` among `needles`, its `step=…` word.
fn decided(run: &Run, t: i64, secs: u64, needles: &[&str]) -> Result<Option<String>, Fail> {
    let line = wait_for(secs, || host_said(run, t, needles))?;
    Ok(line.map(|l| {
        l.split_once("step=")
            .map_or_else(|| l.clone(), |(_, s)| s.to_string())
    }))
}

/// One `gemini-relaunch` run on the lab `tag`, which `cmd_gemini_relaunch` brought up.
#[allow(clippy::too_many_lines)]
fn run_check(tag: &str, armed: bool) -> Result<Verdict, Fail> {
    let mut run = Run::load(tag)?;
    let aterm = restore::seam_checked(&run)?;
    crate::gemini::need(&run, Agent::Gemini).map_err(|f| could_not(&f.msg.unwrap_or_default()))?;
    println!(
        "gemini-relaunch: lab '{tag}', Gemini CLI {} ({}), {aterm}",
        run.gemini_ver, run.gemini_bin
    );
    let mut failed = Vec::new();
    let unseen: Vec<String> = Vec::new();
    run.agent = Agent::Gemini.name().into();
    run.save();
    put_wrapper(&run, None)?;
    crate::gemini::seed(&run, Agent::Gemini, &crate::gemini::Launch::default())?;
    let rc = run.w.join("home/.zshrc");
    crate::sys::put(
        &rc,
        0o644,
        &crate::sys::PutSrc::Bytes(shell_exports(&run).as_bytes()),
    )
    .map_err(|e| could_not(&format!("the shell startup: {e}")))?;
    let door = Door::new(&run)?;
    type_line(&door, None, &format!(". {}", q(&rc.to_string_lossy())))?;
    stop::sleep(Duration::from_millis(500))?;

    let t_launch = iso_now();
    let pid1 = launch(&run, &door, None, &[], &[])?;
    turn(&run, &door, None, &format!("remember the word {WORD}"))?;
    let Some(session) = newest_session(&run, &t_launch) else {
        return Err(could_not("no session header of the launch"));
    };
    let project = cwd_of(pid1);
    println!(
        "  launched  gemini pid {pid1}, session {session}, project {}",
        project.as_deref().unwrap_or("(unread)")
    );
    let journal = wait_for(30, || restore::desk_naming(&run, &session))?;
    println!("  journal   names the session: {}", journal.is_some());
    let Some(child1) = child_of(pid1) else {
        return Err(could_not("the parent has no child"));
    };
    let before = newest_dump(&run);
    let t1 = now_ms();
    let _ = crate::sys::kill(child1, libc::SIGKILL);
    let back = wait_for(90, || resuming(&run, &session, &[pid1]))?;
    let exit1 = exit_in(&door, None, "gemini");
    println!(
        "  crash     SIGKILL child {child1} of pid {pid1}: its block's exit {}",
        shown(exit1)
    );
    let Some((pid2, words2)) = back else {
        for row in restore::ledger_since(&run, t1) {
            println!("  typed     {row}");
        }
        said(
            false,
            "1. the child's death relaunched on its session within 90 s",
            &mut failed,
        );
        return Ok(Verdict::Failed);
    };
    let cwd2 = cwd_of(pid2);
    let continued = wait_for(120, || request_with(&run, before, &[CONTINUED_CRASH, WORD]))?;
    let typed = restore::ledger_since(&run, t1);
    for row in &typed {
        println!("  typed     {row}");
    }
    let line = typed.join("\n");
    let by_name =
        line.contains("'gemini' '--resume'") && !line.contains("Cellar") && !line.contains("/node");
    println!(
        "  relaunch  pid {pid2} after {} in {}: {}; continued: {}",
        restore::since(t1, now_ms()),
        cwd2.as_deref().unwrap_or("(unread)"),
        words2.join(" "),
        continued.map_or_else(
            || "NONE within 120 s".to_string(),
            |n| format!("req-{n:05}")
        )
    );
    said(
        exit1 == Some(1)
            && journal.is_some()
            && by_name
            && project.is_some()
            && cwd2 == project
            && continued.is_some(),
        "1. its child SIGKILLed (1), Gemini CLI is resumed on its session in its project by the shell's `gemini`; the stub sees the prior turn",
        &mut failed,
    );

    let _ = door.ctl(&["await", "idle", "2000", "timeout=60000"]);
    let Some(child2) = child_of(pid2) else {
        return Err(could_not("the relaunched parent has no child"));
    };
    let before = newest_dump(&run);
    let t2 = now_ms();
    let _ = crate::sys::kill(pid2, libc::SIGKILL);
    stop::sleep(Duration::from_millis(300))?;
    let orphaned = crate::procs::alive(child2);
    let back2 = wait_for(120, || resuming(&run, &session, &[pid1, pid2]))?;
    let exit2 = exit_in(&door, None, "'--resume'");
    let orphan_gone = crate::procs::wait_gone(child2, Duration::from_secs(10));
    let continued2 = wait_for(120, || request_with(&run, before, &[CONTINUED_CRASH, WORD]))?;
    let pid3 = back2.as_ref().map(|(p, _)| *p);
    println!(
        "  crash 2   SIGKILL parent {pid2}: its relaunch line's block exit {}; its child {child2} orphaned={orphaned}, gone after the line={orphan_gone}; relaunched {}; continued: {}",
        shown(exit2),
        pid3.map_or_else(
            || "NOTHING within 120 s".to_string(),
            |p| format!("pid {p} after {}", restore::since(t2, now_ms()))
        ),
        continued2.map_or_else(|| "none".to_string(), |n| format!("req-{n:05}"))
    );
    said(
        exit2 == Some(137) && orphaned && orphan_gone && pid3.is_some() && continued2.is_some(),
        "2. its parent SIGKILLed (137), the orphaned child waited out of the terminal, the session is resumed again",
        &mut failed,
    );
    let Some(pid3) = pid3 else {
        return Ok(Verdict::Failed);
    };

    let _ = door.ctl(&["await", "idle", "2000", "timeout=60000"]);
    stop::sleep(Duration::from_secs(3))?;
    let t3 = now_ms();
    let quit = send_when_free(&door, "/quit")?;
    let gone3 = crate::procs::wait_gone(pid3, Duration::from_secs(20));
    stop::sleep(Duration::from_secs(2))?;
    let exit3 = exit_in(&door, None, "'--resume'");
    let decided3 = decided(
        &run,
        t3,
        130,
        &[
            "step=ended:",
            "step=adopted",
            "not relaunched",
            "step=refused:",
        ],
    )?;
    let again3 = resuming(&run, &session, &[pid1, pid2, pid3]);
    let rows3 = relaunch_rows(&run, t3);
    println!(
        "  quit      `/quit` typed={quit}, pid {pid3} gone={gone3}, its block's exit {}; the host: {}; relaunch rows since: {rows3}",
        shown(exit3),
        decided3.as_deref().unwrap_or("NONE within 130 s")
    );
    said(
        quit && gone3
            && exit3 == Some(0)
            && decided3
                .as_deref()
                .is_some_and(|w| w.starts_with("ended:graceful-exit"))
            && again3.is_none()
            && rows3 == 0,
        "3. a clean /quit (0) is never relaunched: the host decided `ended:graceful-exit`",
        &mut failed,
    );

    let known4 = [pid1, pid2, pid3];
    let t_launch4 = iso_now();
    let pid4 = launch(&run, &door, None, &known4, &[])?;
    turn(&run, &door, None, &format!("remember the word {WORD2}"))?;
    let session4 = newest_session(&run, &t_launch4)
        .ok_or_else(|| could_not("no session header of the fresh launch"))?;
    let named4 = wait_for(30, || restore::desk_naming(&run, &session4))?;
    let elsewhere = run.w.join("tmp");
    put_wrapper(&run, Some(&elsewhere.to_string_lossy()))?;
    let child4 = child_of(pid4).ok_or_else(|| could_not("the fresh parent has no child"))?;
    let t4 = now_ms();
    let _ = crate::sys::kill(child4, libc::SIGKILL);
    let decided4 = decided(
        &run,
        t4,
        150,
        &[
            "step=refused:",
            "step=adopted",
            "step=ended:",
            "step=failed:",
        ],
    )?;
    let exit4 = exit_in(&door, None, "'--resume'");
    let rows4 = relaunch_rows(&run, t4);
    stop::sleep(Duration::from_secs(75))?;
    let rows4_later = relaunch_rows(&run, t4);
    let told4 = host_said(
        &run,
        t4,
        &[&format!("gemini --resume {session4} in this tab")],
    );
    for row in restore::ledger_since(&run, t4) {
        println!("  typed     {row}");
    }
    println!(
        "  refused   pid {pid4} on {session4} (journaled={}), `gemini` run from {}: the host decided {}; the line's block exit {}; relaunch rows {rows4}, {rows4_later} after 75 s more; told its own line: {}",
        named4.is_some(),
        elsewhere.display(),
        decided4.as_deref().unwrap_or("NOTHING within 150 s"),
        shown(exit4),
        told4.is_some()
    );
    said(
        decided4
            .as_deref()
            .is_some_and(|w| w.starts_with("refused:resume-refused"))
            && exit4 == Some(42)
            && rows4 == 1
            && rows4_later == 1
            && told4.is_some(),
        "4. a resume Gemini CLI refused (42) is typed once and never again (`refused:resume-refused`), its own line told",
        &mut failed,
    );
    put_wrapper(&run, None)?;

    let Some(proj) = project.clone() else {
        return Ok(Verdict::Failed);
    };
    let together = beside(&run, &door, &proj, &mut failed)?;
    println!("  together  {together}");

    hazard(&run, &door, &proj, &mut failed)?;

    let mut known: Vec<i32> = vec![pid1, pid2, pid3, pid4];
    let t_launch7 = iso_now();
    let pid7 = launch(&run, &door, None, &known, &[])?;
    turn(&run, &door, None, &format!("remember the word {WORD4}"))?;
    let session7 = newest_session(&run, &t_launch7)
        .ok_or_else(|| could_not("no session header of the restore's launch"))?;
    known.push(pid7);
    let pid_row = format!("pid = {pid7}");
    let journal7 = wait_for(30, || {
        restore::desk_naming(&run, &session7)
            .filter(|p| crate::util::scratch_text(p).contains(&pid_row))
    })?;
    let before7 = newest_dump(&run);
    let t_end = restore::end_aterm(&run, &door, "crash", &session7)?;
    let (t_up, _) = restore::relaunch_instance(&mut run, "crash", armed, t_end)?;
    let back7 = wait_for(120, || resuming(&run, &session7, &known))?;
    for row in restore::ledger_since(&run, t_up) {
        println!("  typed     {row}");
    }
    let continued7 = wait_for(120, || {
        request_with(&run, before7, &[CONTINUED_HOST, WORD4])
    })?;
    println!(
        "  restored  after the crash: the journal named pid {pid7}={}; {}; continued: {}",
        journal7.is_some(),
        back7.as_ref().map_or_else(
            || "NO Gemini CLI resumed the session".to_string(),
            |(p, w)| format!(
                "pid {p} after {}: {}",
                restore::since(t_end, now_ms()),
                w.join(" ")
            )
        ),
        continued7.map_or_else(|| "none".to_string(), |n| format!("req-{n:05}"))
    );
    said(
        journal7.is_some() && back7.is_some() && continued7.is_some(),
        "7. a window restored after aterm crashed brings the Gemini CLI tab back on its session",
        &mut failed,
    );
    let Some((pid8, _)) = back7 else {
        return Ok(Verdict::Failed);
    };
    known.push(pid8);

    let door = Door::new(&run)?;
    let _ = door.ctl(&["await", "idle", "2000", "timeout=60000"]);
    let pid_row = format!("pid = {pid8}");
    let journal8 = wait_for(30, || {
        restore::desk_naming(&run, &session7)
            .filter(|p| crate::util::scratch_text(p).contains(&pid_row))
    })?;
    let before8 = newest_dump(&run);
    let t_quit = restore::end_aterm(&run, &door, "quit", &session7)?;
    let (t_up8, _) = restore::relaunch_instance(&mut run, "quit", armed, t_quit)?;
    let back8 = wait_for(120, || resuming(&run, &session7, &known))?;
    for row in restore::ledger_since(&run, t_up8) {
        println!("  typed     {row}");
    }
    let continued8 = wait_for(120, || {
        request_with(&run, before8, &[CONTINUED_HOST, WORD4])
    })?;
    println!(
        "  restored  after the quit: the journal named pid {pid8}={}; {}; continued: {}",
        journal8.is_some(),
        back8.as_ref().map_or_else(
            || "NO Gemini CLI resumed the session".to_string(),
            |(p, w)| format!(
                "pid {p} after {}: {}",
                restore::since(t_quit, now_ms()),
                w.join(" ")
            )
        ),
        continued8.map_or_else(|| "none".to_string(), |n| format!("req-{n:05}"))
    );
    said(
        journal8.is_some() && back8.is_some() && continued8.is_some(),
        "8. a window restored after aterm was quit brings the Gemini CLI tab back on its session",
        &mut failed,
    );
    if let Some((p, _)) = back8 {
        let door = Door::new(&run)?;
        let _ = door.ctl(&["await", "idle", "2000", "timeout=60000"]);
        let _ = send_when_free(&door, "/quit");
        let _ = crate::procs::wait_gone(p, Duration::from_secs(20));
    }

    if !failed.is_empty() {
        println!("gemini-relaunch: FAIL: {}", failed.join("; "));
        Ok(Verdict::Failed)
    } else if unseen.is_empty() {
        println!("gemini-relaunch: PASS");
        Ok(Verdict::Resumed)
    } else {
        println!(
            "gemini-relaunch: PASS but UNOBSERVED: {}",
            unseen.join("; ")
        );
        Ok(Verdict::Resumed)
    }
}

/// A tab of its own in `proj`, its shell given the lab's exports: its sid.
fn open_tab(run: &Run, door: &Door<'_>, proj: &str) -> Result<String, Fail> {
    let spawned = door.ctl(&["spawn", &format!("cwd={proj}"), "raise=false"])?;
    let sid = spawned
        .stdout
        .trim()
        .strip_prefix("OK ")
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_string)
        .ok_or_else(|| could_not(&format!("spawn: {}", spawned.stdout.trim())))?;
    tab_ctl(door, Some(&sid), &["await", "idle", "400", "timeout=30000"])?;
    let rc = run.w.join("home/.zshrc");
    type_line(door, Some(&sid), &format!(". {}", q(&rc.to_string_lossy())))?;
    stop::sleep(Duration::from_millis(500))?;
    Ok(sid)
}

/// Phase 5: two Gemini CLIs started together, each in a tab of its own; one's child SIGKILLed.
fn beside(
    run: &Run,
    door: &Door<'_>,
    proj: &str,
    failed: &mut Vec<String>,
) -> Result<String, Fail> {
    let a = open_tab(run, door, proj)?;
    let b = open_tab(run, door, proj)?;
    let before: Vec<i32> = parents(run).into_iter().map(|(p, _)| p).collect();
    let line = launch_line(run, &[]);
    tab_ctl(door, Some(&a), &["send", &line])?;
    tab_ctl(door, Some(&b), &["send", &line])?;
    tab_ctl(door, Some(&a), &["key", "enter"])?;
    tab_ctl(door, Some(&b), &["key", "enter"])?;
    let pair = wait_for(60, || {
        let new: Vec<i32> = parents(run)
            .into_iter()
            .map(|(p, _)| p)
            .filter(|p| !before.contains(p))
            .collect();
        (new.len() == 2).then_some(new)
    })?
    .ok_or_else(|| could_not("the two Gemini CLIs did not both start"))?;
    for sid in [&a, &b] {
        let _ = wait_for(60, || {
            text_of(door, Some(sid))
                .contains("Type your message")
                .then_some(())
        })?;
    }
    stop::sleep(Duration::from_secs(5))?;
    let starts: Vec<String> = pair.iter().map(|p| crate::procs::start_time(*p)).collect();
    let victim = pair[0];
    let other = pair[1];
    let child = child_of(victim).ok_or_else(|| could_not("a parent of the pair has no child"))?;
    let t = now_ms();
    let _ = crate::sys::kill(child, libc::SIGKILL);
    let word = decided(
        run,
        t,
        130,
        &[
            "step=refused:",
            "step=adopted",
            "step=ended:",
            "step=failed:",
        ],
    )?;
    let rows = relaunch_rows(run, t);
    let line_said = host_said(run, t, &["could not tell which session"]);
    let said_line = format!(
        "pids {pair:?} started {starts:?}; SIGKILL child {child} of {victim}: the host decided {}; relaunch rows {rows}; told: {}",
        word.as_deref().unwrap_or("NOTHING within 130 s"),
        line_said.is_some()
    );
    said(
        word.as_deref()
            .is_some_and(|w| w.starts_with("refused:no-witness"))
            && rows == 0
            && line_said.is_some(),
        "5. two Gemini CLIs started together in one project: neither's session is resumed on a guess (`refused:no-witness`), the person told",
        failed,
    );
    for sid in [&a, &b] {
        let _ = send_when_free_in(door, sid, "/quit");
    }
    let _ = crate::procs::wait_gone(other, Duration::from_secs(20));
    for sid in [&a, &b] {
        let _ = tab_ctl(door, Some(sid), &["close"]);
    }
    Ok(said_line)
}

/// [`send_when_free`] in a tab of its own.
fn send_when_free_in(door: &Door<'_>, sid: &str, text: &str) -> Result<bool, Fail> {
    for _ in 0..60 {
        let r = tab_ctl(door, Some(sid), &["send", text])?;
        if r.code == 0 && !r.stdout.starts_with("ERR") {
            stop::sleep(Duration::from_millis(700))?;
            return Ok(tab_ctl(door, Some(sid), &["key", "enter"])?.code == 0);
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    Ok(false)
}

/// Details of `.lost`/`.gone` Messages lines for `tab` since `ms`.
fn supervisor_lines(run: &Run, tab: &str, ms: i64) -> Vec<String> {
    let field = |line: &str, name: &str| -> Option<String> {
        line.split('\t')
            .find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
            .map(str::to_string)
    };
    let keys = [
        format!("harness.supervisor.{tab}.lost"),
        format!("harness.supervisor.{tab}.gone"),
    ];
    crate::util::scratch_text(&run.state().join("logs/messages.log"))
        .lines()
        .filter(|l| field(l, "kind").as_deref() == Some("posted"))
        .filter(|l| field(l, "key").is_some_and(|k| keys.contains(&k)))
        .filter(|l| {
            field(l, "t")
                .and_then(|at| at.parse::<i64>().ok())
                .is_some_and(|at| at >= ms)
        })
        .filter_map(|l| field(l, "detail"))
        .collect()
}

/// The UTC minute an ISO stamp names (`2026-10-18T07:00`).
fn minute(iso: &str) -> String {
    iso.get(..16).unwrap_or(iso).to_string()
}

/// Phase 6, the G5 hazard, in a tab of its own: a turn, quit.
#[allow(clippy::too_many_lines)]
fn hazard(run: &Run, door: &Door<'_>, proj: &str, failed: &mut Vec<String>) -> Result<(), Fail> {
    let tab = open_tab(run, door, proj)?;
    let sid = Some(tab.as_str());
    let t_launch = iso_now();
    let first = launch(run, door, sid, &[], &[])?;
    turn(run, door, sid, &format!("remember the word {WORD3}"))?;
    let session = newest_session(run, &t_launch)
        .ok_or_else(|| could_not("no session header of the hazard's launch"))?;
    send_when_free_in(door, &tab, "/quit")?;
    if !crate::procs::wait_gone(first, Duration::from_secs(20)) {
        return Err(could_not("the hazard's first Gemini CLI never quit"));
    }
    let begun = session_begun(run, &session)
        .ok_or_else(|| could_not("no header of the hazard's session"))?;
    let launched_in = minute(&begun);
    if wait_for(75, || (minute(&iso_now()) != launched_in).then_some(()))?.is_none() {
        return Err(could_not("the minute never turned"));
    }
    stop::sleep(Duration::from_secs(1))?;
    let resume1 = launch(run, door, sid, &[first], &["--resume", &session])?;
    send_when_free_in(door, &tab, "/quit")?;
    if !crate::procs::wait_gone(resume1, Duration::from_secs(20)) {
        return Err(could_not("the hazard's first resume never quit"));
    }
    let resume2 = launch(run, door, sid, &[first, resume1], &["--resume", &session])?;
    stop::sleep(Duration::from_secs(4))?;
    let child = child_of(resume2).ok_or_else(|| could_not("the second resume has no child"))?;
    let before = newest_dump(run);
    let t = now_ms();
    let _ = crate::sys::kill(child, libc::SIGKILL);
    let word = decided(
        run,
        t,
        130,
        &[
            "step=refused:",
            "step=adopted",
            "step=ended:",
            "step=failed:",
        ],
    )?;
    stop::sleep(Duration::from_secs(5))?;
    let rows = relaunch_rows(run, t);
    let continued = request_with(run, before, &[CONTINUED_CRASH]);
    let again = resuming(run, &session, &[first, resume1, resume2]);
    let told = host_said(run, t, &["without some of its turns"]);
    let lines = wait_for(10, || {
        let lines = supervisor_lines(run, &tab, t);
        (!lines.is_empty()).then_some(lines)
    })?
    .unwrap_or_default();
    let resume_said = lines.iter().any(|l| l.contains("resume its conversation"));
    println!(
        "  hazard    session {session}: a turn, /quit, the minute turned, resumed (pid {resume1}) and quit, resumed (pid {resume2}) and its child {child} SIGKILLed: the host decided {}; relaunch rows {rows}; a relaunched one: {}; a continuation request: {}; told: {}; its Messages line: {}",
        word.as_deref().unwrap_or("NOTHING within 130 s"),
        again
            .as_ref()
            .map_or_else(|| "none".to_string(), |(p, _)| p.to_string()),
        continued.map_or_else(|| "none".to_string(), |n| format!("req-{n:05}")),
        told.is_some(),
        lines.last().map_or("none", String::as_str)
    );
    said(
        word.as_deref()
            .is_some_and(|w| w.starts_with("refused:history-unsafe"))
            && rows == 0
            && again.is_none()
            && continued.is_none()
            && told.is_some()
            && !lines.is_empty()
            && !resume_said,
        "6. the session's newest file an empty duplicate (G5): its resume is never typed (`refused:history-unsafe`), the person told, never to resume it",
        failed,
    );
    let _ = tab_ctl(door, sid, &["close"]);
    Ok(())
}

/// `gemini-relaunch [tag] [--keep]`.
pub fn cmd_gemini_relaunch(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("gemini-relaunch: {m}"));
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
    let tag = tag.unwrap_or_else(|| format!("gemini-relaunch-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    if is_run(&tag) {
        return Err(usage(&format!(
            "lab '{tag}' is up: gemini-relaunch brings up a lab of its own (another tag, or {LAB} down {tag})"
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
        eprintln!("aterm-lab: gemini-relaunch: COULD NOT RUN: the lab did not come up");
        return Ok(3);
    }
    let verdict = run_check(&tag, !keep);
    if keep {
        println!("gemini-relaunch: lab '{tag}' left up ({LAB} down {tag})");
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

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    /// A Gemini CLI parent is read by its node and its script, typed by name
    /// or by path.
    #[test]
    fn a_gemini_parent_is_read_by_its_node_and_its_script() {
        assert_eq!(
            parent_words("/opt/homebrew/opt/node/bin/node /opt/homebrew/bin/gemini --resume s1"),
            Some(words("--resume s1"))
        );
        assert_eq!(
            parent_words(
                "/opt/homebrew/opt/node/bin/node /opt/homebrew/Cellar/gemini-cli/0.46.0/libexec/lib/node_modules/@google/gemini-cli/bundle/gemini.js"
            ),
            Some(Vec::new())
        );
        assert_eq!(
            parent_words(
                "/opt/homebrew/Cellar/node/26.3.0/bin/node --max-old-space-size=18432 /opt/homebrew/bin/gemini"
            ),
            None
        );
        assert_eq!(
            parent_words("/opt/homebrew/opt/node/bin/node /w/server.js"),
            None
        );
    }

    /// A Gemini CLI's words name the session it resumes, any spelling.
    #[test]
    fn a_relaunched_gemini_names_the_session_it_resumes() {
        assert_eq!(resumed(&words("--model x --resume s1")), Some("s1"));
        assert_eq!(resumed(&words("-r=s1")), Some("s1"));
        assert_eq!(resumed(&words("--model x")), None);
        assert_eq!(resumed(&words("-i --resume")), None);
    }

    /// The headers of a session file, its duplicate appended too. NEGATIVE
    /// CONTROLS: a message and a `$set` are no headers.
    #[test]
    fn a_session_files_headers_are_read() {
        let text = r#"{"sessionId":"s1","projectHash":"h","startTime":"2026-10-18T07:00:00.000Z","lastUpdated":"x","kind":"main"}
{"id":"u1","timestamp":"t","type":"user","content":"hi"}
{"$set":{"lastUpdated":"y"}}
{"sessionId":"s1","projectHash":"h","startTime":"2026-10-18T07:00:30.000Z","lastUpdated":"x","kind":"main"}
"#;
        assert_eq!(
            header_lines(text),
            [
                ("s1".to_string(), "2026-10-18T07:00:00.000Z".to_string()),
                ("s1".to_string(), "2026-10-18T07:00:30.000Z".to_string())
            ]
        );
        assert_eq!(minute("2026-10-18T07:00:30.000Z"), "2026-10-18T07:00");
    }

    /// The shell reaches the stub with no proxy, its `gemini` the lab's
    /// wrapper ahead of Homebrew's; the wrapper execs the PATH script, from
    /// another directory where told.
    #[test]
    fn the_shell_reaches_the_stub_and_the_wrapper_execs_the_path_script() {
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
            "GEMINI_API_KEY=",
            "GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:5",
            "NO_BROWSER=true",
            "PATH=/s/runs/t/gbin:/s/runs/t/shim:\"$PATH\" ",
            "HOME=/s/runs/t/scratch/home ",
            "unset GOOGLE_API_KEY",
        ] {
            assert!(s.contains(want), "{want}: {s}");
        }
        assert!(!s.contains("HTTPS_PROXY=http"), "{s}");
        assert!(
            !s.contains("homebrew"),
            "no Homebrew prefix on the lane's PATH: {s}"
        );
        assert_eq!(
            wrapper("/l/gemini-cli/bundle/gemini.js", None),
            "#!/bin/sh\n# aterm-lab gemini-relaunch: the shell's gemini\nexec /l/gemini-cli/bundle/gemini.js \"$@\"\n"
        );
        assert!(wrapper("/a b/gemini", None).contains("exec '/a b/gemini' \"$@\"\n"));
        assert!(wrapper("/g", Some("/x y")).contains("cd '/x y' || exit 1\n"));
        assert_eq!(
            launch_line(&run, &["--resume", "s1"]),
            "HOME=/s/runs/t/scratch/gemini-home gemini --resume s1"
        );
    }
}
