// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The single PTY spawn + IO seam (ATERM_DESIGN WS-G).
//!
//! Every raw PTY primitive — Unix `open_pty_pair_cloexec` + `fork` +
//! `login_tty`, `execve`, `read`, `write`, `ioctl(TIOCSWINSZ)`, the queue
//! probes [`input_queue_len`] (`FIONREAD`: input the program has not read) and
//! [`output_queue_len`] (`TIOCOUTQ`: output aterm has not read), and
//! [`flush_input_queue`] (`TCIFLUSH`); Windows ConPTY (`CreatePseudoConsole` +
//! anonymous pipes) — lives in this one auditable crate, so the frontend holds
//! no unsafe PTY code and there is exactly one place a child is spawned. The
//! master is a raw `i32` because the frontend shares it across the input,
//! reader and control-socket threads.
//!
//! Platform split (module split, no inline cfg): `src/unix.rs` is the POSIX
//! seam; `src/windows/` is the ConPTY backend. On Unix the
//! `i32` IS the PTY master fd; on Windows it is an opaque, always-`>= 0` key
//! into a process-global session registry that owns the real HANDLEs. Every
//! public signature is identical on both platforms; Windows adds only
//! [`OwnedMaster`]/[`close_master`]/[`exit_code`] for the ownership + exit
//! status seams that `OwnedFd`/`waitpid` cover on Unix.

// Under the Trust verifier, register the `trust` tool namespace so the
// `#[cfg_attr(trust_verify, trust::skip)]` opt-out on `spawn_shell_with_pid`
// resolves; plain rustc never sets `trust_verify`, so this is inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

/// A successfully spawned shell: the PTY master fd plus the child's pid. The
/// child is a session + process-group leader (`fork` -> `login_tty` ->
/// `setsid`), so `pid` doubles as the pgid: a `SIGHUP` to it (or `killpg`) tears
/// the whole job tree down. Exposed so the frontend can HANG UP the child first
/// — making the slave produce EOF so the reader's blocking `read(master)` returns
/// — BEFORE closing the master, instead of racing a blocked reader on the tty
/// lock (the macOS quit-hang: `close(master)` wedges in `lck_mtx_sleep` while the
/// reader sits in `read`).
///
/// Windows: `master` is the opaque ConPTY registry key (still `i32`, still
/// `>= 0`) and `pid` is the Windows process id; [`hangup`] closes the
/// pseudoconsole (the console-close analog of SIGHUP-on-controlling-tty).
#[derive(Debug, Clone, Copy)]
pub struct SpawnedShell {
    /// The PTY master fd.
    pub master: i32,
    /// The child's pid == its process-group id (session leader via `login_tty`).
    pub pid: i32,
}

/// Build the child shell's environment: the `inherited` environment with every
/// deny-listed key removed (AI-tool vars `CLAUDE*`/`ANTHROPIC_*`/`COPILOT_*`/… and
/// the recursion-provisioning identity vars, via the canonical
/// [`aterm_types::domain::is_ai_env_var`]), then `env_add` applied on top —
/// overriding an existing key or appending a new one. So a deny-listed var present
/// in aterm's own environment never leaks into the spawned shell, while explicitly
/// injected vars (TERM, shell integration) are always preserved.
///
/// Called by [`spawn_shell_with_pid`] in the PARENT (before the `fork`), so it stays
/// async-signal-safe (no child-side allocation). A non-UTF-8 key is classified by
/// its lossy spelling, so a deny-listed PREFIX on one still denies it. Pure in
/// its inputs so it is unit-tested without mutating the process-global env.
/// Shared by both platform spawns (the Windows env-block builder applies
/// its case-insensitive dedupe on top of this output). The deny check itself
/// is case-insensitive on Windows only — see
/// [`aterm_types::domain::is_ai_env_key`], the one classifier.
// Skip: the `impl Iterator` parameter is CALLER-CHOSEN code — `next` is an
// open-trait dispatch on a type parameter (the genuinely-fatal class; the
// spawn call sites pass std::env::vars_os, but the signature admits any
// iterator). Deny-list logic is unit-tested; verify-only.
#[cfg_attr(trust_verify, trust::skip)]
fn build_child_env(
    inherited: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    env_add: &[(String, String)],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    // Explicit loops instead of `filter(..).collect()` / `iter_mut().find(..)`:
    // the closure bodies those adapters lower to are absent callees for the Trust
    // panic-freedom verifier, leaving the obligations at the closure sites
    // unprovable. The loops keep encounter order and replace the FIRST match.
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

/// What the line discipline will do with the NEXT byte typed into a PTY —
/// the two `c_lflag` bits that decide whether a keypress is the kernel's to
/// echo, and the characters `ISIG` turns into signals ([`TtySignals`]), read
/// by [`tty_echo`] at the moment of the press.
///
/// The four corners, measured on this machine's ptys (2026-09-12):
///
/// | `echo` | `canonical` | who is at the slave                          | who echoes  |
/// |--------|-------------|----------------------------------------------|-------------|
/// | true   | true        | a cooked prompt, `sleep`, `cat`              | the kernel  |
/// | false  | true        | `read -s`, `sudo`, `ssh`'s passphrase, `passwd`, `getpass` | NOBODY |
/// | false  | false       | a raw TUI: Claude Code, vim, less — readline / ZLE at rest — bash's `read -s -n` | the program |
/// | true   | false       | `read -n 3`, a cbreak program that leaves ECHO on | the kernel |
///
/// The second row is iTerm2's password-mode rule: canonical mode makes echo
/// the kernel's job, and ECHO clear says the kernel will not do it, so the
/// press will never land on the screen. The third row is deliberately NOT
/// a swallow verdict: a program that took the tty raw draws its own echo,
/// and the readline prompt itself sits in that mode between keys.
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
    /// The press will reach the program but never the screen: canonical
    /// no-echo — iTerm2's password mode. Exactly `!echo && canonical`; a
    /// raw-mode program (`!echo && !canonical`) is `false` here because it
    /// echoes for itself.
    #[must_use]
    pub const fn swallows_input(self) -> bool {
        !self.echo && self.canonical
    }
}

