// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A real cold relaunch whose shell cannot read its launcher before the native deadline.

use std::time::{Duration, Instant};

use aterm_agent::harness::native_act::{self, Constructor, NativePlan, Request};

use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::pyjson::J;
use crate::relaunch_check::{exit_of, newest_dump, request_with, wait_for};
use crate::run::Run;
use crate::{Fail, Res, procs, restore, stop};

fn fail(message: impl std::fmt::Display) -> Fail {
    Fail::die(format!("cold-retirement: {message}"))
}

/// The birth token prevents a late cleanup from signalling a reused pid.
struct Process {
    pid: i32,
    birth: String,
    stopped: bool,
}

impl Process {
    fn owned(run: &Run, pid: i32) -> Result<Self, Fail> {
        let birth = procs::start_time(pid);
        if birth.is_empty() || !procs::descendants(run.instance_pid).contains(&pid) {
            return Err(fail(format!("pid {pid} is not a live child of this lab")));
        }
        Ok(Self {
            pid,
            birth,
            stopped: false,
        })
    }

    fn signal(&self, signal: i32) -> Result<(), Fail> {
        if procs::start_time(self.pid) != self.birth {
            return Err(fail(format!("pid {} changed identity", self.pid)));
        }
        crate::sys::kill(self.pid, signal).map_err(fail)
    }

    fn resume(&mut self) -> Result<(), Fail> {
        self.signal(libc::SIGCONT)?;
        self.stopped = false;
        Ok(())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.stopped && procs::start_time(self.pid) == self.birth {
            let _ = crate::sys::kill(self.pid, libc::SIGCONT);
        }
    }
}

fn record(run: &Run, conversation: &str) -> Option<J> {
    let path = run
        .state()
        .join("harness")
        .join(aterm_agent::harness::cli::HARNESS)
        .join("upgrade")
        .join(format!("{conversation}.json"));
    J::parse(&crate::util::read_regular_text(&path).ok()?).ok()
}

fn lapsed(record: &J) -> bool {
    record.get("phase").and_then(J::as_str) == Some("failed")
        && record.get("why").and_then(J::as_str) == Some("native-lapsed")
        && record.get("native_prepare").and_then(J::as_str) == Some("")
        && record.get("native_id").is_none()
        && matches!(
            record.get("native_retired_unissued"),
            None | Some(J::Bool(false))
        )
        && record.get("native_account_candidate").is_none()
}

fn agents(run: &Run) -> Vec<(i64, String)> {
    restore::agents_of(
        &run.claude_dir().join("sessions"),
        &procs::descendants(run.instance_pid),
    )
}

fn parent_shell(run: &Run, old: &Process) -> Result<Process, Fail> {
    let parent = aterm_sysprobe::kproc::record_read(u32::try_from(old.pid).map_err(fail)?)
        .map_err(fail)?
        .ok_or_else(|| fail("the original agent disappeared"))?
        .ppid;
    let shell = Process::owned(run, i32::try_from(parent).map_err(fail)?)?;
    let parent_record = aterm_sysprobe::kproc::record_read(parent)
        .map_err(fail)?
        .ok_or_else(|| fail("the parent shell disappeared"))?;
    if parent_record.pgid != shell.pid || parent_record.tty.is_none() {
        return Err(fail(
            "the agent's parent is not the interactive shell's process group leader",
        ));
    }
    Ok(shell)
}

fn freeze_after_reaping(old: &Process, shell: &mut Process, door: &Door<'_>) -> Result<(), Fail> {
    old.signal(libc::SIGKILL)?;

    // A stopped parent cannot reap its child or finish its shell-integration fence.
    // Wait for both the foreground and the completed command block before stopping it.
    let by = stop::deadline_after(Duration::from_secs(60));
    loop {
        let gone = aterm_sysprobe::kproc::record_read(u32::try_from(old.pid).map_err(fail)?)
            .map_err(fail)?
            .is_none();
        let foreground =
            aterm_sysprobe::kproc::record_read(u32::try_from(shell.pid).map_err(fail)?)
                .map_err(fail)?
                .is_some_and(|r| !r.zombie && r.pgid > 0 && r.tpgid == r.pgid);
        if gone && foreground && exit_of(door, "claude").is_some() {
            shell.signal(libc::SIGSTOP)?;
            shell.stopped = true;
            break;
        }
        if Instant::now() >= by {
            return Err(fail(
                "the shell did not reap the agent and regain foreground",
            ));
        }
        stop::sleep(Duration::from_millis(1))?;
    }
    Ok(())
}

