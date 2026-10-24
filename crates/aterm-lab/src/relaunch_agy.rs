// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agy-relaunch [tag] [--keep]`: THE AGY LANE, LIVE — the real agy (the lab's, launched by the seeded `agy` verb: its scratch `agy-home` holds the Gemini API provider and the onboarding done, so no sign-in screen draws and no browser opens) under a private headless aterm with the harness ON and `[harness] relaunch` at its default, against the stub's Gemini API.

use std::path::Path;
use std::time::Duration;

use crate::door::Door;
use crate::gemini::Agent;
use crate::relaunch_check::{
    cwd_of, exit_of, host_said, newest_dump, open_files, relaunch_rows, request_with, running,
    said, send_when_free, shown, unobserved, wait_for,
};
use crate::restore::{self, Verdict};
use crate::run::{Run, is_run};
use crate::stop;
use crate::util::now_ms;
use crate::{Fail, LAB, Res};

/// The word the first turn asks agy to remember: the request after a relaunch carries it only where the conversation came back with its turns.
pub const WORD: &str = "PELICAN-4417";

/// The word the fresh agy of phase 5 is asked to remember.
pub const WORD2: &str = "OSPREY-6620";

/// The flag the refusing build adds: agy 1.4.0 refuses an unknown flag with `flags provided but not defined` and exits 2 before any TUI (measured).
pub const REFUSED_FLAG: &str = "--lab-refused-flag";

/// The app data directory the fresh-starting build reads.
pub const FRESH_DIR: &str = "lab-fresh";

/// The harness's continuation as agy is typed it, after a crash (`relaunch::resumed_prompt_as`, agy's product name).
pub const CONTINUED_CRASH: &str = "[aterm harness] Relaunched: Antigravity CLI exited";

/// …and after aterm's own end.
pub const CONTINUED_HOST: &str = "aterm ended while this session ran";

fn could_not(m: &str) -> Fail {
    Fail::exit(3, format!("agy-relaunch: COULD NOT RUN: {m}"))
}

/// The conversation an agy's words resume (`--conversation`, any spelling).
#[must_use]
pub fn resumed(words: &[String]) -> Option<&str> {
    let mut found = None;
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        i += 1;
        let bare = w.strip_prefix("--").or_else(|| w.strip_prefix('-'));
        match bare.map(|b| b.split_once('=').unwrap_or((b, ""))) {
            Some(("conversation", "")) => {
                found = words.get(i).map(String::as_str);
                i += 1;
            }
            Some(("conversation", value)) => found = Some(value),
            _ => {}
        }
    }
    found
}

/// The agy processes under the lab's instance, `(pid, words)`.
fn agys(run: &Run) -> Vec<(i32, Vec<String>)> {
    running(run, &run.agy_bin)
}

/// agy's config under its scratch home (`agy-home/.gemini/<app>`).
fn config(run: &Run, app: &str) -> std::path::PathBuf {
    run.w.join(Agent::Agy.home()).join(".gemini").join(app)
}

/// The conversation of the presence lock a path names under `config` (`<config>/presence/<uuid>.lock`).
#[must_use]
pub fn lock_conversation(config: &Path, path: &str) -> Option<String> {
    let rest = Path::new(path).strip_prefix(config).ok()?;
    let mut parts = rest.components().map(|c| c.as_os_str().to_string_lossy());
    let (dir, file) = (parts.next()?, parts.next()?);
    (dir == "presence" && parts.next().is_none())
        .then(|| file.strip_suffix(".lock").map(str::to_owned))
        .flatten()
        .filter(|c| c.len() == 36)
}