/// The slave's signal characters — `c_cc[VINTR]`, `c_cc[VQUIT]` and
/// `c_cc[VSUSP]` — read while `ISIG` is set. The line discipline turns each of
/// them into a signal for the foreground group AS IT IS WRITTEN: the byte never
/// enters the input queue, and (unless `NOFLSH`) the queue is flushed with it.
///
/// Measured on Darwin 25.6 (2026-09-24, `openpty`, one unread `q` queued, then
/// `^C`, `^\` or `^Z` written through the master): with `ICANON` off and `ISIG`
/// on (cbreak — readline between keys, `watch`, `htop`) FIONREAD falls from 1
/// to 0; in raw mode (`ISIG` off) it rises to 2, the character queued behind
/// the `q` like any byte. A remapped `VINTR` moves the signal with it: with
/// `VINTR = ^G`, `^C` queues and `^G` flushes. `None` is a disabled character
/// (`_POSIX_VDISABLE`: 0xff on macOS, NUL on Linux), which signals nothing.
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

/// WHICH process a session's shell pid named when the session took it — the
/// identity every pid-addressed signal at teardown must re-verify first.
///
/// Why: a shell ADOPTED across a seamless update has launchd as its parent,
/// so nothing here reaps it and its pid is freed the moment it exits. If the
/// pane stays open (`--hold`, a disowned job) and the kernel reissues the pid,
/// a teardown `killpg(pid, SIGHUP/SIGKILL)` would hit an unrelated process
/// group; `getpgid(pid) == pid` does not tell them apart, since every
/// job-control group leader passes it.
///
/// So the session records the identity when it TAKES the shell, while it is
/// provably the shell ([`record_spawned_shell`]: our own unreaped child;
/// [`record_adopted_shell`]: the session leader of the carried PTY), and every
/// signal sent by pid re-reads the kernel and compares ([`ShellIdentity::verify`]).
/// The recorded fact is the process's START TIME (`proc_pidinfo`
/// `PROC_PIDTBSDINFO` `pbi_start_tvsec/usec` on macOS, `/proc/<pid>/stat`
/// field 22 on Linux): a reissued pid has a different one. When nothing can be
/// proven the teardown signals only the terminal's own foreground group
/// ([`hangup_shell`]), never a pid.
///
/// A value, not a capability: the fields are readable and [`Self::from_parts`]
/// builds one from recorded parts, because the check is always against the
/// kernel's CURRENT answer, which no caller can forge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellIdentity {
    /// The shell's pid (== its process-group and session id: `login_tty` ran
    /// `setsid` in the child).
    pub pid: i32,
    /// The process start time recorded when the session took the shell, in an
    /// opaque per-platform unit (microseconds since the epoch on macOS, clock
    /// ticks since boot on Linux). `None` when it could not be recorded — the
    /// platform has no reader, or an adopted shell was not provably the
    /// carried terminal's session leader any more.
    pub birth: Option<u64>,
    /// The shell is THIS process's own child (a fresh spawn, never an
    /// adoption). While we have not reaped it the kernel cannot reissue its
    /// pid, so an unreaped-child answer from `waitid(WNOWAIT)` is itself a
    /// proof — the one that still holds once the shell is a zombie, whose start
    /// time libproc refuses to report.
    pub own_child: bool,
}

