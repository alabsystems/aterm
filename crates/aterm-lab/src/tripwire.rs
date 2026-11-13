// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keychain tripwire shim (`security`) that logs requests and never touches a keychain.

use std::io::{BufRead as _, Read as _, Write as _};

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
    let say = |via: &str, cmd: &str, acct: &str, svc: &str| {
        use std::os::unix::fs::OpenOptionsExt as _;
        let line = format!(
            "{ts} {cmd} account=\"{acct}\" service=\"{svc}\" via={via} pid={pid} ppid={ppid}\n"
        );
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&log)
        {
            let _ = f.write_all(line.as_bytes());
        }
    };
    if args.first().map(String::as_str) == Some("-i") {
        const MAX_STDIN_BYTES: u64 = 1 << 20;
        let mut last = 0;
        for line in std::io::stdin()
            .lock()
            .take(MAX_STDIN_BYTES)
            .lines()
            .map_while(Result::ok)
        {
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
#[path = "tripwire_tests.rs"]
mod tests;
