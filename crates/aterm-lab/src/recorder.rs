// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab `open` / `$BROWSER` shim recorder and its [`Witness`] / [`control`] checks.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::procs::JailOut;
use crate::run::Run;

/// What the recorder's [`control`] opens: a scheme nothing on a desktop answers.
pub const CONTROL_URL: &str = "x-lab-control://selftest";

/// The proofs' check of the recorder, worded the same in each.
pub const NOTHING_OPENED: &str = "nothing was opened on the desktop: the open recorder logged nothing from the lab (its \
                                  control, a bare-name open in the jail, was logged)";

/// The recorder's control ([`CONTROL_URL`]).
pub fn control(run: &Run) -> PathBuf {
    let shim_path = format!("{}:/usr/bin:/bin", run.run.join("shim").display());
    let log = run.w.join("open-control.log");
    let _ = crate::procs::jailed(
        run,
        &run.w,
        &[
            ("PATH", &shim_path),
            ("LAB_OPEN_LOG", &log.to_string_lossy()),
        ],
        &["open", CONTROL_URL],
        JailOut::Capture,
    );
    log
}

/// What the recorder logged: the lab's agents' opens (`open.log`) and its control's ([`control`]).
#[derive(Debug, Default)]
pub struct Witness {
    /// `<scratch>/open.log`, which every agent's launch line names.
    pub opened: String,
    /// The control's log.
    pub control: String,
}

impl Witness {
    /// Both logs, read through no link.
    #[must_use]
    pub fn read(run: &Run, control: &Path) -> Witness {
        Witness {
            opened: crate::util::scratch_text(&run.w.join("open.log")),
            control: crate::util::scratch_text(control),
        }
    }

    /// The proofs' section on it.
    pub fn print(&self) {
        println!(
            "== the open recorder (a URL or app something tried to open; its control: {})",
            self.control.chars().take(80).collect::<String>().trim_end()
        );
        println!(
            "  {}",
            if self.opened.trim().is_empty() {
                "(nothing)"
            } else {
                self.opened.trim_end()
            }
        );
    }

    /// [`nothing_opened`] of the two logs.
    #[must_use]
    pub fn ok(&self) -> bool {
        nothing_opened(&self.opened, &self.control)
    }
}

/// The verdict on the recorder.
#[must_use]
pub fn nothing_opened(opened: &str, control: &str) -> bool {
    let logged = format!(" open {CONTROL_URL}");
    opened.trim().is_empty() && control.lines().any(|l| l.ends_with(&logged))
}

/// What one argument is logged as.
#[must_use]
pub fn logged(arg: &str) -> Option<String> {
    if arg.contains("://") {
        let (scheme, rest) = arg.split_once("://")?;
        let scheme_ok = scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
        if !scheme_ok {
            return Some(arg.to_string());
        }
        let host: String = rest.chars().take_while(|c| !"/?#".contains(*c)).collect();
        return Some(format!("{scheme}://{host}"));
    }
    if arg.starts_with('-') {
        return None;
    }
    Some("<a path or app>".into())
}

/// `open …` as the recorder answers it: 0, having logged each argument.
#[must_use]
pub fn main(args: &[String]) -> i32 {
    let log = std::env::var("LAB_OPEN_LOG").unwrap_or_else(|_| "/dev/null".into());
    let ts = crate::util::hms(crate::util::now_secs());
    for a in args {
        if let Some(what) = logged(a)
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
        {
            let _ = f.write_all(format!("{ts} open {what}\n").as_bytes());
        }
    }
    0
}

/// `aterm-lab hold <port> <secs>`.
#[must_use]
pub fn hold(args: &[String]) -> i32 {
    let (Some(port), Some(secs)) = (
        args.first().and_then(|p| p.parse::<u16>().ok()),
        args.get(1).and_then(|s| s.parse::<u64>().ok()),
    ) else {
        eprintln!("usage: aterm-lab hold <port> <secs>");
        return 2;
    };
    let Ok(_conn) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return 1;
    };
    std::thread::sleep(std::time::Duration::from_secs(secs));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_is_logged_by_its_scheme_and_host_never_its_query() {
        assert_eq!(
            logged("https://auth.openai.com/oauth/authorize?state=s3cret&code_challenge=x")
                .as_deref(),
            Some("https://auth.openai.com")
        );
        assert_eq!(
            logged("x-lab-control://selftest").as_deref(),
            Some("x-lab-control://selftest")
        );
        assert_eq!(
            logged("/Applications/Safari.app").as_deref(),
            Some("<a path or app>")
        );
        assert_eq!(logged("-a"), None);
    }

    /// The recorder's verdict needs both halves: an empty `open.log` and the
    /// control logged. NEGATIVE CONTROLS: an agent's open in the log fails it,
    /// and so does a control that was never logged (the empty log of a
    /// recorder that logs nothing), or one that logged another URL.
    #[test]
    fn nothing_opened_needs_an_empty_log_and_the_control_logged() {
        let ctl = "18:48:24 open x-lab-control://selftest\n";
        assert!(nothing_opened("", ctl));
        assert!(nothing_opened("\n", ctl));
        assert!(!nothing_opened("18:48:30 open https://claude.ai\n", ctl));
        assert!(!nothing_opened("", ""));
        assert!(!nothing_opened("", "18:48:24 open x-lab-control://other\n"));
        let w = Witness {
            opened: String::new(),
            control: ctl.into(),
        };
        assert!(w.ok());
        let opened = Witness {
            opened: "18:48:30 open <a path or app>\n".into(),
            ..w
        };
        assert!(!opened.ok());
    }
}