impl ShellIdentity {
    /// An identity from its recorded parts. Production records through
    /// [`record_spawned_shell`] / [`record_adopted_shell`]; this exists for a
    /// caller that carries a recorded identity, and for tests that must hand
    /// the teardown a pid whose recorded birth no longer matches.
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

/// What [`hangup_shell`] actually signalled — returned so teardown's choice is
/// observable in tests rather than inferred from a process dying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellHangup {
    /// The recorded shell was verified and its process group got `SIGHUP`
    /// (`docs/DESIGN-pty-keeper-2026-09-26.md` F3).
    Shell(i32),
    /// The shell could not be verified; the terminal's own foreground process
    /// group (`tcgetpgrp(master)` — a group that is on this tty by definition)
    /// got `SIGHUP` instead.
    Foreground(i32),
    /// Neither: nothing was signalled.
    Nothing,
}

/// How a spawned child ended, as far as the platform can actually say.
///
/// Deliberately platform-neutral and deliberately partial: the collector that
/// produces it ([`collect_exit_status`]) never blocks, so "not yet exited" and
/// "not ours to reap" both answer `None` rather than a fabricated code. An
/// ADOPTED session (the seamless-update handoff) is a child of the PREVIOUS
/// aterm process and can never yield a status at all — treat `None` as an
/// honest unknown, never as success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildExit {
    /// Normal termination with this status code.
    Code(i32),
    /// Killed by this signal (Unix only; Windows reports the code the kernel
    /// substitutes).
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

// Test-only: this test process forks outside `InheritStrip::prepare` (the
// fork-safety probe's raw `fork`s, a `setsid` stand-in's `pre_exec`) while
// sibling tests make libnotify calls on other threads — measured, a child lost
// inside `fork()` in 4 of 160 loaded runs. So libnotify's once-gate is passed at
// image load, before the harness starts a thread.
#[cfg(all(test, target_os = "macos"))]
aterm_uds::pass_libnotify_gate_at_load!();

#[cfg(test)]
mod tests {
    use super::*;

    // `build_child_env` (the spawn's `envp`) drops deny-listed keys, keeps
    // ordinary vars, and lets `env_add` override.
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
        // Every deny-listed key (AI-tool + provisioning) is filtered out.
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
        // Ordinary inherited key survives.
        assert!(
            keys.contains(&"PATH".to_string()),
            "PATH must pass through to the child"
        );
        // env_add OVERRIDES the inherited value (TERM was `dumb`, now the injected one),
        // and appears exactly once (no duplicate).
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

    /// A NON-UTF-8 key that carries a deny-listed PREFIX is still denied:
    /// prefixes match a key's leading bytes, so `ANTHROPIC_\xffX` is denied.
    /// The last key is the control: no deny-listed prefix, so it passes.
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

    /// THE IDENTITY SEAM. A session spawned
    /// with `identity=<name>` gets each agent's home variable pointed into the
    /// identity dir through `env_add`, which this builder applies AFTER the
    /// deny pass. For every variable the identity table sets (the primer's
    /// roster read through its `var` column — `CLAUDE_CONFIG_DIR`, `CODEX_HOME`;
    /// spelled here because this crate sits below the primer), the parent's
    /// value is GONE and the identity's is present — and no other `CLAUDE*` /
    /// `CODEX_*` key of the parent's survives beside it. No `is_ai_env_var`
    /// pin is needed for that: an inherited key is REPLACED by `env_add`'s
    /// value whether or not it was denied, so a future row for a variable the
    /// deny tables do not cover (Gemini's) behaves the same.
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

    // Windows env names are case-insensitive: a non-canonical-case copy of a
    // deny-listed var must be filtered too, or it leaks into the child.
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

    // Unix env names ARE case-sensitive: the deny check stays exact-case there
    // (a lowercase `anthropic_api_key` is a DIFFERENT variable, not a leak).
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
