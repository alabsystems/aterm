// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `restart-jobs` ([`cmd_restart_jobs`]): whether a restart of Claude Code ends the background
//! shells it runs. The upgrade's restart signals Claude Code `SIGTERM` over the tab's control
//! socket (`upgrade_drive`'s `@<tab> signal term pid=…`), and a press of the ⬆️ restarts past
//! that work (`upgrade::press_gate`), so the press's words must say what the restart does to it
//! (`upgrade::RESTART_ENDS_BACKGROUND`, `docs/measured/claude-code-restart-jobs-2026-10-27.md`).
//! A lab's real Claude Code, on the stub's `bg` mode, starts `sleep` in the background
//! (`bgstart`); once the shell runs under it, the lab's socket sends the restart's signal, and the
//! drive reads whether the shell outlives Claude Code. Exit 1 where the run disagrees with
//! `RESTART_ENDS_BACKGROUND`.

use std::time::Duration;

use crate::lab::Launch;
use crate::scn::{Ctx, LabKw, ScnErr};
use crate::{Fail, Res, stop};

const USAGE: &str = "usage: targo --unverified lab restart-jobs [TAG]

A lab's real Claude Code (LAB_CLAUDE picks the build) starts `sleep 600` in the background on the
stub's bg mode; once that shell runs under it, the lab's own control socket sends the upgrade
restart's signal (`@<tab> signal term pid=<claude>`). It reads whether the shell outlives Claude
Code and compares that with upgrade::RESTART_ENDS_BACKGROUND. Exit 0 AGREES, 1 DISAGREES,
3 COULD NOT RUN. The lab comes down at the end; its logs are kept.
";

/// How long the background `sleep` runs: far past every wait here, so a shell that outlives the
/// restart is read alive.
const SLEEP_S: u32 = 600;

/// A hang detector for what must happen (the shell starting, Claude Code ending on its signal,
/// a shell the restart ends going): a minute.
const HANG: Duration = Duration::from_secs(60);

/// What one trial read.
#[derive(Debug, PartialEq, Eq)]
struct Trial {
    claude: i32,
    shell: i32,
    /// Claude Code's mode row named its shell (`1 shell`) before the signal.
    counted: bool,
    /// The shell was gone within [`HANG`] of Claude Code's end.
    ended: bool,
}

/// The verdict on `ended` against what aterm assumes (`assumed`,
/// `upgrade::RESTART_ENDS_BACKGROUND`), and its exit code.
const fn verdict(ended: bool, assumed: bool) -> (&'static str, i32) {
    if ended == assumed {
        ("AGREES", 0)
    } else {
        ("DISAGREES", 1)
    }
}

fn could_not(why: impl Into<String>) -> ScnErr {
    ScnErr::Failed(why.into())
}

/// Claude Code's top process in the lab: one running its binary whose parent runs no other.
fn claude_pid(ctx: &Ctx) -> Option<i32> {
    let bin = ctx.run.claude_bin.clone();
    let table = crate::procs::table();
    let runs = |p: i32| crate::procs::command_line(p).contains(&bin);
    crate::procs::lab_tree(&ctx.run)
        .into_iter()
        .filter(|&p| runs(p))
        .find(|&p| {
            table
                .iter()
                .find(|(pid, _)| *pid == p)
                .is_none_or(|&(_, parent)| !runs(parent))
        })
}

/// The background `sleep` under `claude`, once it runs.
fn shell_under(claude: i32) -> Option<i32> {
    let want = format!("sleep {SLEEP_S}");
    crate::procs::descendants(claude)
        .into_iter()
        .find(|&p| crate::procs::command_line(p).trim() == want)
}

/// Whether `pid` is gone or a zombie its parent has not reaped yet.
fn gone(pid: i32) -> bool {
    !crate::procs::alive(pid)
        || crate::procs::state_of(pid).is_some_and(|state| state.starts_with('Z'))
}

