// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines agent …`: the host side (design §3, §4).
//!
//! | verb | runs as | what |
//! |---|---|---|
//! | `ssh` | the admin, as the fleet key's forced command | reads `SSH_ORIGINAL_COMMAND` and runs one of `hello`, `put`, `apply`, `attach`, `screen` ([`dispatch`], [`relay`]) |
//! | `hello` | the admin | one [`crate::protocol::Hello`] JSON line ([`hello`]) |
//! | `put <sha256>` | the admin | standard input into the admin's inbox, checked against the hash ([`inbox`]) |
//! | `apply` | root, through the one sudoers rule | verifies the envelope on standard input, starts the run job, streams it ([`apply`]) |
//! | `run-job <run>` | root, started by `apply` | the run itself, detached from the SSH session ([`job`]) |
//! | `attach <run>` | the admin | streams a run from its start ([`apply::follow`]) |
//! | `put-file <path> <mode>` | a user, never root | standard input atomically into a file in that user's home ([`inbox`]) |
//! | `install …` | root, through a password `sudo` at enrollment only | puts the staged agent in place ([`install`]) |
//! | `lock-screen` | the admin, in their login session, run by `screen` through a signed `exec` | locks that session and reads it back locked ([`lock`]) |
//! | `screen-watch arm …`, `disarm <id>` | root, run by `screen` through a signed `exec` | starts, and ends, a watcher that locks the admin's session should `screen` vanish before its own lock ([`watch`]) |
//! | `screen-watch run <id>` | root, started by `arm` | the watcher itself, detached from the run ([`watch`]) |
//! | `version` | anyone | the agent's version |
//!
//! Nothing here reads the environment, `HOME` or aterm's own configuration, except `ssh`,
//! whose input is `SSH_ORIGINAL_COMMAND`, and `hello`'s tab list, read only in the
//! account's own home (`crate::sessions::is_live_account`): every path comes from
//! [`Layout`], every account from `crate::facts::user`, and every child gets an explicit
//! environment from the port.

use crate::port::System;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub mod apply;
pub mod chain;
pub mod dispatch;
pub mod hello;
pub mod inbox;
pub mod install;
pub mod job;
pub mod lock;
pub mod platform;
pub mod relay;
pub mod state;
pub mod watch;

/// Where everything the agent keeps lives: [`crate::AGENT_ROOT`] on a managed host, a
/// temporary directory in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The agent's root (root:wheel 0755).
    pub root: PathBuf,
}

impl Layout {
    /// The installed layout under [`crate::AGENT_ROOT`].
    #[must_use]
    pub fn installed() -> Self {
        Self {
            root: PathBuf::from(crate::AGENT_ROOT),
        }
    }

    /// `agent/`: the expanded agent (an aterm.app, or a dev binary).
    #[must_use]
    pub fn agent(&self) -> PathBuf {
        self.root.join("agent")
    }

    /// `agent.prev/`: the agent before the last upgrade.
    #[must_use]
    pub fn previous(&self) -> PathBuf {
        self.root.join("agent.prev")
    }

    /// `bin/`.
    #[must_use]
    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// `bin/aterm`: the stable path the sudoers rule and the forced command name, a
    /// root-owned link to the agent's executable.
    #[must_use]
    pub fn exe_link(&self) -> PathBuf {
        self.bin().join("aterm")
    }

    /// `allowed_signers` (0644): who may sign envelopes for this host.
    #[must_use]
    pub fn allowed_signers(&self) -> PathBuf {
        self.root.join("allowed_signers")
    }

    /// `managed.toml` (0644): who enrolled this host and what the agent manages.
    #[must_use]
    pub fn managed(&self) -> PathBuf {
        self.root.join("managed.toml")
    }

    /// `.metadata_never_index`: keeps Spotlight and LaunchServices away from the second
    /// aterm.app.
    #[must_use]
    pub fn never_index(&self) -> PathBuf {
        self.root.join(".metadata_never_index")
    }

