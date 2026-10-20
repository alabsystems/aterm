// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keychain tripwire: this program under the name `security`, first on the
//! `PATH` of the lab's Claude Code (`runs/<tag>/shim/security`). It NEVER runs
//! the real tool and never touches a keychain: it logs what was asked
//! (sub-command, `-a` account, `-s` service; never a `-X`/`-w` value and never
//! a stdin payload) to `$LAB_SECURITY_LOG` and answers "item not found" (exit
//! 44) to reads and deletes, "ok" (exit 0) to writes it silently drops.

use std::io::{BufRead as _, Write as _};

/// What one call asked: `(sub-command, account, service)`, `-` for one not given.
#[must_use]
pub fn parse_argv(args: &[String]) -> (String, String, String) {
    let cmd = args.first().cloned().unwrap_or_default();
    let (mut acct, mut svc) = ("-".to_string(), "-".to_string());
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-a" => {
                acct = args.get(i + 1).cloned().unwrap_or_default();
                i += 1;
            }
            "-s" => {
                svc = args.get(i + 1).cloned().unwrap_or_default();
                i += 1;
            }
            // options that take a value that must not be logged
            "-X" | "-p" | "-j" | "-l" | "-D" | "-C" | "-T" | "-c" | "-r" | "-G" | "-t" | "-y"
            | "-k" => i += 1,
            _ => {}
        }
        i += 1;
    }
    (cmd, acct, svc)
}

/// The value of the LAST ` <flag> "…"` in an interactive line, `-` when none.
#[must_use]
pub fn quoted_after(line: &str, flag: &str) -> String {
    let needle = format!(" {flag} \"");
    let Some(i) = line.rfind(&needle) else {
        return "-".into();
    };
    let rest = &line[i + needle.len()..];
    rest.find('"')
        .map_or_else(|| "-".to_string(), |j| rest[..j].to_string())
}

/// The exit status a sub-command is answered with.
#[must_use]
pub fn answer(cmd: &str) -> i32 {
    match cmd {
        "find-generic-password"
        | "find-internet-password"
        | "find-certificate"
        | "find-identity"
        | "delete-generic-password"
        | "delete-internet-password" => 44,
        "show-keychain-info" | "add-generic-password" | "add-internet-password" => 0,
        _ => 1,
    }
}

/// `security …` as the tripwire answers it.
#[must_use]
pub fn main(args: &[String]) -> i32 {
    let log = std::env::var("LAB_SECURITY_LOG").unwrap_or_else(|_| "/dev/null".into());
    let ts = crate::util::hms(crate::util::now_secs());
    let (pid, ppid) = (std::process::id(), crate::sys::getppid());
    // one write per line: concurrent calls append whole lines (O_APPEND)
    let say = |via: &str, cmd: &str, acct: &str, svc: &str| {
        let line = format!(
            "{ts} {cmd} account=\"{acct}\" service=\"{svc}\" via={via} pid={pid} ppid={ppid}\n"
        );
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            let _ = f.write_all(line.as_bytes());
        }
    };
    if args.first().map(String::as_str) == Some("-i") {
        // interactive mode: sub-commands arrive on stdin, one per line
        let mut last = 0;
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if line.is_empty() {
                continue;
            }
            let cmd = line.split_whitespace().next().unwrap_or("").to_string();
            say(
                "stdin",
                &cmd,
                &quoted_after(&line, "-a"),
                &quoted_after(&line, "-s"),
            );
            last = if cmd.starts_with("find-") || cmd.starts_with("delete-") {
                44
            } else {
                0
            };
        }
        return last;
    }
    let (cmd, acct, svc) = parse_argv(args);
    say("argv", &cmd, &acct, &svc);
    match answer(&cmd) {
        44 => {
            eprintln!(
                "security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain."
            );
            44
        }
        0 => 0,
        _ => {
            eprintln!("security: lab tripwire: '{cmd}' is not available here");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_the_account_and_service_and_never_a_secret_value() {
        let a: Vec<String> = [
            "find-generic-password",
            "-a",
            "me",
            "-w",
            "-s",
            "Claude Code-credentials-1234abcd",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            parse_argv(&a),
            (
                "find-generic-password".into(),
                "me".into(),
                "Claude Code-credentials-1234abcd".into()
            )
        );
        let w: Vec<String> = [
            "add-generic-password",
            "-a",
            "me",
            "-s",
            "svc",
            "-X",
            "deadbeef",
        ]
        .map(String::from)
        .to_vec();
        let (c, ac, s) = parse_argv(&w);
        assert!(!format!("{c}{ac}{s}").contains("deadbeef"));
        assert_eq!(answer("find-generic-password"), 44);
        assert_eq!(answer("add-generic-password"), 0);
        assert_eq!(answer("dump-keychain"), 1);
    }

    #[test]
    fn interactive_lines_name_their_last_account_and_service() {
        let l = r#"add-generic-password -U -a "x" -s "Claude Code" -a "me" -X "6869""#;
        assert_eq!(quoted_after(l, "-a"), "me");
        assert_eq!(quoted_after(l, "-s"), "Claude Code");
        assert_eq!(quoted_after("show-keychain-info", "-a"), "-");
    }
}
