// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Same-image LaunchServices handoff in a disposable VM, without a host window lease.

use std::time::{Duration, Instant};

use aterm_uds::process::Identity;

use super::{Fail, GUEST_HELPER, GUEST_NUX, Guest, Row, Value, first_session, status_field};
use crate::{instance::CONFIG_OFF, stop, util::shell_quote};

const WAIT: Duration = Duration::from_secs(60);
const REQUIRED_QUERY_HANG: Duration = Duration::from_secs(60);
pub(super) const SOCKET: &str = "/var/tmp/aterm-nux/debug-successor.sock";
const LAUNCHED_ROUTE: &str = "this successor owns a launchd application job of its own";

fn ctl(g: &Guest, words: &str) -> Result<String, Fail> {
    g.run(
        &format!("{} ctl --sock {SOCKET} {words} 2>&1", g.aterm),
        WAIT,
    )
}

fn owner(g: &Guest) -> Option<Identity> {
    let read = g
        .run(
            &format!("{GUEST_NUX}/{GUEST_HELPER} socket-owner {SOCKET} 2>/dev/null"),
            Duration::from_secs(5),
        )
        .ok()?;
    Identity::parse(read.trim())
}

fn wait_owner(g: &Guest, before: Option<Identity>) -> Result<Option<Identity>, Fail> {
    let end = Instant::now() + WAIT;
    loop {
        if let Some(now) = owner(g)
            && before.is_none_or(|old| old != now)
        {
            return Ok(Some(now));
        }
        if Instant::now() >= end {
            return Ok(None);
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

fn required_query<T>(
    started: Instant,
    end: Instant,
    run: impl FnOnce(Duration) -> Result<T, Fail>,
) -> Result<T, Fail> {
    let left = end
        .min(started + REQUIRED_QUERY_HANG)
        .saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Fail::die("guest process query exceeded its hang detector"));
    }
    run(left)
}

fn wait_gone(g: &Guest, pid: i32) -> Result<bool, Fail> {
    let end = Instant::now() + WAIT;
    loop {
        let alive = required_query(Instant::now(), end, |left| {
            g.run(
                &format!("kill -0 {pid} >/dev/null 2>&1 && echo alive || echo gone"),
                left,
            )
        })?;
        if alive.trim() == "gone" {
            return Ok(true);
        }
        if Instant::now() >= end {
            return Ok(false);
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

fn shell_mark(g: &Guest, sid: &str, word: &str, seed: Option<&str>) -> Result<(), Fail> {
    let seed = seed.map_or(String::new(), |seed| {
        format!("VM_HANDOFF_STAMP={}; ", shell_quote(seed))
    });
    let line = format!(
        "{seed}printf '%s:%s\\n' \"$$\" \"$VM_HANDOFF_STAMP\" > {GUEST_NUX}/{word}.txt; echo {word}\n"
    );
    let sent = g.run(
        &format!(
            "printf %s {} | {} ctl --sock {SOCKET} @{sid} send --stdin",
            shell_quote(&line),
            g.aterm
        ),
        WAIT,
    )?;
    if !sent.starts_with("OK ") {
        return Err(Fail::die(format!("terminal input: {sent}")));
    }
    let seen = ctl(g, &format!("@{sid} await match '^{word}$' timeout=60000"))?;
    if !seen.starts_with("OK ") {
        return Err(Fail::die(format!("terminal output: {seen}")));
    }
    Ok(())
}

fn continuity(
    before: Identity,
    after: Identity,
    shell_before: &str,
    shell_after: &str,
    stamp: &str,
    log: &str,
) -> Result<(), &'static str> {
    if before.pid == after.pid {
        return Err("the GUI did not move to a different process");
    }
    let seeded = shell_before
        .trim()
        .split_once(':')
        .is_some_and(|(pid, value)| pid.parse::<u32>().is_ok_and(|pid| pid > 1) && value == stamp);
    if !seeded || shell_before != shell_after {
        return Err("the shell process or its unexported variable changed");
    }
    if !log.contains(LAUNCHED_ROUTE) {
        return Err("no out-of-band launchd successor was observed");
    }
    Ok(())
}

fn same_image(before: &str, after: &str) -> bool {
    ["version", "build", "commit"].iter().all(|field| {
        let before = status_field(before, field);
        before.is_some_and(|value| !value.is_empty()) && before == status_field(after, field)
    })
}

pub(super) fn run(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    // This disposable guest deliberately asks native restoration to do the opposite of the
    // application's process-local policy. The app must start without rewriting these settings.
    g.sh(
        "/usr/bin/defaults write com.aterm.aterm NSQuitAlwaysKeepsWindows -bool true; \
         /usr/bin/defaults write com.aterm.aterm ApplePersistenceIgnoreState -bool false",
        WAIT,
    )?;
    let native_preferences = "/usr/bin/defaults read com.aterm.aterm NSQuitAlwaysKeepsWindows && \
                              /usr/bin/defaults read com.aterm.aterm ApplePersistenceIgnoreState";
    let preferences_before = g.run(native_preferences, WAIT)?;
    if preferences_before.trim() != "1\n0" {
        row.fail("the guest did not seed the native restoration preferences".into());
        return Ok(());
    }
    let config = format!("{GUEST_NUX}/config");
    g.sh(&format!(
        "chmod 700 {GUEST_NUX}; mkdir -p {config}/aterm; printf %s {} > {config}/aterm/aterm.toml; chmod 600 {config}/aterm/aterm.toml; \
         open --stderr {GUEST_NUX}/debug-successor.out --env ATERM_DEBUG_SEAMLESS_REEXEC=1 --env XDG_CONFIG_HOME={config} {} \
         --args --no-reroute --control-sock {SOCKET} -e /bin/sh -i",
        shell_quote(CONFIG_OFF), shell_quote(&g.app)
    ), WAIT)?;
    let Some(before) = wait_owner(g, None)? else {
        row.fail("the test-profile app did not serve its control socket".into());
        return Ok(());
    };
    let version = ctl(g, "version")?;
    row.cell("test image", Value::Text(version.trim().to_owned()));
    if status_field(&version, "profile") != Some("debug") {
        row.fail("the image did not report the debug profile required by the QA seam".into());
        return Ok(());
    }
    let sessions = ctl(g, "ls")?;
    let sid = first_session(&sessions).ok_or_else(|| Fail::die("no terminal session"))?;
    let stamp = format!("handoff-{}", crate::util::now_ms());
    shell_mark(g, sid, "VM_BEFORE", Some(&stamp))?;
    let shell_before = g.read(&format!("{GUEST_NUX}/VM_BEFORE.txt"))?;
    let status = ctl(g, "update status")?;
    row.cell("before apply", Value::Text(status.trim().to_owned()));
    if status_field(&status, "enabled") != Some("false") {
        row.fail("the diagnostic did not disable the real update channel".into());
        return Ok(());
    }
    let applied = ctl(g, "update apply")?;
    row.cell("apply", Value::Text(applied.trim().to_owned()));
    if !applied.starts_with("OK ") {
        row.fail("the same-image apply was refused".into());
        return Ok(());
    }
    let Some(after) = wait_owner(g, Some(before))? else {
        row.fail("no new control-socket owner before the one-minute hang detector".into());
        row.cell(
            "after apply",
            Value::Text(ctl(g, "update status")?.trim().to_owned()),
        );
        return Ok(());
    };
    let after_version = ctl(g, "version")?;
    row.cell(
        "successor image",
        Value::Text(after_version.trim().to_owned()),
    );
    if !same_image(&version, &after_version) {
        row.fail("the QA successor did not run the same build".into());
    }
    shell_mark(g, sid, "VM_AFTER", None)?;
    let shell_after = g.read(&format!("{GUEST_NUX}/VM_AFTER.txt"))?;
    let log = g.log()?;
    row.cell("GUI before", Value::Text(before.token()));
    row.cell("GUI after", Value::Text(after.token()));
    row.cell("shell before", Value::Text(shell_before.trim().to_owned()));
    row.cell("shell after", Value::Text(shell_after.trim().to_owned()));
    if let Err(why) = continuity(before, after, &shell_before, &shell_after, &stamp, &log) {
        row.fail(why.into());
    } else {
        row.cell("out-of-band launch verified", Value::Text("true".into()));
    }
    if wait_gone(g, before.pid)? {
        row.cell("outgoing process gone", Value::Text("true".into()));
    } else {
        row.fail("the outgoing GUI did not exit before the one-minute hang detector".into());
    }
    let metrics = g.run(
        &format!(
            "{GUEST_NUX}/{GUEST_HELPER} socket-metrics {SOCKET} > {GUEST_NUX}/successor-metrics.txt 2> {GUEST_NUX}/successor-metrics-error.txt; cat {GUEST_NUX}/successor-metrics.txt",
        ),
        WAIT,
    )?;
    let metrics: aterm_json::Value = aterm_json::from_str(&metrics)
        .map_err(|error| Fail::die(format!("successor metrics unavailable: {error}")))?;
    if metrics["peer"].as_str() != Some(after.token().as_str()) {
        row.fail("the metrics responder was not the observed successor".into());
    } else {
        row.cell("startup metrics peer", Value::Text(after.token()));
    }
    let quit = ctl(g, "invoke Quit")?;
    row.cell("quit", Value::Text(quit.trim().to_owned()));
    if !quit.starts_with("OK ") {
        row.fail("the successor did not accept normal Quit".into());
    } else {
        if quit.starts_with("OK confirm pending") {
            let confirmed = ctl(g, "confirm yes")?;
            row.cell("quit confirmed", Value::Text(confirmed.trim().to_owned()));
            if !confirmed.starts_with("OK ") {
                row.fail("the successor did not accept its Quit confirmation".into());
            }
        }
        if !wait_gone(g, after.pid)? {
            row.fail("the successor did not exit after normal Quit".into());
        } else {
            row.cell("successor closed", Value::Text("true".into()));
        }
    }
    let preferences_after = g.run(native_preferences, WAIT)?;
    if preferences_before == preferences_after {
        row.cell(
            "native preferences preserved",
            Value::Text(preferences_after.trim().replace('\n', "/")),
        );
    } else {
        row.fail("the application rewrote persistent native restoration preferences".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_query_completes_native_command_after_the_old_query_limit() {
        const ACTOR: &str = "ATERM_NUX_REQUIRED_QUERY_ACTOR";
        if std::env::var_os(ACTOR).is_some() {
            use std::io::Write as _;
            println!("required-query-executed");
            std::io::stdout().flush().unwrap();
            return;
        }
        let started = Instant::now() - Duration::from_secs(6);
        let out = required_query(started, started + WAIT, |left| {
            let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
            command
                .args([
                    "mac_nux::debug_successor::tests::required_query_completes_native_command_after_the_old_query_limit",
                    "--exact",
                    "--nocapture",
                ])
                .env(ACTOR, "1");
            aterm_uds::deadline::output_within(&mut command, left)
                .map_err(|error| Fail::die(format!("{error:?}")))
        })
        .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(
            String::from_utf8(out.stdout)
                .unwrap()
                .lines()
                .any(|line| line == "required-query-executed")
        );
    }

    #[test]
    fn expired_required_query_does_not_start_another_command() {
        let started = Instant::now() - Duration::from_secs(61);
        assert!(
            required_query(started, started + WAIT, |_| -> Result<(), Fail> {
                panic!("expired query ran");
            })
            .is_err()
        );
        assert!(
            required_query(Instant::now(), started + WAIT, |_| -> Result<(), Fail> {
                panic!("query restarted the enclosing wait");
            })
            .is_err()
        );
    }

    #[test]
    fn only_a_new_launched_gui_with_the_same_live_shell_qualifies() {
        let before = Identity::parse_shaped("10@100:1", true).unwrap();
        let after = Identity::parse_shaped("11@101:1", true).unwrap();
        let shell = "20:unexported-state\n";
        let stamp = "unexported-state";
        assert!(continuity(before, after, shell, shell, stamp, LAUNCHED_ROUTE).is_ok());
        assert!(continuity(before, before, shell, shell, stamp, LAUNCHED_ROUTE).is_err());
        assert!(
            continuity(
                before,
                after,
                shell,
                "21:new-shell\n",
                stamp,
                LAUNCHED_ROUTE
            )
            .is_err()
        );
        assert!(continuity(before, after, shell, shell, stamp, "forked successor").is_err());
        assert!(continuity(before, after, "20:\n", "20:\n", stamp, LAUNCHED_ROUTE).is_err());
    }

    #[test]
    fn the_successor_must_report_the_same_complete_build_identity() {
        let before = "OK version=1.2.0 build=123 commit=abc profile=debug";
        assert!(same_image(before, before));
        assert!(!same_image(before, "OK version=1.2.0 build=124 commit=abc"));
        assert!(!same_image(before, "OK version=1.2.0 build=123 commit=def"));
        assert!(!same_image("OK version=1.2.0", "OK version=1.2.0"));
        assert!(!same_image(
            "OK version= build= commit=",
            "OK version= build= commit="
        ));
    }
}
