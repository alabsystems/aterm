// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The single PTY spawn and IO seam: every raw PTY primitive (Unix pty pair, `fork`,
//! `execve`, `read`/`write`, `TIOCSWINSZ`, the queue probes [`input_queue_len`],
//! [`output_queue_len`] and [`flush_input_queue`]; Windows ConPTY) lives here, so the
//! frontend holds no unsafe PTY code and a child is spawned in one place.
//!
//! The master is a raw `i32`, shared by the input, reader and control-socket threads.
//! On Unix it is the PTY master fd (`src/unix.rs`); on Windows (`src/windows/`) an
//! opaque, always non-negative key into a process-global registry that owns the
//! HANDLEs. Public signatures match on both; Windows adds [`OwnedMaster`],
//! [`close_master`] and [`exit_code`] for what `OwnedFd`/`waitpid` cover on Unix.

// Registers the `trust` tool namespace for `trust::skip` under the Trust verifier.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

/// A spawned shell: the PTY master and the child's pid. The child is a session and
/// process-group leader, so a `SIGHUP` to `pid` tears the job tree down. The frontend
/// hangs the child up first so the reader's blocking `read(master)` gets EOF, then
/// closes the master: on macOS `close(master)` wedges while a reader sits in `read`.
///
/// Windows: `master` is the opaque ConPTY registry key and `pid` the process id;
/// [`hangup`] closes the pseudoconsole.
#[derive(Debug, Clone, Copy)]
pub struct SpawnedShell {
    /// The PTY master fd.
    pub master: i32,
    /// The child's pid, also its process-group id.
    pub pid: i32,
}

/// The child shell's environment: `inherited` minus every deny-listed key
/// ([`aterm_types::domain::is_ai_env_key`]: AI-tool vars and the recursion-provisioning
/// identity vars; case-insensitive on Windows only), then `env_add` on top, overriding
/// or appending. Built in the parent before the fork, so the child allocates nothing.
// Skip: `inherited` is a caller-chosen iterator, so `next` is open-trait dispatch the
// verifier cannot follow; the deny-list logic is unit-tested.
#[cfg_attr(trust_verify, trust::skip)]
fn build_child_env(
    inherited: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    env_add: &[(String, String)],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    // Plain loops, not adapters: closure bodies are absent callees for the Trust
    // panic-freedom verifier. They keep encounter order and replace the first match.
    let mut env_pairs: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    for (k, v) in inherited {
        if !aterm_types::domain::is_ai_env_key(&k) {
            env_pairs.push((k, v));
        }
    }
    for (k, v) in env_add {
        let key = std::ffi::OsString::from(k);
        let mut replaced = false;
        for pair in env_pairs.iter_mut() {
            if pair.0 == key {
                pair.1 = std::ffi::OsString::from(v);
                replaced = true;
                break;
            }
        }
        if !replaced {
            env_pairs.push((key, std::ffi::OsString::from(v)));
        }
    }
    env_pairs
}

/// The refusal of a spawn whose argument `index` (from 0, as the caller gave `list`)
/// holds a NUL byte. Such an argument cannot reach the program whole, and dropping or
/// cutting it would shift the command, so nothing is spawned.
pub(crate) fn argv_nul_refusal(list: &str, index: usize) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "argument {index} of {list} holds a NUL byte, which no program can be handed: not \
             started, since leaving it out would shift every argument after it"
        ),
    )
}

/// The refusal of a spawn whose program, `what` (the configured shell and where it was
/// set), holds a NUL byte: the OS would read the name only up to it and run a different
/// program, so nothing is spawned.
pub(crate) fn program_nul_refusal(what: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "{what} holds a NUL byte, which no file name can hold: not started, since running \
             it would start a different program"
        ),
    )
}

/// What the line discipline does with the next byte typed into a PTY: the `c_lflag`
/// bits that decide who echoes a keypress, and the `ISIG` characters ([`TtySignals`]),
/// read by [`tty_echo`] at the press.
///
/// | `echo` | `canonical` | who is at the slave                                    | who echoes  |
/// |--------|-------------|--------------------------------------------------------|-------------|
/// | true   | true        | a cooked prompt, `sleep`, `cat`                        | the kernel  |
/// | false  | true        | `read -s`, `sudo`, `ssh`'s passphrase, `getpass`       | nobody      |
/// | false  | false       | a raw TUI (vim, less), readline at rest, `read -s -n`  | the program |
/// | true   | false       | `read -n 3`, a cbreak program that leaves ECHO on      | the kernel  |
///
/// Only the second row swallows input; a raw program draws its own echo, and a
/// readline prompt sits in raw mode between keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TtyEcho {
    /// `ECHO` is set: the line discipline echoes input it processes.
    pub echo: bool,
    /// `ICANON` is set: line-at-a-time input, so echo is the kernel's job.
    pub canonical: bool,
    /// The signal characters while `ISIG` is set; `None` when it is clear
    /// (raw mode), where every byte is queued for the program to read.
    pub signals: Option<TtySignals>,
}