    /// `.staging/`: where enrollment expands the agent archive before `install`.
    #[must_use]
    pub fn staging(&self) -> PathBuf {
        self.root.join(".staging")
    }

    /// `state/` (0755): floors, the run lock, the journal and the runs.
    #[must_use]
    pub fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    /// `state/floors.toml` (0644, so `hello` reads it as the admin).
    #[must_use]
    pub fn floors(&self) -> PathBuf {
        self.state().join("floors.toml")
    }

    /// `state/run.lock` (0644): `pid=<n> run=<id>` of the run in progress.
    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.state().join("run.lock")
    }

    /// `state/run.lock.guard` (0600): the `flock` every change of `run.lock` is made under.
    #[must_use]
    pub fn lock_guard(&self) -> PathBuf {
        self.state().join("run.lock.guard")
    }

    /// `state/last_run` (0644): the newest finished run's id.
    #[must_use]
    pub fn last_run(&self) -> PathBuf {
        self.state().join("last_run")
    }

    /// `state/last_busy` (0644): the newest signed request refused `busy`, named as its run
    /// would have been: a heartbeat of the screen watch ([`watch`]).
    #[must_use]
    pub fn last_busy(&self) -> PathBuf {
        self.state().join("last_busy")
    }

    /// `state/screen-watch/` (0700): `screen`'s armed watchers and what each did ([`watch`]).
    #[must_use]
    pub fn screen_watch(&self) -> PathBuf {
        self.state().join("screen-watch")
    }

    /// `state/journal.ndjson` (0600): one line per change, written before it is applied.
    #[must_use]
    pub fn journal(&self) -> PathBuf {
        self.state().join("journal.ndjson")
    }

    /// `state/runs/`.
    #[must_use]
    pub fn runs(&self) -> PathBuf {
        self.state().join("runs")
    }

    /// `state/runs/<id>/`.
    #[must_use]
    pub fn run_dir(&self, id: &str) -> PathBuf {
        self.runs().join(id)
    }

    /// `cache/` (0755): verified artifacts, one `<sha256>` file each.
    #[must_use]
    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// `cache/<sha256>`.
    #[must_use]
    pub fn cached(&self, sha256: &str) -> PathBuf {
        self.cache().join(sha256.to_ascii_lowercase())
    }

    /// `stage/` (root-only work space): envelope scratch files, expanded archives.
    #[must_use]
    pub fn stage(&self) -> PathBuf {
        self.root.join("stage")
    }

    /// `inbox/`: where an artifact copied from the admin's inbox lands, root-owned, until
    /// its hash is checked and it moves into `cache/`.
    #[must_use]
    pub fn inbox(&self) -> PathBuf {
        self.root.join("inbox")
    }
}

/// The admin's upload inbox: `put` writes here as the admin, `apply` copies from here.
#[must_use]
pub fn user_inbox(home: &str) -> PathBuf {
    Path::new(home).join("Library/Caches/aterm-machines/inbox")
}

/// The item families this agent knows, as the baseline's tables name them.
pub const FAMILIES: &[&str] = &[
    "names",
    "sharing",
    "firewall",
    "time",
    "power",
    "updates",
    "keyboard",
    "consumer",
    "setup",
    "defaults",
    "developer",
    "homebrew",
    "app",
    "aterm",
    "github",
    "person",
];

/// The exit status of a refusal (design `protocol`: `refused: <why>`, status 2).
pub const REFUSED: u8 = 2;

/// `aterm machines agent <args>`.
#[must_use]
pub fn main(args: &[String], sys: &dyn System) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("refused: the agent cannot name its own executable: {e}");
            return ExitCode::from(REFUSED);
        }
    };
    let layout = Layout::installed();
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    run(sys, &layout, &exe, &words, &mut stdin, &mut stdout)
}