fn expire(
    run: &Run,
    conversation: &str,
    shell: i32,
) -> Result<(Box<native_act::Prepare>, u64), Fail> {
    let prepared = wait_for(60, || {
        let value = record(run, conversation)?;
        let Request::Prepare(p) = Request::parse(value.get("native_prepare")?.as_str()?)? else {
            return None;
        };
        (value.get("phase").and_then(J::as_str) == Some("relaunched")
            && value.get("native_id").and_then(J::as_str) == Some(p.id.as_str())
            && p.constructor() == Constructor::ColdRestart
            && matches!(&p.plan, NativePlan::Restart { shell: s, .. } if i32::try_from(s.pid) == Ok(shell)))
        .then_some(p)
    })?
    .ok_or_else(|| fail("no admitted cold launcher was recorded while the shell was stopped"))?;
    let remaining = prepared
        .deadline_us
        .checked_sub(
            native_act::monotonic_us().ok_or_else(|| fail("the monotonic clock did not read"))?,
        )
        .ok_or_else(|| fail("the cold launcher was already expired when first observed"))?;
    if remaining > 180_000_000 {
        return Err(fail(
            "the cold launcher's deadline exceeds the lab's three-minute observation range",
        ));
    }
    println!(
        "cold-retirement: admitted {} · deadline in {:.1}s · keeping shell stopped",
        prepared.id,
        Duration::from_micros(remaining).as_secs_f64()
    );
    let by = stop::deadline_after(Duration::from_micros(remaining) + Duration::from_secs(60));
    loop {
        if let Some(value) = record(run, conversation) {
            if lapsed(&value) {
                break;
            }
            if let Some(Request::Prepare(next)) = value
                .get("native_prepare")
                .and_then(J::as_str)
                .and_then(Request::parse)
                && next.id != prepared.id
            {
                return Err(fail(
                    "the old operation changed before its durable lapse was observed",
                ));
            }
        }
        if Instant::now() >= by {
            return Err(fail(
                "the expired cold launcher never reached durable native-lapsed",
            ));
        }
        stop::sleep(Duration::from_millis(100))?;
    }
    let observed =
        native_act::monotonic_us().ok_or_else(|| fail("the monotonic clock did not read"))?;
    if observed < prepared.deadline_us {
        return Err(fail(
            "the lapse was not observed after the original deadline",
        ));
    }
    Ok((prepared, observed))
}

/// This lab explicitly runs the managed CLI in its restricted jail. Acknowledge only
/// that CLI's known bypass warning, never a generic confirmation or the host's prompts.
fn ready(door: &Door<'_>) -> Result<(), Fail> {
    const WARNING: &str = "WARNING: Claude Code running in Bypass Permissions mode";
    const SELECTED: &str = "❯.Yes,.I.accept";
    if !door.await_match("Bypass.Permissions.mode|shift.tab|for.agents|shortcuts", 60)? {
        return Err(fail(
            "Claude drew neither its known startup warning nor its footer",
        ));
    }
    let text = door.out(&["text"]);
    if text.contains(WARNING) && text.contains("Yes, I accept") && text.contains("No, exit") {
        let moved = door.ctl(&[
            "key",
            "if=WARNING:.Claude.Code.running.in.Bypass.Permissions.mode",
            "down",
        ])?;
        if moved.code != 0 || !door.await_match(SELECTED, 60)? {
            return Err(fail("the lab-only bypass acceptance was not selected"));
        }
        let accepted = door.ctl(&["key", "if=❯.Yes,.I.accept", "enter"])?;
        if accepted.code != 0 {
            return Err(fail("the lab-only bypass acceptance was not submitted"));
        }
        println!("cold-retirement: acknowledged the managed CLI's warning inside the private jail");
    }
    if !door.await_match("shift.tab|for.agents|shortcuts", 60)? {
        return Err(fail("Claude did not reach its composer after startup"));
    }
    Ok(())
}

