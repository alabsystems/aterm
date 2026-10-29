// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! The single PTY spawn and IO seam.

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

/// A spawned shell.
#[derive(Debug, Clone, Copy)]
pub struct SpawnedShell {
    /// The PTY master fd.
    pub master: i32,
    /// The child's pid, also its process-group id.
    pub pid: i32,
}

/// The variables another terminal emulator sets for ITS children: when aterm is
/// started from a gnome-terminal, kitty, Konsole, … tab, these name that terminal's
/// window, pane or control socket, so in aterm's shell they point at the wrong
/// terminal (`$WINDOWID` tools act on the outer window, `kitty @` drives the outer
/// kitty, Ubuntu's `vte-2.91.sh` runs a VTE prompt hook at every prompt).
const FOREIGN_TERMINAL_VARS: &[&str] = &[
    "WINDOWID",
    "VTE_VERSION",
    "GNOME_TERMINAL_SCREEN",
    "GNOME_TERMINAL_SERVICE",
    "TILIX_ID",
    "TERMINATOR_UUID",
    "TERMINATOR_DBUS_NAME",
    "TERMINATOR_DBUS_PATH",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_LISTEN_ON",
    "KITTY_PUBLIC_KEY",
    "ALACRITTY_WINDOW_ID",
    "ALACRITTY_SOCKET",
    "ALACRITTY_LOG",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_EXECUTABLE_DIR",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "TERM_SESSION_ID",
    "WT_SESSION",
    "WT_PROFILE_ID",
    "COLORFGBG",
];

/// Prefixes of [`FOREIGN_TERMINAL_VARS`]: every `KONSOLE_*` name is Konsole's
/// per-session D-Bus address or profile.
const FOREIGN_TERMINAL_PREFIXES: &[&str] = &["KONSOLE_"];

/// Whether `key` is another terminal's identity variable ([`FOREIGN_TERMINAL_VARS`],
/// [`FOREIGN_TERMINAL_PREFIXES`]).
#[must_use]
fn is_foreign_terminal_key(key: &std::ffi::OsStr) -> bool {
    let key = key.to_string_lossy();
    #[cfg(windows)]
    let key = key.to_ascii_uppercase();
    for name in FOREIGN_TERMINAL_VARS {
        if *name == key {
            return true;
        }
    }
    for prefix in FOREIGN_TERMINAL_PREFIXES {
        if key.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// Whether `key` hands the child a descriptor by number, which no child of aterm's
/// has: `WAYLAND_SOCKET`, the compositor connection a launcher passes down, which
/// libwayland takes as its connection without falling back to `WAYLAND_DISPLAY`.
#[must_use]
fn is_inherited_descriptor_key(key: &std::ffi::OsStr) -> bool {
    key == "WAYLAND_SOCKET"
}

/// Whether environment key `key` is `name`: case-insensitive on Windows only, as the deny list.
#[must_use]
fn env_key_is(key: &std::ffi::OsStr, name: &str) -> bool {
    #[cfg(windows)]
    {
        key.to_string_lossy().eq_ignore_ascii_case(name)
    }
    #[cfg(not(windows))]
    {
        key == name
    }
}

/// The child shell's environment: `inherited` minus every deny-listed key
/// ([`aterm_types::domain::is_ai_env_key`]: AI-tool vars and the recursion-provisioning
/// identity vars; case-insensitive on Windows only), every descriptor handed down by
/// number ([`is_inherited_descriptor_key`]) and an `EDITOR` an outer aterm's tab exported
/// ([`aterm_types::domain::is_handed_down_editor`], which this hop's own default replaces),
/// then `env_add` on top, overriding or appending.
#[cfg_attr(trust_verify, trust::skip)]
fn build_child_env(
    inherited: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    env_add: &[(String, String)],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut claims_terminal = false;
    for (k, _) in env_add {
        if k == "TERM_PROGRAM" {
            claims_terminal = true;
            break;
        }
    }
    let mut env_pairs: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    for (k, v) in inherited {
        if !aterm_types::domain::is_ai_env_key(&k)
            && !is_inherited_descriptor_key(&k)
            && !(claims_terminal && is_foreign_terminal_key(&k))
        {
            env_pairs.push((k, v));
        }
    }
    let aterm_child = env_pairs
        .iter()
        .find(|(k, _)| env_key_is(k, aterm_types::domain::ENV_ATERM_CHILD))
        .map(|(_, v)| v.clone());
    env_pairs.retain(|(k, v)| {
        !(env_key_is(k, "EDITOR")
            && aterm_types::domain::is_handed_down_editor(v, aterm_child.as_deref()))
    });
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

/// The refusal of a spawn whose argument `index` (from 0, as the caller gave `list`) holds a NUL byte.
pub(crate) fn argv_nul_refusal(list: &str, index: usize) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "argument {index} of {list} holds a NUL byte, which no program can be handed: not \
             started, since leaving it out would shift every argument after it"
        ),
    )
}