/// The conversation whose presence lock `pid` holds open (its descriptors), under the
/// default config.
fn conversation_of(run: &Run, pid: i32) -> Option<String> {
    let config = std::fs::canonicalize(config(run, "antigravity-cli")).ok()?;
    let held: Vec<String> = open_files(pid)
        .iter()
        .filter_map(|f| lock_conversation(&config, f))
        .collect();
    match held.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// An agy resuming `conv` under the instance other than every pid of `not`.
fn resuming(run: &Run, conv: &str, not: &[i32]) -> Option<(i32, Vec<String>)> {
    agys(run)
        .into_iter()
        .find(|(pid, w)| !not.contains(pid) && resumed(w) == Some(conv))
}

/// The shell environment a relaunched agy reaches the stub with, as the lab's launcher gives it agy ([`crate::gemini::launcher`]) but its `HOME`, which the relaunch line carries from the process.
#[must_use]
pub fn shell_exports(run: &Run) -> String {
    use crate::util::shell_quote as q;
    let w = run.w.to_string_lossy();
    let shim = run.run.join("shim");
    let px = q(&run.proxy);
    let mut s = String::from("PROMPT='lab$ '\n");
    s.push_str(
        "unset GOOGLE_API_KEY GOOGLE_GENAI_USE_VERTEXAI GOOGLE_CLOUD_PROJECT \
         GOOGLE_APPLICATION_CREDENTIALS\n",
    );
    s.push_str(&format!(
        "export PATH={}:\"$PATH\" HOME={} TMPDIR={}\n",
        q(&shim.to_string_lossy()),
        q(&format!("{w}/home")),
        q(&format!("{w}/tmp"))
    ));
    s.push_str(&format!(
        "export HTTP_PROXY={px} HTTPS_PROXY={px} http_proxy={px} https_proxy={px} ALL_PROXY={px} all_proxy={px}\n"
    ));
    s.push_str("export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost\n");
    s.push_str(&format!(
        "export GEMINI_API_KEY={} GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:{}\n",
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

/// atpkg's twin for `build`: its export, then `exec '<build>' "$@"`.
#[must_use]
pub fn twin_script(build: &str) -> String {
    format!(
        "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n# aterm-lab agy-relaunch: the lab's build as the managed twin (crates/aterm-lab)\nexport AGY_CLI_DISABLE_AUTO_UPDATE='true'\nexec '{}' \"$@\"\n",
        build.replace('\'', "'\\''")
    )
}

/// A stand-in build named `agy` that runs the lab's build with `flag` first: what a twin execs to make a relaunch refused, or start fresh.
#[must_use]
pub fn build_with(bin: &str, flag: &str) -> String {
    format!(
        "#!/bin/sh\nexec '{}' '{flag}' \"$@\"\n",
        bin.replace('\'', "'\\''")
    )
}

/// A tab of its own in `proj`, its shell given the environment the lab's
/// launcher gives agy but its `HOME` (as the first tab's is, so a relaunch
/// typed there reaches the stub), and an agy launched in it by the lab's
/// seeded launcher with `args` (the verb's scratch `agy-home`, its provider
/// and onboarding already seeded by the first launch): its tab and pid once
/// that agy draws its composer.
fn open_tab(run: &Run, proj: &str, known: &[i32], args: &[&str]) -> Result<(String, i32), Fail> {
    let door = Door::new(run)?;
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let script = run.run.join(format!("agy-tab-{}.sh", args.len()));
    crate::sys::put(
        &script,
        0o755,
        &crate::sys::PutSrc::Bytes(crate::gemini::launcher(run, Agent::Agy, &[], &args).as_bytes()),
    )
    .map_err(|e| could_not(&format!("a tab's launcher: {e}")))?;
    let spawned = door.ctl(&["spawn", &format!("cwd={proj}"), "raise=false"])?;
    let sid = spawned
        .stdout
        .trim()
        .strip_prefix("OK ")
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_string)
        .ok_or_else(|| could_not(&format!("spawn: {}", spawned.stdout.trim())))?;
    let at = format!("@{sid}");
    door.ctl(&[&at, "await", "idle", "400", "timeout=30000"])?;
    let rc = run.w.join("home/.zshrc");
    for line in [
        format!(". {}", crate::util::shell_quote(&rc.to_string_lossy())),
        format!("sh {}", crate::util::shell_quote(&script.to_string_lossy())),
    ] {
        door.ctl(&[&at, "send", &line])?;
        door.ctl(&[&at, "key", "enter"])?;
        stop::sleep(Duration::from_millis(500))?;
    }
    let pid = wait_for(60, || {
        agys(run)
            .into_iter()
            .find(|(p, w)| !known.contains(p) && *w == args)
            .map(|(p, _)| p)
    })?
    .ok_or_else(|| could_not("a tab's agy never started"))?;
    let drawn = wait_for(60, || {
        let text = door.out(&[&at, "text"]);
        (text.contains("Antigravity CLI") && text.lines().any(|l| l.starts_with('>'))).then_some(())
    })?;
    if drawn.is_none() {
        return Err(could_not("a tab's agy never drew its composer"));
    }
    Ok((sid, pid))
}

/// The word the agy of phase 2a is asked to remember.
pub const WORD3: &str = "EGRET-3301";

/// Phases 2a and 2b: a second agy opened by `-c` beside an agy of its own, each in a tab of
/// its own in `proj`, keeps the first's crash from being relaunched while it runs (2a); once it
/// quits, the crash is relaunched on its conversation (2b). `known`, the agy processes the check
/// has seen.
#[allow(clippy::too_many_lines)]
fn beside(run: &Run, proj: &str, known: &[i32], failed: &mut Vec<String>) -> Result<(), Fail> {
    let door = Door::new(run)?;
    let (tab_b, holder_b) = open_tab(run, proj, known, &[])?;
    let at_b = format!("@{tab_b}");
    let before = newest_dump(run);
    door.ctl(&[&at_b, "send", &format!("remember the word {WORD3}")])?;
    stop::sleep(Duration::from_millis(700))?;
    door.ctl(&[&at_b, "key", "enter"])?;
    let answered = wait_for(60, || request_with(run, before, &[WORD3]))?;
    let Some(conv_b) = wait_for(60, || conversation_of(run, holder_b))? else {
        return Err(could_not("the second tab's agy holds no presence lock"));
    };
    let journaled = wait_for(30, || restore::desk_naming(run, &conv_b))?.is_some();
    let mut seen = known.to_vec();
    seen.push(holder_b);
    let (tab_c, opener) = open_tab(run, proj, &seen, &["-c"])?;
    seen.push(opener);
    let at_c = format!("@{tab_c}");
    let drawn = wait_for(60, || {
        door.out(&[&at_c, "text"])
            .contains("Conversation already open")
            .then_some(())
    })?
    .is_some();
    let lockless = conversation_of(run, opener).is_none();
    let db = format!("/conversations/{conv_b}.db");
    let holds_db = open_files(opener).iter().any(|f| f.ends_with(&db));
    println!(
        "  beside    agy pid {holder_b} in tab {tab_b} on conversation {conv_b} (its turn reached the stub={}, the journal names it={journaled}); `agy -c` pid {opener} in tab {tab_c}: `Conversation already open` drawn={drawn}, holds a presence lock={}, holds that conversation's database={holds_db}",
        answered.is_some(),
        !lockless
    );
    door.ctl(&[&at_b, "await", "idle", "2000", "timeout=60000"])?;
    let before = newest_dump(run);
    let t = now_ms();
    let _ = crate::sys::kill(holder_b, libc::SIGKILL);
    let waited = wait_for(60, || {
        host_said(
            run,
            t,
            &[
                "step=wait:conversation-held",
                "step=adopted",
                "step=refused:",
                "step=failed:",
                "step=ended:",
            ],
        )
    })?;
    let early = resuming(run, &conv_b, &seen);
    let rows_early = relaunch_rows(run, t);
    let opener_alive = crate::procs::alive(opener);
    println!(
        "  held      SIGKILL pid {holder_b}, `agy -c` alive={opener_alive}: the host: {}; an agy resuming it since: {}; relaunch rows: {rows_early}",
        waited.as_deref().map_or("NONE within 60 s", |l| l
            .split_once(": harness @")
            .map_or(l, |(_, s)| s)),
        early
            .as_ref()
            .map_or_else(|| "none".to_string(), |(p, _)| p.to_string())
    );
    said(
        journaled
            && drawn
            && lockless
            && holds_db
            && opener_alive
            && waited
                .as_deref()
                .is_some_and(|l| l.contains("step=wait:conversation-held") && l.contains(&tab_b))
            && early.is_none()
            && rows_early == 0,
        "2a. a second agy opened by `-c` (no lock, the conversation's database held) keeps a crash from being relaunched onto that conversation",
        failed,
    );
    door.ctl(&[&at_c, "send", "/quit"])?;
    door.ctl(&[&at_c, "key", "enter"])?;
    let quit = crate::procs::wait_gone(opener, Duration::from_secs(20));
    let closed_c = door.ctl(&[&at_c, "close"])?;
    let t_quit = now_ms();
    let back = wait_for(150, || resuming(run, &conv_b, &seen))?;
    let continued = wait_for(120, || request_with(run, before, &[CONTINUED_CRASH, WORD3]))?;
    for row in restore::ledger_since(run, t) {
        println!("  typed     {row}");
    }
    println!(
        "  beside    `/quit` pid {opener} gone={quit} ({}); {}; continued: {}",
        closed_c.stdout.trim(),
        back.as_ref().map_or_else(
            || "NO agy resumed the conversation within 150 s".to_string(),
            |(p, _)| format!(
                "relaunched pid {p} in {}, {} after the second agy quit",
                cwd_of(*p).as_deref().unwrap_or("(unread)"),
                restore::since(t_quit, now_ms())
            )
        ),
        continued.map_or_else(|| "none".to_string(), |n| format!("req-{n:05}"))
    );
    said(
        quit && back.is_some() && continued.is_some(),
        "2b. once the second agy quit, the crash is relaunched on its conversation, the stub seeing its turn",
        failed,
    );
    if let Some((p, _)) = back {
        let _ = door.ctl(&[&at_b, "await", "idle", "2000", "timeout=60000"]);
        let _ = door.ctl(&[&at_b, "send", "/quit"]);
        let _ = door.ctl(&[&at_b, "key", "enter"]);
        let _ = crate::procs::wait_gone(p, Duration::from_secs(20));
    }
    let closed_b = door.ctl(&[&at_b, "close"])?;
    println!("  beside    tab {tab_b}: {}", closed_b.stdout.trim());
    Ok(())
}

/// Point the instance's twin at `build`.
fn put_twin(twin: &Path, build: &str) -> Result<(), Fail> {
    crate::sys::put(
        twin,
        0o755,
        &crate::sys::PutSrc::Bytes(twin_script(build).as_bytes()),
    )
    .map_err(|e| could_not(&format!("the twin: {e}")))
}

/// A stand-in build `<run>/<name>/agy` that runs the lab's build with `flag`.
fn put_build(run: &Run, name: &str, flag: &str) -> Result<String, Fail> {
    let path = run.run.join(name).join("agy");
    crate::sys::put(
        &path,
        0o755,
        &crate::sys::PutSrc::Bytes(build_with(&run.agy_bin, flag).as_bytes()),
    )
    .map_err(|e| could_not(&format!("the {name} build: {e}")))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Launch agy by the seeded verb (`crate::gemini::launch`), the lab
/// reloaded, and wait for ITS screen: a new agy process under the instance,
/// then its logo (the launcher's `clear` wipes the last agy's frame, whose
/// rows a composer pattern alone would match at once) and its composer, the
/// screen settled.
fn launch_agy(tag: &str, args: &[String]) -> Result<Run, Fail> {
    let run = Run::load(tag)?;
    let before: Vec<i32> = agys(&run).into_iter().map(|(p, _)| p).collect();
    crate::gemini::launch(
        &run,
        Agent::Agy,
        &crate::gemini::Launch {
            no_wait: true,
            args: args.to_vec(),
            ..crate::gemini::Launch::default()
        },
    )?;
    let run = Run::load(tag)?;
    if wait_for(60, || {
        agys(&run).into_iter().find(|(p, _)| !before.contains(p))
    })?
    .is_none()
    {
        return Err(could_not("no new agy started"));
    }
    let door = Door::new(&run)?;
    if !door.seen("Antigravity CLI", 60)? || !door.seen("^>", 60)? {
        return Err(could_not("agy never drew its composer"));
    }
    door.await_idle(1500, 30)?;
    Ok(run)
}

/// A turn typed into agy's composer, or why the check cannot run.
fn turn(door: &Door<'_>, text: &str) -> Result<(), Fail> {
    if crate::verbs::turn(door, text)? {
        Ok(())
    } else {
        Err(could_not(&format!("the turn `{text}` was not sent")))
    }
}

/// The agy that holds a conversation lock, other than `not`, within 60 s.
fn holder(run: &Run, not: &[i32]) -> Result<Option<(i32, String)>, Fail> {
    wait_for(60, || {
        agys(run).into_iter().find_map(|(pid, _)| {
            (!not.contains(&pid))
                .then(|| conversation_of(run, pid).map(|c| (pid, c)))
                .flatten()
        })
    })
}

/// One `agy-relaunch` run on the lab `tag`, which `cmd_agy_relaunch` brought
/// up.
#[allow(clippy::too_many_lines)]
fn run_check(tag: &str, armed: bool) -> Result<Verdict, Fail> {
    let mut run = Run::load(tag)?;
    let aterm = restore::seam_checked(&run)?;
    crate::gemini::need(&run, Agent::Agy).map_err(|f| could_not(&f.msg.unwrap_or_default()))?;
    println!(
        "agy-relaunch: lab '{tag}', agy {} ({}), {aterm}",
        run.agy_ver, run.agy_bin
    );
    let mut failed = Vec::new();
    let mut unseen = Vec::new();
    let pkg = run.w.join("home/Library/Application Support/aterm/pkg");
    let twin = pkg.join("agents/agy");
    put_twin(&twin, &run.agy_bin)?;
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
    println!("  twin      {} -> {}", twin.display(), run.agy_bin);

    run = launch_agy(tag, &[])?;
    let door = Door::new(&run)?;
    turn(&door, &format!("remember the word {WORD}"))?;
    if !door.seen(r"\(reply 1\)", 60)? {
        return Err(could_not("the stub's first answer never showed"));
    }
    let Some((pid1, conv)) = holder(&run, &[])? else {
        return Err(could_not(
            "no agy holds a presence lock under the scratch agy-home",
        ));
    };
    let workspace = cwd_of(pid1);
    println!(
        "  launched  agy pid {pid1}, conversation {conv}, directory {}",
        workspace.as_deref().unwrap_or("(unread)")
    );
    let t_j = now_ms();
    let journal = wait_for(30, || restore::desk_naming(&run, &conv))?;
    println!(
        "  journal   {}",
        journal.map_or_else(
            || "NO crash journal named the conversation within 30 s".to_string(),
            |_| format!(
                "names the conversation within {}",
                restore::since(t_j, now_ms())
            )
        )
    );
    door.await_idle(2000, 30)?;
    let before = newest_dump(&run);
    let t_crash = now_ms();
    let _ = crate::sys::kill(pid1, libc::SIGKILL);
    let back = wait_for(90, || resuming(&run, &conv, &[pid1]))?;
    let crashed_exit = exit_of(&door, "agy.sh");
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
            "1. the crash relaunched on its conversation within 90 s",
            &mut failed,
        );
        return Ok(Verdict::Failed);
    };
    let cwd2 = cwd_of(pid2);
    println!(
        "  relaunch  agy pid {pid2} after {} in {}: {} {}",
        restore::since(t_crash, now_ms()),
        cwd2.as_deref().unwrap_or("(unread)"),
        run.agy_bin,
        words.join(" ")
    );
    let continued = wait_for(120, || request_with(&run, before, &[CONTINUED_CRASH, WORD]))?;
    let typed = restore::ledger_since(&run, t_crash);
    for row in &typed {
        println!("  typed     {row}");
    }
    let line = typed.join("\n");
    let on_twin = line.contains("agents/agy'") && !line.contains("/store/agy/");
    let holds2 = conversation_of(&run, pid2);
    println!(
        "  continued {}; the relaunched agy holds {}",
        continued.map_or_else(
            || "NO request carried the continuation and the earlier turn within 120 s".to_string(),
            |n| format!("req-{n:05} carried the harness's continuation and `{WORD}`")
        ),
        holds2.as_deref().unwrap_or("no lock")
    );
    said(
        crashed_exit == Some(137)
            && on_twin
            && workspace.is_some()
            && cwd2 == workspace
            && holds2.as_deref() == Some(conv.as_str())
            && continued.is_some(),
        "1. a SIGKILLed agy is relaunched on its conversation, in its directory, on the twin; the stub sees the prior turns",
        &mut failed,
    );

    door.await_idle(2000, 60)?;
    let before = newest_dump(&run);
    let t2 = now_ms();
    let _ = crate::sys::kill(pid2, libc::SIGKILL);
    let back = wait_for(120, || resuming(&run, &conv, &[pid1, pid2]))?;
    let relaunch_line = format!("--conversation={conv}");
    let resumed_exit = exit_of(&door, &relaunch_line);
    println!(
        "  crash 2   SIGKILL the resumed agy pid {pid2}: its relaunch line's block exit {}",
        shown(resumed_exit)
    );
    let pid3 = back.as_ref().map(|(p, _)| *p);
    if let Some(p) = pid3 {
        println!(
            "  relaunch  agy pid {p} after {}",
            restore::since(t2, now_ms())
        );
    }
    let continued2 = wait_for(120, || request_with(&run, before, &[CONTINUED_CRASH, WORD]))?;
    said(
        resumed_exit == Some(137) && pid3.is_some() && continued2.is_some(),
        "2. a RESUMED agy's SIGKILL is read off its block (137) and its crash file, and relaunched again",
        &mut failed,
    );
    let Some(pid3) = pid3 else {
        return Ok(Verdict::Failed);
    };

    let Some(proj) = workspace.clone() else {
        return Ok(Verdict::Failed);
    };
    beside(&run, &proj, &[pid1, pid2, pid3], &mut failed)?;

    door.await_idle(2000, 60)?;
    stop::sleep(Duration::from_secs(2))?;
    let t3 = now_ms();
    door.ctl(&["key", "ctrl+z"])?;
    stop::sleep(Duration::from_secs(3))?;
    let stopped_exit = exit_of(&door, &relaunch_line);
    let state3 = crate::procs::state_of(pid3);
    let decided3 = wait_for(130, || {
        host_said(
            &run,
            t3,
            &["not relaunched: not-exited", "step=adopted", "step=ended:"],
        )
    })?;
    let again3 = resuming(&run, &conv, &[pid1, pid2, pid3]);
    let rows3 = relaunch_rows(&run, t3);
    let alive3 = crate::procs::alive(pid3);
    println!(
        "  ctrl-z    pid {pid3} state {}, alive={alive3}; its block's exit {}; the host: {}; an agy since: {}; relaunch rows since: {rows3}",
        state3.as_deref().unwrap_or("(unread)"),
        shown(stopped_exit),
        decided3.as_deref().map_or("NONE within 130 s", |l| l
            .split_once(": harness @")
            .map_or(l, |(_, s)| s)),
        again3
            .as_ref()
            .map_or_else(|| "none".to_string(), |(p, _)| p.to_string())
    );
    said(
        stopped_exit == Some(146)
            && alive3
            && state3.as_deref().is_some_and(|s| s.starts_with('T'))
            && decided3
                .as_deref()
                .is_some_and(|l| l.contains("not-exited"))
            && again3.is_none()
            && rows3 == 0,
        "3. Ctrl-Z (146, alive) is never relaunched: the host left it `not-exited`",
        &mut failed,
    );
    send_when_free(&door, "fg")?;
    stop::sleep(Duration::from_secs(3))?;

    door.await_idle(2000, 60)?;
    stop::sleep(Duration::from_secs(5))?;
    let t4 = now_ms();
    let typed_quit = send_when_free(&door, "/quit")?;
    let gone = crate::procs::wait_gone(pid3, Duration::from_secs(20));
    stop::sleep(Duration::from_secs(2))?;
    let clean_exit = exit_of(&door, "fg");
    let decided4 = wait_for(130, || {
        host_said(
            &run,
            t4,
            &[
                "step=ended:graceful-exit",
                "step=adopted",
                "not relaunched",
                "nothing-recorded",
            ],
        )
    })?;
    let t_decided4 = now_ms();
    let again4 = resuming(&run, &conv, &[pid1, pid2, pid3]);
    let rows4 = relaunch_rows(&run, t4);
    println!(
        "  quit      `/quit` typed={typed_quit}, pid {pid3} gone={gone}, its `fg` block's exit {}; the host's decision after {}: {}; an agy since: {}; relaunch rows since: {rows4}",
        shown(clean_exit),
        restore::since(t4, t_decided4),
        decided4.as_deref().map_or("NONE within 130 s", |l| l
            .split_once(": harness @")
            .map_or(l, |(_, s)| s)),
        again4
            .as_ref()
            .map_or_else(|| "none".to_string(), |(p, _)| p.to_string())
    );
    let what4 = "4. a clean /quit (0) is never relaunched: the host decided `ended:graceful-exit`";
    let left4 = typed_quit && gone && clean_exit == Some(0) && again4.is_none() && rows4 == 0;
    match decided4.as_deref() {
        Some(l) if left4 && l.contains("graceful-exit") => said(true, what4, &mut failed),
        Some(l) if left4 && l.contains("nothing-recorded") => unobserved(
            what4,
            "the host read nothing of the agy `fg` brought back before it ended",
            &mut unseen,
        ),
        _ => said(false, what4, &mut failed),
    }

    let known5 = [pid1, pid2, pid3];
    run = launch_agy(tag, &[])?;
    let door = Door::new(&run)?;
    let before5 = newest_dump(&run);
    turn(&door, &format!("remember the word {WORD2}"))?;
    let answered = wait_for(60, || request_with(&run, before5, &[WORD2]))?;
    door.await_idle(2000, 60)?;
    let Some((pid6, conv2)) = holder(&run, &known5)? else {
        return Err(could_not("the fresh agy holds no presence lock"));
    };
    let named = wait_for(30, || restore::desk_naming(&run, &conv2))?;
    let refuser = put_build(&run, "refuser", REFUSED_FLAG)?;
    put_twin(&twin, &refuser)?;
    println!(
        "  fresh     agy pid {pid6}, conversation {conv2}: its turn reached the stub={}, the journal names it={}, the twin now execs {refuser}",
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
    let line5 = format!("--conversation={conv2}");
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
        decided_word
            .as_deref()
            .is_some_and(|w| w.starts_with("refused:resume-refused"))
            && refused_line == Some(2)
            && rows5 == 1
            && rows5_later == 1,
        "5. a relaunch agy refused (2) is typed once and never again (`refused:resume-refused`)",
        &mut failed,
    );
    put_twin(&twin, &run.agy_bin)?;

    let Some(proj) = workspace.clone() else {
        return Ok(Verdict::Failed);
    };
    crate::gemini::seed(&run, Agent::Agy, &crate::gemini::Launch::default())?;
    let script = run.run.join("agy-resume.sh");
    crate::sys::put(
        &script,
        0o755,
        &crate::sys::PutSrc::Bytes(
            crate::gemini::launcher(&run, Agent::Agy, &[], &[format!("--conversation={conv}")])
                .as_bytes(),
        ),
    )
    .map_err(|e| could_not(&format!("the resume launcher: {e}")))?;
    let q = |p: &str| format!("'{}'", p.replace('\'', "'\\''"));
    let resume_by_hand = format!(
        "( cd {} && exec sh {} )",
        q(&proj),
        q(&script.to_string_lossy())
    );
    send_when_free(&door, &format!("mkdir -p {0}/sub && cd {0}/sub", q(&proj)))?;
    stop::sleep(Duration::from_secs(1))?;
    send_when_free(&door, &resume_by_hand)?;
    let mut known = vec![pid1, pid2, pid3, pid6];
    let Some((pid4, _)) = wait_for(30, || resuming(&run, &conv, &known))? else {
        said(
            false,
            "6. the conversation resumed by hand from a subdirectory",
            &mut failed,
        );
        return Ok(Verdict::Failed);
    };
    known.push(pid4);
    door.seen("^>", 60)?;
    door.await_idle(2000, 30)?;
    let pid4_row = format!("pid = {pid4}");
    let journal6 = wait_for(30, || {
        restore::desk_naming(&run, &conv)
            .filter(|p| crate::util::scratch_text(p).contains(&pid4_row))
    })?;
    println!(
        "  cd        the journal names the agy resumed by hand, pid {pid4}, and its conversation: {}",
        journal6.is_some()
    );
    let before = newest_dump(&run);
    let t6 = now_ms();
    let _ = crate::sys::kill(pid4, libc::SIGKILL);
    let back = wait_for(130, || resuming(&run, &conv, &known))?;
    let pid5 = back.as_ref().map(|(p, _)| *p);
    let cwd5 = pid5.and_then(cwd_of);
    let continued6 = wait_for(120, || request_with(&run, before, &[CONTINUED_CRASH, WORD]))?;
    let typed6 = restore::ledger_since(&run, t6);
    for row in &typed6 {
        println!("  typed     {row}");
    }
    let cd_typed = typed6
        .iter()
        .any(|r| r.contains(&format!("cd -- {} && ", q(&proj))));
    println!(
        "  cd        shell in {proj}/sub: the line cds first={cd_typed}; relaunched pid {} in {}",
        pid5.map_or_else(|| "none".to_string(), |p| p.to_string()),
        cwd5.as_deref().unwrap_or("(unread)")
    );
    said(
        cd_typed && cwd5.as_deref() == Some(proj.as_str()) && continued6.is_some(),
        "6. relaunched from a shell in another directory, the line cds to the launch's directory",
        &mut failed,
    );
    let Some(pid5) = pid5 else {
        return Ok(Verdict::Failed);
    };
    known.push(pid5);

    let pid7 = pid5;
    door.await_idle(2000, 60)?;
    let pid_row = format!("pid = {pid7}");
    let journal8 = wait_for(30, || {
        restore::desk_naming(&run, &conv)
            .filter(|p| crate::util::scratch_text(p).contains(&pid_row))
    })?;
    println!(
        "  restore   the journal names agy pid {pid7} and its conversation: {}",
        journal8.is_some()
    );
    let before = newest_dump(&run);
    let t_end = restore::end_aterm(&run, &door, "crash", &conv)?;
    let (t_launch, _) = restore::relaunch_instance(&mut run, "crash", armed, t_end)?;
    let back8 = wait_for(120, || resuming(&run, &conv, &known))?;
    for row in restore::ledger_since(&run, t_launch) {
        println!("  typed     {row}");
    }
    let continued8 = wait_for(120, || request_with(&run, before, &[CONTINUED_HOST, WORD]))?;
    println!(
        "  restored  {}; continued: {}",
        back8.as_ref().map_or_else(
            || "NO agy resumed the conversation".to_string(),
            |(p, w)| format!(
                "agy pid {p} after {}: {}",
                restore::since(t_end, now_ms()),
                w.join(" ")
            )
        ),
        continued8.map_or_else(|| "none".to_string(), |n| format!("req-{n:05}"))
    );
    said(
        journal8.is_some() && back8.is_some() && continued8.is_some(),
        "7. a window restored after aterm crashed brings the agy tab back on its conversation",
        &mut failed,
    );
    let Some((pid8, _)) = back8 else {
        return Ok(Verdict::Failed);
    };
    known.push(pid8);

    let door = Door::new(&run)?;
    door.await_idle(2000, 60)?;
    let pid_row = format!("pid = {pid8}");
    let journal9 = wait_for(30, || {
        restore::desk_naming(&run, &conv)
            .filter(|p| crate::util::scratch_text(p).contains(&pid_row))
    })?;
    let before9 = newest_dump(&run);
    let t_quit = restore::end_aterm(&run, &door, "quit", &conv)?;
    let (t_launch9, _) = restore::relaunch_instance(&mut run, "quit", armed, t_quit)?;
    let back9 = wait_for(120, || resuming(&run, &conv, &known))?;
    for row in restore::ledger_since(&run, t_launch9) {
        println!("  typed     {row}");
    }
    let continued9 = wait_for(120, || request_with(&run, before9, &[CONTINUED_HOST, WORD]))?;
    println!(
        "  restored  after the quit: the journal named pid {pid8}={}; {}; continued: {}",
        journal9.is_some(),
        back9.as_ref().map_or_else(
            || "NO agy resumed the conversation".to_string(),
            |(p, w)| format!(
                "agy pid {p} after {}: {}",
                restore::since(t_quit, now_ms()),
                w.join(" ")
            )
        ),
        continued9.map_or_else(|| "none".to_string(), |n| format!("req-{n:05}"))
    );
    said(
        journal9.is_some() && back9.is_some() && continued9.is_some(),
        "8. a window restored after aterm was quit brings the agy tab back on its conversation",
        &mut failed,
    );
    let Some((pid9, _)) = back9 else {
        return Ok(Verdict::Failed);
    };
    known.push(pid9);
    let door = Door::new(&run)?;
    door.await_idle(2000, 60)?;
    let fresh_config = config(&run, FRESH_DIR);
    let canonical_proj = std::fs::canonicalize(&run.proj)
        .map_or(run.proj.clone(), |p| p.to_string_lossy().into_owned());
    for (file, text) in [
        (
            "settings.json",
            crate::gemini::agy_settings(&[canonical_proj, run.proj.clone(), proj.clone()]),
        ),
        ("cache/onboarding.json", crate::gemini::agy_onboarded()),
    ] {
        crate::sys::put(
            &fresh_config.join(file),
            0o600,
            &crate::sys::PutSrc::Bytes(text.as_bytes()),
        )
        .map_err(|e| could_not(&format!("the fresh config's seed: {e}")))?;
    }
    let fresh = put_build(&run, "fresh", &format!("--app_data_dir={FRESH_DIR}"))?;
    put_twin(&twin, &fresh)?;
    let before9f = newest_dump(&run);
    let t9 = now_ms();
    let _ = crate::sys::kill(pid9, libc::SIGKILL);
    let decided9 = wait_for(130, || {
        host_said(
            &run,
            t9,
            &[
                "step=refused:",
                "step=adopted",
                "step=ended:",
                "step=failed:",
            ],
        )
    })?;
    stop::sleep(Duration::from_secs(5))?;
    let fresh_agy = agys(&run)
        .into_iter()
        .find(|(p, w)| !known.contains(p) && w.iter().any(|x| x.contains(FRESH_DIR)));
    let continued9f = request_with(&run, before9f, &[CONTINUED_CRASH]);
    let decided9_word = decided9
        .as_deref()
        .and_then(|l| l.split_once("step=").map(|(_, s)| s.to_string()));
    println!(
        "  fresh     SIGKILL pid {pid9}, the twin execing {fresh}: the host decided {}; a fresh agy: {}; a continuation request: {}",
        decided9_word.as_deref().unwrap_or("NOTHING within 130 s"),
        fresh_agy
            .as_ref()
            .map_or_else(|| "none".to_string(), |(p, _)| p.to_string()),
        continued9f.map_or_else(|| "none".to_string(), |n| format!("req-{n:05}"))
    );
    said(
        decided9_word
            .as_deref()
            .is_some_and(|w| w.starts_with("refused:started-fresh"))
            && fresh_agy.is_some()
            && continued9f.is_none(),
        "9. a relaunch that started a fresh conversation is told (`refused:started-fresh`), nothing typed into it",
        &mut failed,
    );
    put_twin(&twin, &run.agy_bin)?;
    if let Some((p, _)) = fresh_agy {
        known.push(p);
        let _ = send_when_free(&door, "/quit");
        let _ = crate::procs::wait_gone(p, Duration::from_secs(20));
    }

    if !failed.is_empty() {
        println!("agy-relaunch: FAIL: {}", failed.join("; "));
        Ok(Verdict::Failed)
    } else if unseen.is_empty() {
        println!("agy-relaunch: PASS");
        Ok(Verdict::Resumed)
    } else {
        println!("agy-relaunch: PASS but UNOBSERVED: {}", unseen.join("; "));
        Ok(Verdict::Resumed)
    }
}

/// `agy-relaunch [tag] [--keep]`.
pub fn cmd_agy_relaunch(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("agy-relaunch: {m}"));
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
    let tag = tag.unwrap_or_else(|| format!("agy-relaunch-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    if is_run(&tag) {
        return Err(usage(&format!(
            "lab '{tag}' is up: agy-relaunch brings up a lab of its own (another tag, or {LAB} down {tag})"
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
        eprintln!("aterm-lab: agy-relaunch: COULD NOT RUN: the lab did not come up");
        return Ok(3);
    }
    let verdict = run_check(&tag, !keep);
    if keep {
        println!("agy-relaunch: lab '{tag}' left up ({LAB} down {tag})");
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

    /// An agy's words name the conversation it resumes, any spelling.
    #[test]
    fn a_relaunched_agy_names_the_conversation_it_resumes() {
        assert_eq!(
            resumed(&words("--effort high --conversation=c1")),
            Some("c1")
        );
        assert_eq!(resumed(&words("-conversation c1")), Some("c1"));
        assert_eq!(resumed(&words("--effort high")), None);
        assert_eq!(resumed(&words("-c")), None);
        assert_eq!(resumed(&words("--conversations=c1")), None);
    }

    /// A presence lock names its conversation under its config. NEGATIVE
    /// CONTROLS: another directory, another config.
    #[test]
    fn a_presence_lock_names_its_conversation() {
        let config = Path::new("/s/agy-home/.gemini/antigravity-cli");
        let c = "0a9c7e21-3b5d-4f60-9e18-2c4d6f8a0b1c";
        let lock = format!("{}/presence/{c}.lock", config.display());
        assert_eq!(lock_conversation(config, &lock).as_deref(), Some(c));
        let db = format!("{}/conversations/{c}.db", config.display());
        assert_eq!(lock_conversation(config, &db), None);
        assert_eq!(
            lock_conversation(config, &format!("/elsewhere/presence/{c}.lock")),
            None
        );
    }

    /// The shell gets the launcher's environment but its `HOME`, which the
    /// relaunch line carries; the twin exports what atpkg's does and execs
    /// the build; a stand-in build runs the lab's with its flag first.
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
            "GEMINI_API_KEY=",
            "GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:5",
            "HTTPS_PROXY=http://127.0.0.1:6",
            "HOME=/s/runs/t/scratch/home ",
        ] {
            assert!(s.contains(want), "{want}: {s}");
        }
        assert!(!s.contains("agy-home"), "{s}");
        let t = twin_script("/a b/agy");
        assert!(
            t.starts_with("#!/bin/sh\n")
                && t.contains("export AGY_CLI_DISABLE_AUTO_UPDATE='true'\n")
                && t.contains("exec '/a b/agy' \"$@\"\n"),
            "{t}"
        );
        assert_eq!(
            build_with("/a/agy", REFUSED_FLAG),
            "#!/bin/sh\nexec '/a/agy' '--lab-refused-flag' \"$@\"\n"
        );
    }
}