impl TtyEcho {
    /// The press reaches the program but never the screen (canonical no-echo, iTerm2's
    /// password mode). A raw-mode program echoes for itself, so it is `false`.
    #[must_use]
    pub const fn swallows_input(self) -> bool {
        !self.echo && self.canonical
    }
}

/// The slave's signal characters (`c_cc[VINTR]`, `c_cc[VQUIT]`, `c_cc[VSUSP]`), read
/// while `ISIG` is set. The line discipline turns each into a signal for the
/// foreground group as it is written: the byte never enters the input queue, and
/// (unless `NOFLSH`) the queue is flushed. With `ISIG` off they queue like any byte. A
/// remapped `VINTR` moves the signal with it. `None` is a disabled character
/// (`_POSIX_VDISABLE`: 0xff on macOS, NUL on Linux).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TtySignals {
    /// `VINTR` (SIGINT; `^C` by default).
    pub intr: Option<u8>,
    /// `VQUIT` (SIGQUIT; `^\` by default).
    pub quit: Option<u8>,
    /// `VSUSP` (SIGTSTP; `^Z` by default).
    pub susp: Option<u8>,
}

impl TtySignals {
    /// Whether `byte`, written into the PTY, is turned into a signal rather
    /// than queued.
    #[must_use]
    pub fn signals(self, byte: u8) -> bool {
        [self.intr, self.quit, self.susp].contains(&Some(byte))
    }
}

/// Which process a session's shell pid named when the session took it; every
/// pid-addressed signal at teardown re-verifies it first.
///
/// A shell adopted across a seamless update has launchd as its parent, so its pid is
/// freed when it exits; if the pane stays open and the pid is reissued, a teardown
/// `killpg(pid, …)` would hit an unrelated group (`getpgid(pid) == pid` holds for any
/// group leader). So the session records the process start time while the pid is
/// provably the shell ([`record_spawned_shell`], [`record_adopted_shell`]), and
/// [`ShellIdentity::verify`] compares it with the kernel's current answer. With nothing
/// proven, teardown signals only the terminal's foreground group ([`hangup_shell`]).
///
/// A value, not a capability: the check is always against the kernel, which no caller
/// can forge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellIdentity {
    /// The shell's pid, also its process-group and session id.
    pub pid: i32,
    /// The start time recorded when the session took the shell, in a per-platform unit
    /// (microseconds since the epoch on macOS, clock ticks since boot on Linux). `None`
    /// when it could not be recorded.
    pub birth: Option<u64>,
    /// The shell is this process's own child (a fresh spawn). Until it is reaped its
    /// pid cannot be reissued, so an unreaped-child answer from `waitid(WNOWAIT)` is
    /// proof, and the only one once the shell is a zombie (libproc reports no start
    /// time for a zombie).
    pub own_child: bool,
}

impl ShellIdentity {
    /// An identity from recorded parts (production records through
    /// [`record_spawned_shell`] / [`record_adopted_shell`]).
    #[must_use]
    pub const fn from_parts(pid: i32, birth: Option<u64>, own_child: bool) -> Self {
        Self {
            pid,
            birth,
            own_child,
        }
    }

    /// Nothing recorded, nothing ever proven: [`Self::verify`] is `false` for
    /// every pid, so teardown never signals by pid. Stub sessions (pid `-1`)
    /// carry this.
    #[must_use]
    pub const fn unproven(pid: i32) -> Self {
        Self::from_parts(pid, None, false)
    }
}

/// What [`hangup_shell`] signalled, so tests observe teardown's choice directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellHangup {
    /// The recorded shell was verified and its process group got `SIGHUP`
    /// (`docs/DESIGN-pty-keeper-2026-09-26.md` F3).
    Shell(i32),
    /// The shell could not be verified; the terminal's foreground process group
    /// (`tcgetpgrp(master)`) got `SIGHUP` instead.
    Foreground(i32),
    /// Neither: nothing was signalled.
    Nothing,
}

/// How a spawned child ended, as far as the platform can say.
///
/// [`collect_exit_status`] never blocks, so "not yet exited" and "not ours to reap"
/// both answer `None`. An adopted session (a child of the previous aterm) never yields
/// a status: treat `None` as unknown, never as success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildExit {
    /// Normal termination with this status code.
    Code(i32),
    /// Killed by this signal (Unix only).
    Signal(u8),
}

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