/// The refusal of a spawn whose program, `what` (the configured shell and where it was set), holds a NUL byte.
pub(crate) fn program_nul_refusal(what: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "{what} holds a NUL byte, which no file name can hold: not started, since running \
             it would start a different program"
        ),
    )
}

/// What the line discipline does with the next byte typed into a PTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TtyEcho {
    /// `ECHO` is set.
    pub echo: bool,
    /// `ICANON` is set.
    pub canonical: bool,
    /// The signal characters while `ISIG` is set.
    pub signals: Option<TtySignals>,
}

impl TtyEcho {
    /// The press reaches the program but never the screen (canonical no-echo, iTerm2's password mode).
    #[inline]
    #[must_use]
    pub const fn swallows_input(self) -> bool {
        !self.echo && self.canonical
    }

    /// Whether the line discipline is in full raw mode (`!ECHO && !ICANON && ISIG` off).
    #[inline]
    #[must_use]
    pub const fn is_raw_mode(self) -> bool {
        !self.echo && !self.canonical && self.signals.is_none()
    }

    /// Whether the line discipline is in standard cooked echo mode (`ECHO && ICANON`).
    #[inline]
    #[must_use]
    pub const fn is_cooked_echo(self) -> bool {
        self.echo && self.canonical
    }

    /// Whether writing `byte` into this line discipline triggers a terminal signal (`ISIG` enabled and `byte` matches `VINTR`, `VQUIT`, or `VSUSP`).
    #[inline]
    #[must_use]
    pub const fn intercepts_signal_byte(self, byte: u8) -> bool {
        match self.signals {
            Some(s) => s.signals(byte),
            None => false,
        }
    }
}

/// The slave's signal characters (`c_cc[VINTR]`, `c_cc[VQUIT]`, `c_cc[VSUSP]`), read while `ISIG` is set.
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
    /// Standard POSIX default control characters (`^C` = `0x03`, `^\` = `0x1c`, `^Z` = `0x1a`).
    #[inline]
    #[must_use]
    pub const fn posix_defaults() -> Self {
        Self {
            intr: Some(0x03),
            quit: Some(0x1c),
            susp: Some(0x1a),
        }
    }

    /// All three signal characters disabled (`_POSIX_VDISABLE`).
    #[inline]
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            intr: None,
            quit: None,
            susp: None,
        }
    }

    /// Whether `byte` matches `VINTR`.
    #[inline]
    #[must_use]
    pub const fn is_intr(self, byte: u8) -> bool {
        matches!(self.intr, Some(b) if b == byte)
    }

    /// Whether `byte` matches `VQUIT`.
    #[inline]
    #[must_use]
    pub const fn is_quit(self, byte: u8) -> bool {
        matches!(self.quit, Some(b) if b == byte)
    }

    /// Whether `byte` matches `VSUSP`.
    #[inline]
    #[must_use]
    pub const fn is_susp(self, byte: u8) -> bool {
        matches!(self.susp, Some(b) if b == byte)
    }

    /// Whether `byte`, written into the PTY, is turned into a signal rather than queued.
    #[inline]
    #[must_use]
    pub const fn signals(self, byte: u8) -> bool {
        self.is_intr(byte) || self.is_quit(byte) || self.is_susp(byte)
    }
}