fn exercise(run: &Run) -> Result<J, Fail> {
    restore::seam_checked(run)?;
    crate::lab::launch_claude(
        run,
        &Launch {
            by_name: true,
            ..Launch::default()
        },
    )?;
    let door = Door::new(run)?;
    ready(&door)?;
    if !crate::verbs::turn(&door, "hi")?
        || !door.seen("I am the local lab stub", 60)?
        || !door.await_idle(1000, 60)?
    {
        return Err(fail("the initial Claude conversation did not settle"));
    }
    let (old_pid, conversation) = wait_for(60, || restore::the_agent(run))?
        .ok_or_else(|| fail("the original agent did not register its conversation"))?;
    if !aterm_agent::harness::upgrade::is_session_id(&conversation) {
        return Err(fail(
            "the agent's conversation identifier is not a valid session name",
        ));
    }
    let old = Process::owned(run, i32::try_from(old_pid).map_err(fail)?)?;
    let mut shell = parent_shell(run, &old)?;
    let host_birth = procs::start_time(run.instance_pid);
    if host_birth.is_empty() {
        return Err(fail("the original host identity did not read"));
    }
    println!(
        "cold-retirement: {} · Claude {} · conversation {conversation} · agent {} · shell {}",
        run.aterm_build, run.claude_ver, old.pid, shell.pid
    );
    freeze_after_reaping(&old, &mut shell, &door)?;
    let (prepared, observed) = expire(run, &conversation, shell.pid)?;
    if procs::start_time(run.instance_pid) != host_birth {
        return Err(fail("the host was replaced during the stopped-shell check"));
    }
    println!(
        "cold-retirement: durable lapse observed · resuming shell {}",
        shell.pid
    );
    shell.resume()?;
    let (new_pid, _) = wait_for(60, || {
        let live = agents(run);
        (live.len() == 1
            && live[0].0 != old_pid
            && live[0].1 == conversation
            && record(run, &conversation)
                .as_ref()
                .is_some_and(|value| value.get("phase").and_then(J::as_str) == Some("done")))
        .then(|| live[0].clone())
    })?
    .ok_or_else(|| fail("exactly one agent did not resume the original conversation"))?;
    ready(&door)?;
    if !door.await_idle(1000, 60)? {
        return Err(fail("the resumed agent did not settle after its handoff"));
    }
    println!("cold-retirement: agent {new_pid} completed its handoff on the original conversation");
    let before = newest_dump(run);
    let marker = "cold-retirement-conversation-check";
    if !crate::verbs::turn(&door, marker)?
        || wait_for(60, || request_with(run, before, &[marker]))?.is_none()
        || agents(run) != [(new_pid, conversation.clone())]
        || procs::start_time(run.instance_pid) != host_birth
    {
        return Err(fail(
            "the resumed conversation did not handle its next turn alone",
        ));
    }
    println!(
        "cold-retirement: PASS · agent {old_pid} -> {new_pid} · same conversation · same host"
    );
    Ok(J::obj([
        ("aterm", J::s(&run.aterm_build)),
        ("claude", J::s(&run.claude_ver)),
        ("conversation", J::s(conversation)),
        ("old_pid", J::Int(old_pid)),
        ("new_pid", J::Int(new_pid)),
        ("shell_pid", J::Int(i64::from(shell.pid))),
        ("operation", J::s(prepared.id)),
        (
            "deadline_us",
            J::Int(i64::try_from(prepared.deadline_us).map_err(fail)?),
        ),
        ("lapsed_us", J::Int(i64::try_from(observed).map_err(fail)?)),
        ("passed", J::Bool(true)),
    ]))
}

/// Run the stopped-shell recovery check in a private lab and retain its evidence.
/// # Errors
/// Arguments are invalid, the lab cannot start or stop, or an observed recovery condition fails.
pub fn cmd(args: &[String]) -> Res {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: {} cold-retirement [tag]", crate::LAB);
        return Ok(0);
    }
    if args.len() > 1 || args.first().is_some_and(|a| a.starts_with('-')) {
        return Err(Fail::usage("cold-retirement accepts one optional lab tag"));
    }
    let tag = args
        .first()
        .cloned()
        .unwrap_or_else(|| format!("cold-retire-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) || crate::run::is_run(&tag) {
        return Err(Fail::usage("cold-retirement needs an unused lab tag"));
    }
    stop::install();
    let mut options = UpOpts::from_env();
    options.restore = true;
    options.trust = true;
    options.reroute = true;
    options.lifeline = true;
    options.clock = false;
    options.harness_keys = "continue = false".into();
    let run = match crate::lab::up(&tag, &options) {
        Ok(run) => run,
        Err(error) => {
            if crate::run::is_run(&tag) {
                let _ = crate::lab::down(&tag, true);
            }
            return Err(error);
        }
    };
    let result = exercise(&run);
    let logs = crate::lab::down(&tag, true)?;
    println!("cold-retirement: evidence {}", logs.display());
    let report = result?;
    crate::util::atomic_write(&logs.join("cold-retirement.json"), report.py().as_bytes())
        .map_err(fail)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_durable_lapse_with_no_retained_authority_is_success() {
        let good = r#"{"phase":"failed","why":"native-lapsed","native_prepare":""}"#;
        assert!(lapsed(&J::parse(good).unwrap()));
        for bad in [
            r#"{"phase":"failed","why":"native-lapsed","native_prepare":"prepare"}"#,
            r#"{"phase":"failed","why":"native-lapsed","native_prepare":"","native_id":"old"}"#,
            r#"{"phase":"failed","why":"native-lapsed","native_prepare":"","native_retired_unissued":true}"#,
            r#"{"phase":"failed","why":"native-lapsed","native_prepare":"","native_retired_unissued":null}"#,
            r#"{"phase":"failed","why":"native-lapsed","native_prepare":"","native_account_candidate":{}}"#,
            r#"{"phase":"failed","why":"something-else","native_prepare":""}"#,
            r#"{"phase":"relaunched","why":"native-lapsed","native_prepare":""}"#,
        ] {
            assert!(!lapsed(&J::parse(bad).unwrap()), "{bad}");
        }
    }
}