// Tests fork outside `InheritStrip::prepare` (raw `fork`s, a `setsid` stand-in's
// `pre_exec`) while sibling threads call libnotify, and a child can die inside `fork()`
// (4 in 160 loaded runs). So libnotify's once-gate is passed at image load.
#[cfg(all(test, target_os = "macos"))]
aterm_uds::pass_libnotify_gate_at_load!();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_child_env_drops_denylisted_and_keeps_overrides() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let inherited = vec![
            (os("PATH"), os("/usr/bin")),
            (os("ATERM_SESSION_ID"), os("s-0123456789abcdef0123")),
            (os("ANTHROPIC_API_KEY"), os("secret")),
            (os("CLAUDECODE"), os("1")),
            (os("CURSOR_TRACE_ID"), os("xyz")),
            (os("TERM"), os("dumb")),
        ];
        let env_add = vec![("TERM".to_string(), "xterm-256color".to_string())];
        let out = build_child_env(inherited.into_iter(), &env_add);
        let keys: Vec<String> = out
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for denied in [
            "ATERM_SESSION_ID",
            "ANTHROPIC_API_KEY",
            "CLAUDECODE",
            "CURSOR_TRACE_ID",
        ] {
            assert!(
                !keys.contains(&denied.to_string()),
                "{denied} must be filtered from the child env, got {keys:?}"
            );
        }
        assert!(
            keys.contains(&"PATH".to_string()),
            "PATH must pass through to the child"
        );
        // `env_add` overrides the inherited TERM, which appears exactly once.
        let terms: Vec<&OsString> = out
            .iter()
            .filter(|(k, _)| k == &os("TERM"))
            .map(|(_, v)| v)
            .collect();
        assert_eq!(terms.len(), 1, "TERM must appear exactly once");
        assert_eq!(
            terms[0],
            &os("xterm-256color"),
            "env_add must override inherited TERM"
        );
    }

    /// A non-UTF-8 key with a denied prefix is still denied; the last key is the control.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_key_with_a_denied_prefix_is_still_denied() {
        use std::os::unix::ffi::OsStringExt as _;
        let raw = |b: &[u8]| std::ffi::OsString::from_vec(b.to_vec());
        let inherited = vec![
            (raw(b"ANTHROPIC_\xffX"), raw(b"secret")),
            (raw(b"CLAUDE\xfe"), raw(b"1")),
            (raw(b"PLAIN_\xffKEY"), raw(b"kept")),
        ];
        let keys: Vec<Vec<u8>> = build_child_env(inherited.into_iter(), &[])
            .into_iter()
            .map(|(k, _)| k.into_vec())
            .collect();
        assert_eq!(keys, vec![b"PLAIN_\xffKEY".to_vec()]);
    }

    /// An identity's agent-home variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) arrives through
    /// `env_add`: it replaces the parent's, and no other `CLAUDE*`/`CODEX_*` key survives.
    #[test]
    fn build_child_env_replaces_the_parents_agent_home_with_the_identitys() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let table = [
            ("CLAUDE_CONFIG_DIR", "/state/identities/worker/.claude"),
            ("CODEX_HOME", "/state/identities/worker/.codex"),
        ];
        for (var, identity_value) in table {
            let inherited = vec![
                (os("HOME"), os("/Users//human")),
                (os(var), os("/Users//human/agent-config-of-the-human")),
                (os("CLAUDECODE"), os("1")),
                (os("CLAUDE_CODE_ENTRYPOINT"), os("cli")),
                (os("CODEX_SANDBOX"), os("seatbelt")),
                (os("PATH"), os("/usr/bin")),
            ];
            let env_add = vec![(var.to_string(), identity_value.to_string())];
            let out = build_child_env(inherited.into_iter(), &env_add);
            let values: Vec<(String, String)> = out
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect();
            let of_var: Vec<&str> = values
                .iter()
                .filter(|(k, _)| k == var)
                .map(|(_, v)| v.as_str())
                .collect();
            assert_eq!(
                of_var,
                vec![identity_value],
                "{var}: exactly once, and it is the identity's value, not the parent's"
            );
            assert!(
                !values
                    .iter()
                    .any(|(_, v)| v.contains("agent-config-of-the-human")),
                "{var}: the parent's value is gone from the child env: {values:?}"
            );
            let agent_keys: Vec<&str> = values
                .iter()
                .map(|(k, _)| k.as_str())
                .filter(|k| k.starts_with("CLAUDE") || k.starts_with("CODEX_"))
                .collect();
            assert_eq!(
                agent_keys,
                vec![var],
                "{var}: no other CLAUDE*/CODEX_* key of the parent's rides along"
            );
            assert!(
                values
                    .iter()
                    .any(|(k, v)| k == "HOME" && v == "/Users//human"),
                "HOME is the human's: an identity moves the agent's config, not the user"
            );
        }
    }

    // Windows env names are case-insensitive, so any casing of a denied key is denied.
    #[cfg(windows)]
    #[test]
    fn build_child_env_denies_mixed_case_keys_on_windows() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let inherited = vec![
            (os("Path"), os("C:\\Windows")),
            (os("anthropic_api_key"), os("secret")),
            (os("Claude_Code"), os("1")),
            (os("Aterm_Containment_Mode"), os("containment")),
            (os("cursor_trace_id"), os("xyz")),
        ];
        let out = build_child_env(inherited.into_iter(), &[]);
        let keys: Vec<String> = out
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            keys,
            vec!["Path".to_string()],
            "every mixed-case deny-listed key must be filtered on Windows"
        );
    }

    // Unix env names are case-sensitive: lowercase `anthropic_api_key` is another variable.
    #[cfg(unix)]
    #[test]
    fn build_child_env_stays_case_sensitive_on_unix() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let inherited = vec![(os("anthropic_api_key"), os("distinct-var"))];
        let out = build_child_env(inherited.into_iter(), &[]);
        assert_eq!(out.len(), 1, "exact-case deny must not match on unix");
    }
}
