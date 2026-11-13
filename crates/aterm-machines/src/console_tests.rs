// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