/// Which process a session's shell pid named when the session took it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellIdentity {
    /// The shell's pid, also its process-group and session id.
    pub pid: i32,
    /// Session start time in platform units (microseconds since epoch on macOS, clock ticks since boot on Linux).
    pub birth: Option<u64>,
    /// The shell is this process's own child (a fresh spawn).
    pub own_child: bool,
}

impl ShellIdentity {
    /// An identity from recorded parts (production records through [`record_spawned_shell`] / [`record_adopted_shell`]).
    #[inline]
    #[must_use]
    pub const fn from_parts(pid: i32, birth: Option<u64>, own_child: bool) -> Self {
        Self {
            pid,
            birth,
            own_child,
        }
    }

    /// Nothing recorded, nothing ever proven.
    #[inline]
    #[must_use]
    pub const fn unproven(pid: i32) -> Self {
        Self::from_parts(pid, None, false)
    }

    /// Whether the recorded pid is a positive OS process identifier (`> 0`).
    #[inline]
    #[must_use]
    pub const fn has_positive_pid(self) -> bool {
        self.pid > 0
    }

    /// Whether a process birth timestamp was recorded when the session took the shell.
    #[inline]
    #[must_use]
    pub const fn has_birth_record(self) -> bool {
        self.birth.is_some()
    }

    /// Pure birth-timestamp comparison against a kernel-reported `(pid, birth)`.
    #[inline]
    #[must_use]
    pub const fn matches_kernel_birth(self, kernel_pid: i32, kernel_birth: u64) -> bool {
        self.pid > 0 && self.pid == kernel_pid && matches!(self.birth, Some(b) if b == kernel_birth)
    }
}

/// What [`hangup_shell`] signalled, so tests observe teardown's choice directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellHangup {
    /// The recorded shell was verified and its process group got `SIGHUP` (`docs/DESIGN-pty-keeper-2026-09-26.md` F3).
    Shell(i32),
    /// The shell could not be verified.
    Foreground(i32),
    /// Neither: nothing was signalled.
    Nothing,
}

impl ShellHangup {
    /// Return the process-group id that received `SIGHUP`, if any.
    #[inline]
    #[must_use]
    pub const fn signalled_pgid(self) -> Option<i32> {
        match self {
            Self::Shell(pgid) | Self::Foreground(pgid) => Some(pgid),
            Self::Nothing => None,
        }
    }

    /// Whether any process group was signalled.
    #[inline]
    #[must_use]
    pub const fn did_signal(self) -> bool {
        !matches!(self, Self::Nothing)
    }
}

/// How a spawned child ended, as far as the platform can say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildExit {
    /// Normal termination with this status code.
    Code(i32),
    /// Killed by this signal (Unix only).
    Signal(u8),
}

impl ChildExit {
    /// Whether the child exited normally with code `0`.
    #[inline]
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Code(0))
    }

    /// Return the normal exit code if the child was not killed by a signal.
    #[inline]
    #[must_use]
    pub const fn exit_code(self) -> Option<i32> {
        match self {
            Self::Code(c) => Some(c),
            Self::Signal(_) => None,
        }
    }

    /// Return the signal number if the child was terminated by a signal.
    #[inline]
    #[must_use]
    pub const fn signal_number(self) -> Option<u8> {
        match self {
            Self::Code(_) => None,
            Self::Signal(s) => Some(s),
        }
    }

    /// Conventional POSIX shell `$?` encoding (`code` for normal exit, `128 + sig` for signals).
    #[inline]
    #[must_use]
    pub const fn shell_status_code(self) -> i32 {
        match self {
            Self::Code(c) => c,
            Self::Signal(s) => 128i32.saturating_add(s as i32),
        }
    }
}

impl SpawnedShell {
    /// Whether both the master descriptor/key (`>= 0`) and child pid (`> 0`) are valid.
    #[inline]
    #[must_use]
    pub const fn has_valid_handles(self) -> bool {
        self.master >= 0 && self.pid > 0
    }
}

