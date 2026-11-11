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

#[cfg(all(unix, any(test, debug_assertions)))]
pub mod test_budget;

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
        if env_key_is(std::ffi::OsStr::new(k), "TERM_PROGRAM") {
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
        let val = std::ffi::OsString::from(v);
        if let Some(first) = env_pairs.iter().position(|(ek, _)| env_key_is(ek, k)) {
            env_pairs[first] = (key, val);
            let mut idx = first + 1;
            while idx < env_pairs.len() {
                if env_key_is(&env_pairs[idx].0, k) {
                    env_pairs.remove(idx);
                } else {
                    idx += 1;
                }
            }
        } else {
            env_pairs.push((key, val));
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
mod cmdline_units;
#[cfg(any(windows, test))]
mod input_release;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(all(test, target_os = "macos"))]
aterm_uds::pass_libnotify_gate_at_load!();

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