fn trial(ctx: &mut Ctx) -> Result<Trial, ScnErr> {
    ctx.start()?;
    ctx.setup_bg(SLEEP_S, 1);
    ctx.launch(120.0, Launch::default())?;
    let claude = claude_pid(ctx).ok_or_else(|| could_not("no Claude Code process in the lab"))?;
    ctx.turn("bgstart")?;
    let mut shell = None;
    if !crate::wait_until(HANG, || {
        shell = shell_under(claude);
        shell.is_some()
    }) {
        stop::check()?;
        return Err(could_not(format!(
            "no `sleep {SLEEP_S}` under Claude Code (pid {claude}) within a minute of bgstart"
        )));
    }
    let shell = shell.unwrap_or_default();
    let mut counted = false;
    let _ = crate::wait_until(HANG, || {
        counted = ctx
            .ctl(&["text"])
            .stdout
            .lines()
            .any(|row| row.contains("1 shell"));
        counted
    });
    ctx.mark(&format!("shell {shell} under claude {claude}"), None);
    let sid = ctx.sid.clone();
    let reply = ctx.ctl(&[
        &format!("@{sid}"),
        "signal",
        "term",
        &format!("pid={claude}"),
    ]);
    ctx.mark(
        &format!(
            "signal term: {}",
            format!("{}{}", reply.stdout, reply.stderr).trim()
        ),
        None,
    );
    if !crate::wait_until(HANG, || gone(claude)) {
        stop::check()?;
        return Err(could_not(format!(
            "Claude Code (pid {claude}) did not end within a minute of its SIGTERM"
        )));
    }
    let ended = crate::wait_until(HANG, || gone(shell));
    stop::check()?;
    Ok(Trial {
        claude,
        shell,
        counted,
        ended,
    })
}

/// `restart-jobs [TAG]`.
pub fn cmd_restart_jobs(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("restart-jobs: {m}"));
    let mut tag = None::<String>;
    for a in args {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(0);
            }
            s if s.starts_with('-') => return Err(usage(&format!("unknown option '{s}'"))),
            s => tag = Some(s.to_string()),
        }
    }
    let tag = tag.unwrap_or_else(|| format!("rjobs-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    if crate::run::is_run(&tag) {
        return Err(usage(&format!(
            "lab '{tag}' is up: this verb brings its own lab up"
        )));
    }
    stop::install();
    let mut ctx = Ctx::new(
        "restart-jobs",
        1,
        &tag,
        "continue = false",
        LabKw::default(),
        false,
        true,
    );
    let outcome = trial(&mut ctx);
    let assumed = aterm_agent::harness::upgrade::RESTART_ENDS_BACKGROUND;
    let rc = match &outcome {
        Ok(t) => {
            let (word, rc) = verdict(t.ended, assumed);
            println!(
                "restart-jobs: Claude Code {} pid {} with `sleep {SLEEP_S}` pid {} in the \
                 background (its mode row {}): after its SIGTERM the shell {}; \
                 RESTART_ENDS_BACKGROUND = {assumed}: {word}",
                ctx.run.claude_ver,
                t.claude,
                t.shell,
                if t.counted {
                    "said `1 shell`"
                } else {
                    "named no shell"
                },
                if t.ended {
                    "was gone within a minute"
                } else {
                    "lived on past a minute"
                },
            );
            ctx.put("ended", t.ended);
            ctx.put("counted", t.counted);
            ctx.put("verdict", word);
            Ok(rc)
        }
        Err(ScnErr::Failed(why)) => {
            println!("restart-jobs: COULD NOT RUN: {why}");
            Ok(3)
        }
        Err(ScnErr::Stopped(s)) => Err(stop::Stopped(s.0).into()),
    };
    ctx.save_timeline();
    // Brings the lab down, its logs kept.
    ctx.finish();
    ctx.save_summary();
    if let Some(d) = &ctx.dir {
        println!("results: {}", d.display());
    }
    rc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verdict agrees only where the run read what aterm assumes. NEGATIVE CONTROL: the
    /// other reading disagrees, either way round.
    #[test]
    fn the_verdict_holds_the_run_to_what_aterm_assumes() {
        assert_eq!(verdict(true, true), ("AGREES", 0));
        assert_eq!(verdict(false, false), ("AGREES", 0));
        assert_eq!(verdict(false, true), ("DISAGREES", 1), "CONTROL");
        assert_eq!(verdict(true, false), ("DISAGREES", 1), "CONTROL");
    }
}