/// How long a session's start may take before it is judged hung: the
/// one-minute hang detector, never a latency budget. A start that must happen
/// is waited for, and a start that fails is told at once. The Unix spawn waits
/// this long for its child's exec verdict, and a window holds keys typed into a
/// tab still starting from it.
pub const START_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// Clamp a PTY window dimension (`cols` or `rows`) to at least `1` cell, using `fallback` when `cells == 0`.
#[inline]
#[must_use]
pub const fn clamp_winsize_cells(cells: u16, fallback: u16) -> u16 {
    if cells > 0 {
        cells
    } else if fallback > 0 {
        fallback
    } else {
        1
    }
}

/// Encode a 4-field PTY window size `(cols, rows, xpixel, ypixel)` into 8 little-endian bytes.
#[inline]
#[must_use]
pub const fn encode_winsize_le(cols: u16, rows: u16, xpixel: u16, ypixel: u16) -> [u8; 8] {
    [
        (cols & 0xFF) as u8,
        ((cols >> 8) & 0xFF) as u8,
        (rows & 0xFF) as u8,
        ((rows >> 8) & 0xFF) as u8,
        (xpixel & 0xFF) as u8,
        ((xpixel >> 8) & 0xFF) as u8,
        (ypixel & 0xFF) as u8,
        ((ypixel >> 8) & 0xFF) as u8,
    ]
}

/// Decode 8 little-endian bytes into a 4-field PTY window size `(cols, rows, xpixel, ypixel)`.
#[inline]
#[must_use]
pub const fn decode_winsize_le(bytes: [u8; 8]) -> (u16, u16, u16, u16) {
    let cols = (bytes[0] as u16) | ((bytes[1] as u16) << 8);
    let rows = (bytes[2] as u16) | ((bytes[3] as u16) << 8);
    let xpixel = (bytes[4] as u16) | ((bytes[5] as u16) << 8);
    let ypixel = (bytes[6] as u16) | ((bytes[7] as u16) << 8);
    (cols, rows, xpixel, ypixel)
}

