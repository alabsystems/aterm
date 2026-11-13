// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the live relaunch checks share.

use std::path::PathBuf;
use std::time::Duration;

use crate::Fail;
use crate::door::Door;
use crate::pyjson::J;
use crate::restore;
use crate::run::Run;
use crate::stop;

/// Arguments after `bin` on a process command line `cmd` (`None` for another program).
#[must_use]
pub fn words_of(cmd: &str, bin: &str) -> Option<Vec<String>> {
    let rest = cmd.strip_prefix(bin)?;
    (rest.is_empty() || rest.starts_with(' '))
        .then(|| rest.split_whitespace().map(str::to_string).collect())
}

/// The processes under the lab's instance running `bin`, `(pid, words)`.
pub fn running(run: &Run, bin: &str) -> Vec<(i32, Vec<String>)> {
    crate::procs::descendants(run.instance_pid)
        .into_iter()
        .filter_map(|pid| words_of(&crate::procs::command_line(pid), bin).map(|w| (pid, w)))
        .collect()
}

/// Exit code of the newest finished command block in `blocks --json` matching `cmd`.
#[must_use]
pub fn block_exit(json: &str, cmd: &str) -> Option<i64> {
    let j = J::parse(json.trim()).ok()?;
    j.get("blocks")?
        .as_arr()?
        .iter()
        .rev()
        .find(|b| {
            b.get("state").and_then(J::as_str) == Some("complete")
                && b.get("cmdline")
                    .and_then(J::as_str)
                    .is_some_and(|c| c.contains(cmd))
        })?
        .get("exit")
        .and_then(J::as_i64)
}

/// The exit of the newest finished block of the lab's tab running `cmd`.
pub fn exit_of(door: &Door<'_>, cmd: &str) -> Option<i64> {
    let reply = door.out(&["blocks", "6", "--json"]);
    let json = reply.strip_prefix("OK ").unwrap_or(&reply);
    block_exit(json, cmd)
}

fn dumps(run: &Run) -> Vec<(u64, PathBuf)> {
    crate::stub::turn_dumps(&run.w.join("dump"))
}

/// The newest request dump past `since` whose body holds every one of `want`: its index.
pub fn request_with(run: &Run, since: u64, want: &[&str]) -> Option<u64> {
    dumps(run)
        .into_iter()
        .take_while(|(n, _)| *n > since)
        .find(|(_, p)| {
            let body = crate::util::scratch_text(p);
            want.iter().all(|w| body.contains(w))
        })
        .map(|(n, _)| n)
}

/// The newest request dump's index (`0`: none yet).
pub fn newest_dump(run: &Run) -> u64 {
    dumps(run).first().map_or(0, |(n, _)| *n)
}

/// An exit status as the report says it.
pub fn shown(exit: Option<i64>) -> String {
    exit.map_or_else(|| "unreadable".to_string(), |e| e.to_string())
}

/// A process's working directory, as the kernel names it, read from outside the jail.
pub fn cwd_of(pid: i32) -> Option<String> {
    let pid = u32::try_from(pid).ok()?;
    aterm_sysprobe::process_cwd(pid).map(|p| p.to_string_lossy().into_owned())
}

/// Every file a process holds open through a descriptor, read from outside the jail in this
/// process ([`aterm_uds::holders::open_files`]: none of its sockets or kqueues is read).
pub fn open_files(pid: i32) -> Vec<String> {
    u32::try_from(pid)
        .ok()
        .and_then(|pid| aterm_uds::holders::open_files(pid).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// The lines of an instance log (`<unix s.ms> <pid> <level> …`) stamped at or after `ms`.
#[must_use]
pub fn lines_since(log: &str, ms: i64) -> Vec<String> {
    log.lines()
        .filter(|l| {
            l.split_whitespace()
                .next()
                .and_then(|t| t.parse::<f64>().ok())
                .is_some_and(|secs| {
                    #[allow(clippy::cast_possible_truncation)]
                    let at = (secs * 1000.0).round() as i64;
                    at >= ms
                })
        })
        .map(str::to_string)
        .collect()
}

fn host_log_since(run: &Run, ms: i64) -> Vec<String> {
    lines_since(
        &crate::util::scratch_text(&run.state().join("logs/aterm.log")),
        ms,
    )
}

/// The first host log line since `ms` that holds any of `needles`.
pub fn host_said(run: &Run, ms: i64, needles: &[&str]) -> Option<String> {
    host_log_since(run, ms)
        .into_iter()
        .find(|l| needles.iter().any(|n| l.contains(n)))
}

/// The upgrade ledger's `relaunched` rows written since `ms`.
pub fn relaunch_rows(run: &Run, ms: i64) -> usize {
    restore::ledger_since(run, ms)
        .into_iter()
        .filter(|r| r.starts_with("relaunched"))
        .count()
}

/// Wait up to `secs` for `f` to give something, every 250 ms.
/// # Errors
/// The run was stopped.
pub fn wait_for<T>(secs: u64, mut f: impl FnMut() -> Option<T>) -> Result<Option<T>, Fail> {
    for _ in 0..secs * 4 {
        if let Some(t) = f() {
            return Ok(Some(t));
        }
        stop::sleep(Duration::from_millis(250))?;
    }
    Ok(f())
}

/// Types `text` and Enter once the tab accepts driver input (up to 30 s).
/// # Errors
/// The run was stopped, or the door is gone.
pub fn send_when_free(door: &Door<'_>, text: &str) -> Result<bool, Fail> {
    for _ in 0..60 {
        let r = door.ctl(&["send", text])?;
        if r.code == 0 && !r.stdout.starts_with("ERR") {
            stop::sleep(Duration::from_millis(700))?;
            return Ok(door.ctl(&["key", "enter"])?.code == 0);
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    Ok(false)
}

/// One phase's verdict, said.
pub fn said(ok: bool, what: &str, failed: &mut Vec<String>) {
    println!("  {} {what}", if ok { "ok  " } else { "FAIL" });
    if !ok {
        failed.push(what.to_string());
    }
}

/// A phase whose fact nothing could observe: said as that, never as ok.
pub fn unobserved(what: &str, why: &str, list: &mut Vec<String>) {
    println!("  ---- {what}: UNOBSERVED ({why})");
    list.push(what.to_string());
}

#[cfg(test)]
#[path = "relaunch_check_tests.rs"]
mod tests;