/// [`main`] with its process inputs explicit.
pub fn run(
    sys: &dyn System,
    layout: &Layout,
    exe: &Path,
    args: &[&str],
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    match args {
        ["ssh"] => {
            // The one environment read in the agent: the forced command's input.
            let original = std::env::var("SSH_ORIGINAL_COMMAND").ok();
            dispatch::ssh(sys, layout, original.as_deref(), stdin, out)
        }
        ["hello"] => hello::print(sys, layout, out),
        ["put", sha] => inbox::put(sys, sha, stdin, out),
        ["apply"] => apply::apply(sys, layout, exe, stdin, out),
        ["run-job", id] => job::run_job(sys, layout, exe, id),
        ["attach", id] => apply::attach(sys, layout, id, out),
        ["put-file", path, mode] => inbox::put_file(sys, Path::new(path), mode, stdin, out),
        ["install", rest @ ..] => install::main(sys, layout, exe, rest, out),
        ["lock-screen"] => lock::lock_screen(sys, out, lock::LOCK_HANG),
        ["screen-watch", rest @ ..] => watch::main(sys, layout, exe, rest, out),
        ["version"] => {
            let _ = writeln!(out, "{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => refuse(
            out,
            &format!(
                "`aterm machines agent {}` is not an agent verb (ssh, hello, put, apply, \
                 run-job, attach, put-file, install, lock-screen, screen-watch, version)",
                args.join(" ")
            ),
        ),
    }
}

/// Print `refused: <why>` and answer status 2.
pub fn refuse(out: &mut dyn Write, why: &str) -> ExitCode {
    let _ = writeln!(out, "refused: {why}");
    let _ = out.flush();
    ExitCode::from(REFUSED)
}

/// Whether `s` is a lowercase SHA-256 hex digest.
#[must_use]
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `s` is a run id this agent makes: `<unix seconds>-<serial>`.
#[must_use]
pub fn is_run_id(s: &str) -> bool {
    let Some((a, b)) = s.split_once('-') else {
        return false;
    };
    let digits = |p: &str| !p.is_empty() && p.len() <= 20 && p.bytes().all(|b| b.is_ascii_digit());
    digits(a) && digits(b)
}

/// The account this process runs as, resolved through the directory (never `HOME`).
///
/// # Errors
/// One line.
pub fn me(sys: &dyn System) -> Result<crate::model::User, String> {
    let ran = sys
        .run(&crate::port::Cmd::new("/usr/bin/id").arg("-un"))
        .map_err(|e| format!("id -un: {e}"))?;
    if !ran.success() {
        return Err(format!("id -un: {}", ran.describe()));
    }
    let name = ran.out().trim().to_owned();
    crate::facts::user(sys, &name)
}

/// Remove `path` and everything below it, never following a link (a link is removed, not
/// entered).
///
/// # Errors
/// The first I/O error.
pub fn remove_tree(sys: &dyn System, path: &Path) -> std::io::Result<()> {
    let st = match sys.stat(path) {
        Ok(st) => st,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if st.dir && !st.symlink {
        for name in sys.list_dir(path)? {
            remove_tree(sys, &path.join(name))?;
        }
    }
    sys.remove(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_and_digests_are_checked_before_they_name_a_path() {
        assert!(is_run_id("1800000000-12"));
        assert!(!is_run_id("../../etc"));
        assert!(!is_run_id("1800000000-"));
        assert!(!is_run_id("1-2-3"));
        assert!(is_sha256(&"a1".repeat(32)));
        assert!(!is_sha256(&"A1".repeat(32)));
        assert!(!is_sha256("../x"));
    }

    #[test]
    fn an_unknown_verb_is_refused_with_status_two() {
        let fake = crate::port::Fake::root();
        let mut out = Vec::new();
        let code = run(
            &fake,
            &Layout::installed(),
            Path::new("/x"),
            &["shell"],
            &mut std::io::empty(),
            &mut out,
        );
        assert_eq!(code, ExitCode::from(REFUSED));
        assert!(String::from_utf8_lossy(&out).starts_with("refused: "));
    }
}