/// Promote the calling thread to `QOS_CLASS_USER_INTERACTIVE` on macOS (no-op elsewhere).
pub fn declare_interactive_thread() {
    #[cfg(target_os = "macos")]
    // SAFETY: sets this thread's own QoS class; takes no pointers and mutates no shared state.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

mod console_host;
pub use console_host::{ConsoleHostChoice, ConsoleHostKind, ConsoleHostReport};
#[cfg(unix)]
pub use console_host::{conpty_host, set_console_host, take_console_host_fallback};
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(unix)]
mod unix_shell;
#[cfg(unix)]
pub use unix_shell::{
    ShellOrigin, fallback_shell, login_shell, select_shell_with_origin, shell_cannot_start,
    shell_env_for_child,
};
#[cfg(any(windows, test))]
mod input_release;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

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

    /// The default-editor hint is per hop (docs/DESIGN-amacs-2026-10-19.md §8.3, W1): an
    /// aterm started from a tab drops the hint it inherited and hands its own, while the
    /// person's `EDITOR` and `VISUAL` pass through.
    /// NEGATIVE CONTROL: with no hint of its own in `env_add`, the child gets none, though one
    /// was inherited.
    #[test]
    fn build_child_env_rederives_the_default_editor_hint_per_hop() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let hint = aterm_types::domain::ENV_DEFAULT_EDITOR;
        let inherited = || {
            vec![
                (os(hint), os("amacs")),
                (os("EDITOR"), os("vi")),
                (os("VISUAL"), os("nano")),
            ]
            .into_iter()
        };
        let value = |out: &[(OsString, OsString)], key: &str| {
            out.iter()
                .filter(|(k, _)| k == &os(key))
                .map(|(_, v)| v.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let own = vec![(hint.to_string(), "amacs".to_string())];
        let out = build_child_env(inherited(), &own);
        assert_eq!(value(&out, hint), ["amacs"], "this hop's own hint, once");
        assert_eq!(value(&out, "EDITOR"), ["vi"]);
        assert_eq!(value(&out, "VISUAL"), ["nano"]);
        let out = build_child_env(inherited(), &[]);
        assert!(value(&out, hint).is_empty(), "an inherited hint is dropped");
        assert_eq!(value(&out, "EDITOR"), ["vi"]);
    }

    /// An `EDITOR=amacs` an outer aterm's tab exported is that aterm's default, not the
    /// person's: an aterm started from such a tab drops it, so its own setting (the opt-out
    /// included) decides again, while a person's `EDITOR` passes through.
    /// NEGATIVE CONTROL: the same `EDITOR=amacs` with no `ATERM_CHILD` (an aterm started from
    /// the Dock or another terminal) reaches the child, as do `EDITOR=vi` under `ATERM_CHILD`
    /// and an `EDITOR` this hop sets itself.
    #[test]
    fn build_child_env_drops_an_editor_an_outer_aterm_tab_exported() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let editor = |out: &[(OsString, OsString)]| {
            out.iter()
                .filter(|(k, _)| k == "EDITOR")
                .map(|(_, v)| v.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let from_tab =
            |value: &str| vec![(os("ATERM_CHILD"), os("1")), (os("EDITOR"), os(value))].into_iter();
        let out = build_child_env(from_tab("amacs"), &[]);
        assert!(editor(&out).is_empty(), "{out:?}");
        assert!(out.iter().any(|(k, _)| k == "ATERM_CHILD"), "{out:?}");
        assert_eq!(editor(&build_child_env(from_tab("vi"), &[])), ["vi"]);
        let own = vec![("EDITOR".to_string(), "amacs".to_string())];
        assert_eq!(editor(&build_child_env(from_tab("amacs"), &own)), ["amacs"]);
        let outside = vec![(os("EDITOR"), os("amacs"))].into_iter();
        assert_eq!(editor(&build_child_env(outside, &[])), ["amacs"]);
    }

    /// aterm launched from another terminal's tab: that terminal's window, pane and
    /// socket names do not reach aterm's shell, aterm's own identity does, and a
    /// multiplexer's or a user's configuration variable is left alone.
    #[test]
    fn build_child_env_drops_another_terminals_identity() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let inherited = vec![
            (os("PATH"), os("/usr/bin")),
            (os("WINDOWID"), os("4242")),
            (os("VTE_VERSION"), os("7600")),
            (
                os("GNOME_TERMINAL_SCREEN"),
                os("/org/gnome/Terminal/screen/x"),
            ),
            (os("GNOME_TERMINAL_SERVICE"), os(":1.99")),
            (os("KITTY_WINDOW_ID"), os("3")),
            (os("KONSOLE_DBUS_SERVICE"), os(":1.7")),
            (os("TERM_PROGRAM"), os("WezTerm")),
            (os("TMUX"), os("/tmp/tmux-1000/default,9,0")),
            (os("KITTY_CONFIG_DIRECTORY"), os("/home/u/kitty")),
        ];
        let env_add = vec![("TERM_PROGRAM".to_string(), "aterm".to_string())];
        let out = build_child_env(inherited.into_iter(), &env_add);
        let pairs: Vec<(String, String)> = out
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(
            pairs,
            vec![
                pair("PATH", "/usr/bin"),
                pair("TERM_PROGRAM", "aterm"),
                pair("TMUX", "/tmp/tmux-1000/default,9,0"),
                pair("KITTY_CONFIG_DIRECTORY", "/home/u/kitty"),
            ]
        );
    }

    /// aterm launched with its compositor connection handed down (`WAYLAND_SOCKET`): no
    /// shell gets the variable, with or without the terminal claimed, since the
    /// descriptor it names is closed in every child, and a Wayland client run from the
    /// shell would take that number as its connection; `WAYLAND_DISPLAY` reaches it.
    #[test]
    fn build_child_env_drops_a_compositor_connection_handed_down_by_number() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        for env_add in [
            vec![("TERM_PROGRAM".to_string(), "aterm".to_string())],
            Vec::new(),
        ] {
            let inherited = vec![
                (os("WAYLAND_SOCKET"), os("5")),
                (os("WAYLAND_DISPLAY"), os("wayland-0")),
                (os("PATH"), os("/usr/bin")),
            ];
            let keys: Vec<String> = build_child_env(inherited.into_iter(), &env_add)
                .into_iter()
                .map(|(k, _)| k.to_string_lossy().into_owned())
                .filter(|k| k != "TERM_PROGRAM")
                .collect();
            assert_eq!(keys, ["WAYLAND_DISPLAY", "PATH"], "env_add {env_add:?}");
        }
    }

    /// The passthrough session of an `aterm` typed at a prompt: its `env_add` holds the
    /// session PATH and no `TERM_PROGRAM`, so the outer terminal stays the shell's
    /// terminal and its identity variables reach the shell unchanged.
    #[test]
    fn build_child_env_keeps_the_outer_terminals_identity_without_a_claim() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let inherited = vec![
            (os("WINDOWID"), os("4242")),
            (os("VTE_VERSION"), os("7600")),
            (os("KITTY_WINDOW_ID"), os("3")),
            (os("KONSOLE_DBUS_SERVICE"), os(":1.7")),
            (os("TERM_PROGRAM"), os("kitty")),
            (os("PATH"), os("/usr/bin")),
        ];
        let env_add = vec![("PATH".to_string(), "/opt/aterm/bin:/usr/bin".to_string())];
        let out = build_child_env(inherited.into_iter(), &env_add);
        let pairs: Vec<(String, String)> = out
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(
            pairs,
            vec![
                pair("WINDOWID", "4242"),
                pair("VTE_VERSION", "7600"),
                pair("KITTY_WINDOW_ID", "3"),
                pair("KONSOLE_DBUS_SERVICE", ":1.7"),
                pair("TERM_PROGRAM", "kitty"),
                pair("PATH", "/opt/aterm/bin:/usr/bin"),
            ]
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

    #[cfg(windows)]
    #[test]
    fn build_child_env_denies_mixed_case_keys_on_windows() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let inherited = vec![
            (os("Path"), os("C:\\Windows")),
            (os("anthropic_api_key"), os("secret")),
            (os("Claude_Code"), os("1")),
            (os("Aterm_Update_Token"), os("secret")),
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

    #[cfg(unix)]
    #[test]
    fn build_child_env_stays_case_sensitive_on_unix() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let inherited = vec![(os("anthropic_api_key"), os("distinct-var"))];
        let out = build_child_env(inherited.into_iter(), &[]);
        assert_eq!(out.len(), 1, "exact-case deny must not match on unix");
    }

    #[test]
    fn pty_echo_signals_identity_and_exit_const_helpers() {
        let sigs = TtySignals::posix_defaults();
        assert!(sigs.is_intr(0x03) && sigs.is_quit(0x1c) && sigs.is_susp(0x1a));
        assert!(sigs.signals(0x03) && !sigs.signals(b'a'));
        assert!(!TtySignals::disabled().signals(0x03));

        let cooked = TtyEcho {
            echo: true,
            canonical: true,
            signals: Some(sigs),
        };
        assert!(cooked.is_cooked_echo() && !cooked.is_raw_mode() && !cooked.swallows_input());
        assert!(cooked.intercepts_signal_byte(0x03));

        let raw = TtyEcho {
            echo: false,
            canonical: false,
            signals: None,
        };
        assert!(raw.is_raw_mode() && !raw.intercepts_signal_byte(0x03));

        let id = ShellIdentity::from_parts(42, Some(1000), true);
        assert!(id.has_positive_pid() && id.has_birth_record());
        assert!(id.matches_kernel_birth(42, 1000));
        assert!(!id.matches_kernel_birth(42, 999));
        assert!(!ShellIdentity::unproven(-1).has_positive_pid());

        assert_eq!(ShellHangup::Shell(42).signalled_pgid(), Some(42));
        assert!(ShellHangup::Foreground(9).did_signal());
        assert!(!ShellHangup::Nothing.did_signal());

        assert!(ChildExit::Code(0).is_success());
        assert_eq!(ChildExit::Code(2).exit_code(), Some(2));
        assert_eq!(ChildExit::Signal(9).signal_number(), Some(9));
        assert_eq!(ChildExit::Signal(9).shell_status_code(), 137);
        assert!(
            SpawnedShell {
                master: 3,
                pid: 100
            }
            .has_valid_handles()
        );
        assert_eq!(clamp_winsize_cells(0, 80), 80);
        assert_eq!(clamp_winsize_cells(0, 0), 1);
        let ws = encode_winsize_le(80, 24, 640, 384);
        assert_eq!(decode_winsize_le(ws), (80, 24, 640, 384));
    }
}
