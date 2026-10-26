// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System`]'s live reads of this process's own session, for
//! [`crate::console::presence`]: the remote-login markers in the environment, the audit
//! session's flags, the controlling terminal, and the coding agents among its ancestors.
//! Each is a local read of this process (no tool, no dialog), made only when asked.

/// How many ancestors the agent walk looks at before it stops: a hang detector against a
/// cycle in a racing process table, far past any real shell nesting.
const ANCESTORS: usize = 64;

/// Whether OpenSSH started this process's login: it sets `SSH_CONNECTION` and, with a tty,
/// `SSH_TTY`, and a shell under it inherits both.
pub(super) fn remote_login() -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

/// Whether this process's audit session has the window server and was not established
/// over the network: Security's `SessionGetInfo` on the caller's own session
/// (`AuthSession.h`). An SSH login's session has neither graphic access nor locality; a
/// shell in Terminal.app or an aterm tab inherits the console login's.
#[cfg(target_os = "macos")]
pub(super) fn graphic_session() -> Option<bool> {
    /// `AuthSession.h`: `callerSecuritySession`, `((SecuritySessionId)-1)`.
    const CALLER_SECURITY_SESSION: u32 = u32::MAX;
    /// `AuthSession.h`: `sessionHasGraphicAccess`.
    const SESSION_HAS_GRAPHIC_ACCESS: u32 = 0x0010;
    /// `AuthSession.h`: `sessionIsRemote`.
    const SESSION_IS_REMOTE: u32 = 0x1000;
    // Security is a system framework every aterm process already links (aterm-http's
    // verifier), so declaring the one call here adds no load.
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SessionGetInfo(session: u32, session_id: *mut u32, attributes: *mut u32) -> i32;
    }
    let mut id = 0u32;
    let mut attributes = 0u32;
    // SAFETY: both out-pointers name live locals of the declared C type (`UInt32`); the call
    // only writes them.
    let status =
        unsafe { SessionGetInfo(CALLER_SECURITY_SESSION, &raw mut id, &raw mut attributes) };
    (status == 0).then_some(
        attributes & SESSION_HAS_GRAPHIC_ACCESS != 0 && attributes & SESSION_IS_REMOTE == 0,
    )
}

/// Off macOS there is no window server session to read: unknown.
#[cfg(not(target_os = "macos"))]
pub(super) fn graphic_session() -> Option<bool> {
    None
}

/// Whether a person can answer here: standard input is a terminal, and the controlling
/// terminal opens as one.
pub(super) fn terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal()
        && std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok_and(|tty| tty.is_terminal())
}

/// The first coding agent up this process's parent chain, named by
/// `aterm_agent_names::agent_of` on each ancestor's argv[0], or on its script (argv[1]) when
/// argv[0] is a runtime an agent runs under (`aterm_agent_names::may_host_agent`: an npm install
/// of Claude Code is `node …/bin/claude`); the walk stops at launchd, at an ancestor it
/// cannot read, or after [`ANCESTORS`].
pub(super) fn agent_ancestor() -> Option<&'static str> {
    let mut pid = i32::try_from(std::os::unix::process::parent_id()).ok()?;
    for _ in 0..ANCESTORS {
        if pid <= 1 {
            return None;
        }
        if let Some(argv) = aterm_uds::process::read_process_argv(pid)
            && let Some(agent) = agent_of(&argv)
        {
            return Some(agent);
        }
        pid = i32::try_from(aterm_uds::process::read_process_parent(pid)?).ok()?;
    }
    None
}

/// The coding agent one process's `argv` names ([`agent_ancestor`]), each word by its file
/// name: a path may hold spaces (`~/Library/Application Support/…/bin/claude`), which
/// `aterm_agent_names::agent_of`'s command-line reading would split.
fn agent_of(argv: &[std::ffi::OsString]) -> Option<&'static str> {
    let word = |at: usize| {
        argv.get(at).map(|arg| {
            let path = std::path::Path::new(arg);
            path.file_name()
                .unwrap_or(arg.as_os_str())
                .to_string_lossy()
        })
    };
    let first = word(0)?;
    let named = if aterm_agent_names::may_host_agent(&first) {
        word(1)?
    } else {
        first
    };
    aterm_agent_names::agent_of(&named).map(aterm_agent_names::Agent::product)
}

#[cfg(test)]
mod tests {
    use super::agent_of;
    use std::ffi::OsString;

    /// An agent is named by its own argv[0] or, under a runtime, by its script; a shell, a
    /// runtime running another script, and an empty argv name none.
    #[test]
    fn an_agent_is_named_by_its_program_or_its_script() {
        let argv = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(agent_of(&argv(&["claude"])), Some("Claude Code"));
        assert_eq!(
            agent_of(&argv(&[
                "/Users//a/Library/Application Support/aterm/pkg/store/claude/1/bin/claude",
                "--resume",
            ])),
            Some("Claude Code")
        );
        assert_eq!(
            agent_of(&argv(&["node", "/opt/homebrew/bin/claude", "--resume"])),
            Some("Claude Code")
        );
        assert!(agent_of(&argv(&["/opt/homebrew/bin/codex"])).is_some());
        assert_eq!(agent_of(&argv(&["-zsh"])), None);
        assert_eq!(agent_of(&argv(&["node", "server.js"])), None);
        assert_eq!(agent_of(&argv(&["node"])), None);
        assert_eq!(agent_of(&[]), None);
    }
}
