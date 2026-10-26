// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who is at this Mac: the one person-at-console predicate every surface that changes the
//! person's own Mac asks first (`aterm init`'s workstation layer, `aterm doctor --apply` and
//! `--restore`, a window's first-run setup, the terminal lane's daily host settings).
//!
//! Owning `/dev/console` is not enough. An agent's tool call, a script, and an SSH session
//! all run under the console owner's uid, and none of them is the person: an agent's
//! `aterm init` replaced the Dock and could sign the owner out of Messages, and `ssh mac
//! aterm init` relaunched the Dock on the console's screen. So [`presence`] also asks this
//! process's own session: whether it came in over SSH, whether its audit session reaches the
//! window server, whether a person can answer at its terminal, and whether a coding agent
//! started it. Every fact comes through [`System`], so a test answers from a fake.

use crate::port::System;
use std::path::Path;

/// Who is at this process, as [`presence`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// A person at this Mac's console: this login owns it, the process runs in its graphic
    /// session, and a person can answer at its terminal.
    AtConsole,
    /// A remote shell (SSH), or a session without the window server: this login owns the
    /// console, but this process does not run in that session.
    Remote,
    /// In the console's own session with nobody to ask: no controlling terminal (Claude
    /// Code's Bash tool, a script, a launch agent) or input from a file.
    NoTerminal,
    /// In the console's own session at a terminal, started by a coding agent.
    AgentTab,
    /// Another user is logged in at this Mac's console.
    OtherLogin,
    /// Nobody is logged in at this Mac's screen (the login window holds it), or it cannot be
    /// read.
    NoConsole,
}

impl Presence {
    /// Why nobody here may change the person's own Mac, in a few words; `None` at the
    /// console.
    #[must_use]
    pub const fn why(self) -> Option<&'static str> {
        match self {
            Self::AtConsole => None,
            Self::Remote => Some("this is a remote shell"),
            Self::NoTerminal => Some("there is no terminal to ask at"),
            Self::AgentTab => Some("a coding agent started this"),
            Self::OtherLogin => Some("another user is logged in there"),
            Self::NoConsole => Some("nobody is logged in at its screen"),
        }
    }

    /// The one line a surface says where a change to the person's Mac waits for them
    /// (`aterm init`'s deferred workstation row, `aterm doctor --apply`'s refusal); `None`
    /// at the console.
    #[must_use]
    pub fn needs_person(self) -> Option<String> {
        self.why().map(|why| {
            format!("needs a person at this Mac's console ({why}): run aterm init there")
        })
    }
}

/// Who is at this process, asked in order: who owns `/dev/console`, whether this process
/// came in remotely or runs outside the window server's session, whether it has a terminal
/// a person answers at, and whether a coding agent is among its ancestors. Each read is
/// local and bounded (a stat, the environment, the audit session's flags, `/dev/tty`, a
/// walk up the process table), and stops at the first answer.
#[must_use]
pub fn presence(sys: &dyn System) -> Presence {
    if let Some(away) = away(sys) {
        return away;
    }
    if !sys.terminal() {
        return Presence::NoTerminal;
    }
    if sys.agent_ancestor().is_some() {
        return Presence::AgentTab;
    }
    Presence::AtConsole
}

/// Whether this process runs outside the console owner's own graphic session ([`presence`]'s
/// first two questions only: no terminal or ancestry read): an SSH session, a session
/// without the window server, another login at the console, or nobody at it.
#[must_use]
pub fn remote(sys: &dyn System) -> bool {
    away(sys).is_some()
}

/// [`presence`] of this process, read on the live system.
#[must_use]
pub fn here() -> Presence {
    presence(&crate::port::Live::new())
}

/// [`remote`] of this process, read on the live system.
#[must_use]
pub fn remote_here() -> bool {
    remote(&crate::port::Live::new())
}

/// Where this process runs outside the console owner's own graphic session ([`remote`]), which
/// of [`Presence::Remote`], [`Presence::OtherLogin`] and [`Presence::NoConsole`] it is; `None`
/// in that session.
#[must_use]
pub fn away(sys: &dyn System) -> Option<Presence> {
    match sys.stat(Path::new("/dev/console")) {
        Err(_) => return Some(Presence::NoConsole),
        Ok(console) if console.uid == sys.euid() => {}
        // The login window owns it while nobody is logged in at the screen.
        Ok(console) if console.uid == 0 => return Some(Presence::NoConsole),
        Ok(_) => return Some(Presence::OtherLogin),
    }
    (sys.remote_login() || sys.graphic_session() != Some(true)).then_some(Presence::Remote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Stat};

    fn console(uid: u32) -> Stat {
        Stat {
            uid,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        }
    }

    /// A person at the console reads `AtConsole`; each fact that says otherwise turns it,
    /// the earlier question first: console ownership, then SSH and the session, then the
    /// terminal, then an agent.
    #[test]
    fn presence_is_at_console_only_when_every_fact_agrees() {
        struct Facts {
            console: Option<u32>,
            remote_login: bool,
            graphic: Option<bool>,
            terminal: bool,
            agent: Option<&'static str>,
        }
        let person = || Facts {
            console: Some(501),
            remote_login: false,
            graphic: Some(true),
            terminal: true,
            agent: None,
        };
        let rows = [
            (person(), Presence::AtConsole),
            (
                Facts {
                    console: None,
                    ..person()
                },
                Presence::NoConsole,
            ),
            (
                Facts {
                    console: Some(0),
                    ..person()
                },
                Presence::NoConsole,
            ),
            (
                Facts {
                    console: Some(502),
                    ..person()
                },
                Presence::OtherLogin,
            ),
            (
                Facts {
                    remote_login: true,
                    ..person()
                },
                Presence::Remote,
            ),
            (
                Facts {
                    graphic: Some(false),
                    ..person()
                },
                Presence::Remote,
            ),
            (
                Facts {
                    graphic: None,
                    ..person()
                },
                Presence::Remote,
            ),
            (
                Facts {
                    terminal: false,
                    ..person()
                },
                Presence::NoTerminal,
            ),
            (
                Facts {
                    agent: Some("Claude Code"),
                    ..person()
                },
                Presence::AgentTab,
            ),
            // The earlier question decides: SSH without a terminal is remote, an agent
            // without a terminal has nobody to ask, another login's console is theirs.
            (
                Facts {
                    remote_login: true,
                    terminal: false,
                    ..person()
                },
                Presence::Remote,
            ),
            (
                Facts {
                    terminal: false,
                    agent: Some("Codex"),
                    ..person()
                },
                Presence::NoTerminal,
            ),
            (
                Facts {
                    console: Some(502),
                    remote_login: true,
                    ..person()
                },
                Presence::OtherLogin,
            ),
        ];
        for (facts, want) in rows {
            let mut sys = Fake::root();
            sys.euid = 501;
            match facts.console {
                Some(uid) => {
                    sys.stat_of("/dev/console", console(uid));
                }
                None => {
                    sys.stat_fails("/dev/console", std::io::ErrorKind::NotFound);
                }
            }
            sys.remote_login = facts.remote_login;
            sys.graphic_session = facts.graphic;
            sys.terminal = facts.terminal;
            sys.agent = facts.agent;
            let got = presence(&sys);
            assert_eq!(got, want);
            assert_eq!(got.needs_person().is_none(), want == Presence::AtConsole);
            assert_eq!(
                remote(&sys),
                !matches!(
                    want,
                    Presence::AtConsole | Presence::NoTerminal | Presence::AgentTab
                ),
                "{want:?}"
            );
            assert!(sys.calls().is_empty(), "presence runs no command");
        }
    }
}
