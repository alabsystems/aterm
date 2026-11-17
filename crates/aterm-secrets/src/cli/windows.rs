// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets` on Windows: a member that reads its vault. It joins one (a vault at a URL,
//! where an admin Mac approves it by its code or a member's Allow lets it in, or a vault at a
//! drive or share path), syncs its newest version when asked, and reads every entry in place
//! (`status`, `join`, `sync`, `mirrors`, `ls`, `get`), through the code Unix runs
//! (`super::member`). No vault subscriber, approver, server or service runs on Windows, and no
//! Windows member writes its vault: every other verb says so and changes nothing
//! (`docs/DESIGN-secrets-2026-10-20.md` §12).

use super::Command;
use super::member::{self, JoinHost, StatusHost};
use crate::client::{self, Paths};
use std::io::{Read, Write};
use std::process::ExitCode;

/// What `status` says of who writes a Windows member's vault.
const WRITES: &str = "vault writes: none from this PC: a Windows member reads its vault; set, \
                      unset and an admin's verbs run on a Mac or Linux member";

/// What `status` says runs for a Windows member's vault.
const NO_SUBSCRIBER: &str = "subscriber: none on Windows: this PC reads the vault's newest \
                             version when aterm secrets sync runs";

/// What a join on Windows says once it asked: nothing follows the vault here by itself.
const FOLLOW: &str = "no vault subscriber runs on Windows, so nothing follows the vault here by \
                      itself: aterm secrets sync reads its newest version, and finishes a join \
                      once this PC is let in";

/// The verbs a Windows member runs.
const READS: &str = "status, join, sync, mirrors, ls and get";

/// Run `command` as a Windows member of the vault under this user's home.
pub(super) fn run(command: Command) -> crate::Result<ExitCode> {
    run_at(command, &Paths::live)
}

/// [`run`] with the paths `live` gives: read only by the verbs that read the vault, never by one
/// that is refused.
fn run_at(command: Command, live: &dyn Fn() -> crate::Result<Paths>) -> crate::Result<ExitCode> {
    let refused = |verb: &str| {
        eprintln!(
            "aterm secrets {verb}: not available on Windows: a Windows member reads its vault \
             ({READS}); {verb} runs on a Mac or Linux machine"
        );
        Ok(ExitCode::from(2))
    };
    match command {
        Command::Status => member::status(
            &live()?,
            &StatusHost {
                writes: &|_| vec![WRITES.to_owned()],
                release: &|_| None,
                services: &services,
            },
        ),
        Command::Join {
            source,
            name,
            agent,
            wait,
            password_stdin,
        } => {
            let code = member::join(
                &live()?,
                &source,
                name,
                agent,
                wait,
                password_stdin,
                &JoinHost {
                    ask: &ask,
                    ask_hidden: None,
                    subscribe: None,
                    no_agent: &|| {},
                },
            )?;
            eprintln!("{FOLLOW}");
            Ok(code)
        }
        Command::Sync { diagnostic } => member::sync(&live()?, diagnostic),
        Command::Mirrors { change, urls } => member::mirrors(&live()?, change, &urls),
        Command::Ls { describe, json } => member::ls(&live()?, describe, json),
        Command::Get(name) => member::get(&live()?, &name),
        Command::StatusJson => refused("status --json"),
        Command::Repair { .. } => refused("repair"),
        Command::Watch => refused("watch"),
        Command::GitCredential { .. } => refused("git-credential"),
        Command::GitSetup { .. } => refused("git-setup"),
        Command::Create { .. } => refused("create"),
        Command::Edit => refused("edit"),
        Command::Set { .. } => refused("set"),
        Command::Unset { .. } => refused("unset"),
        Command::Password { .. } => refused("password"),
        Command::Requests { .. } => refused("requests"),
        Command::Approve { .. } => refused("approve"),
        Command::Revoke { .. } => refused("revoke"),
        Command::Retire(_) => refused("retire"),
        Command::Admins { .. } => refused("admins"),
        Command::RotateKey => refused("rotate-key"),
        Command::CommonsInit | Command::CommonsRestore { .. } => refused("commons"),
        Command::Genesis(_) => refused("genesis"),
        Command::Serve(_) => refused("serve"),
        Command::Agent { .. } => refused("agent"),
        Command::Server { .. } => refused("server"),
    }
}

/// `status`'s last line on Windows, for a configured vault: no subscriber follows it.
fn services(_: &Paths, status: &client::Status) {
    if status.config.is_some() {
        println!("{NO_SUBSCRIBER}");
    }
}

/// The longest line [`ask`] keeps.
const ASK_MAX: usize = 4096;

/// One line typed at this process's console (never stdin), echo on: the prompt on `CONOUT$`,
/// the line read from `CONIN$` in the console's own line mode. `None` with no console.
fn ask(prompt: &str) -> Option<String> {
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .open("CONOUT$")
        .ok()?;
    let mut input = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .ok()?;
    out.write_all(prompt.as_bytes()).ok()?;
    out.flush().ok()?;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while input.read(&mut byte).ok()? == 1 {
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() > ASK_MAX {
            return None;
        }
    }
    Some(String::from_utf8_lossy(&line).trim().to_string())
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
