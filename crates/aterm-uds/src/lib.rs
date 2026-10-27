// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-uds: the portable stream type behind aterm's control socket, plus the platform helpers the control channel needs (the `latest` alias, CSPRNG bytes, pid liveness and, on Unix, `SCM_RIGHTS` descriptor passing authenticated by the peer pid).

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

#[cfg(unix)]
pub type CtlStream = std::os::unix::net::UnixStream;
#[cfg(unix)]
pub type CtlListener = std::os::unix::net::UnixListener;

/// Keep a socket out of every child this process forks or spawns (`FD_CLOFORK`).
///
/// On macOS a socket is drained for good when one of its descriptors closes while another
/// process is inside a call on that descriptor (a same-user descriptor scan such as lsof or
/// aterm's job sampler, `aterm_sysprobe::jobs`, or an `fstat`): every later blocking read of
/// the socket answers `EBADF` at once, and a drained listener hands that to every connection it
/// accepts. A spawned child holds a copy of each of its parent's descriptors until its `exec`
/// closes it, even under `POSIX_SPAWN_CLOEXEC_DEFAULT`, so a scan of a child being spawned
/// drains the parent's sockets. Mark each long-lived socket (a listener, what it accepts, a
/// connection kept open) as it is created. Off Apple it does nothing
/// (`astream_sys::close_on_fork`).
///
/// # Errors
/// The `fcntl(2)` errno of reading or setting the descriptor's flags.
#[cfg(unix)]
pub fn keep_out_of_children(socket: &impl std::os::fd::AsFd) -> std::io::Result<()> {
    astream_sys::close_on_fork(socket.as_fd())
}

/// Windows: nothing to do, as a handle reaches a child only when it is made inheritable.
///
/// # Errors
/// None.
#[cfg(windows)]
pub fn keep_out_of_children<T>(_socket: &T) -> std::io::Result<()> {
    Ok(())
}

/// The Windows AF_UNIX-over-winsock implementation of [`CtlStream`] / [`CtlListener`] (afunix.sys; raw `ws2_32` FFI, no external deps).
#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub use win::{CtlListener, CtlStream};

/// Unix-only: the one writer of a whole file (a new temp beside it, its mode on the descriptor, synced, renamed) and aterm.toml's, under the config service's write lock.
#[cfg(unix)]
pub mod atomic_file;

/// This account's own home as the account database has it ([`account::home`]), whatever `$HOME` says: what a launchd job of this login runs with.
#[cfg(unix)]
pub mod account;

/// Test support (test and debug builds, macOS and Linux): the call a thread of this process sleeps in, as the kernel has it ([`asleep::asleep`]), which tells a thread queued for a file lock (`flock(2)`) from one that polls.
#[cfg(all(
    any(test, debug_assertions),
    any(target_os = "macos", target_os = "linux")
))]
pub mod asleep;

/// A deadline on a call that can block forever: in this process ([`deadline::Gate`], the shared [`deadline::TRUST`] gate) or in a child ([`deadline::output_until`]).
#[cfg(not(target_arch = "wasm32"))]
pub mod deadline;
/// Whether this process runs elevated (a Windows administrator's full token), which gives it a control directory of its own ([`control_socket_dir`]).
pub mod elevation;
/// A kernel witness of another process's death with its `wait(2)` status (the PTY keeper's death classifier), and the moment of it ([`exitwatch::ExitMoment`], read by `aterm ctl subscribe`'s redial).
pub mod exitwatch;
pub mod fdpass;
/// Test support (test and debug builds): the `git` a fixture runs, isolated from this machine.
#[cfg(all(any(test, debug_assertions), not(target_arch = "wasm32")))]
pub mod fixture_git;
/// Another program's live SQLite database (agy's summaries, Muse's goals, tccd's `TCC.db`), read into memory with no lock, copy or subprocess ([`foreign_db::read_bytes`]), plus the remote immutable URI spelling ([`foreign_db::immutable_uri`]).
pub mod foreign_db;
/// Whether a connected stream's peer has closed entirely (a lane's hangup check).
pub mod hangup;
/// Which live processes of this uid hold a descriptor for one device (the PTY keeper's holder scan), or use a path under a set of directories (the disk watch's check before it removes a build directory whole).
pub mod holders;
pub mod host;
pub mod latest;
/// Unix-only: the descriptor a throwaway `--headless` instance watches so it ends with the process that started it — both ends.
#[cfg(unix)]
pub mod lifeline;
/// Which listening socket a path names, so a hang-up is judged by the listener that accepted the stream and never by the pid alone.
pub mod listener;
/// Local interface addresses for retry decisions, read without connecting a socket.
#[cfg(unix)]
pub mod network;
/// Unix-only: paths read without following a link anywhere on them (macOS's `O_NOFOLLOW_ANY`): what is at one, an event-only watch descriptor and the kqueue that hears it, and a reader of a regular file alone that never blocks on a FIFO; off macOS, `Unsupported`.
#[cfg(unix)]
pub mod nofollow;
/// Captured subprocess output, optionally delegated to a caller-owned executor.
#[cfg(not(target_arch = "wasm32"))]
pub mod output;
/// Who holds a file's owner lock (the PTY keeper reads a dead window's crash marker with it).
pub mod ownerlock;
/// The kernel's record of a socket peer: its uid and (Darwin) audit token.
pub mod peer;
/// Unix-only: strict private files, the user's own regular file directly in the user's own 0700 directory, with no second link, no symlink and no ACL grant (lifted from aterm-net's `fleet_member`).
#[cfg(unix)]
pub mod private_file;
/// `KERN_PROCARGS2`, a process's exec path, argument words and initial environment: the one reader ([`procargs::read`], macOS) and the one parser ([`procargs::parse`]) every crate reads it through, aterm-sysprobe's (its `kproc` reads a process with it, and this crate depends on that one).
#[cfg(any(unix, windows))]
pub use aterm_sysprobe::procargs;
pub mod process;
/// A kernel witness of a session's `exec`s (card engine §3.3): one kqueue per shell, its forks and the `exec`s and exits of its children and of the terminal's holder.
#[cfg(unix)]
pub mod procwatch;
/// macOS's program check (`syspolicyd`) read from outside: its thread pool, by one bounded setuid `ps` of its pid, for the wedge judge.
#[cfg(not(target_arch = "wasm32"))]
pub mod program_check;
/// `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`: a child that inherits nothing but a `/dev/null` stdio (the PTY keeper's one spawn).
pub mod pspawn;
pub mod rand;
/// Unix-only: one wait on two descriptors with no timeout, ended by whichever is readable, at its end or in error first, reading neither (the standby bridge's wait for its lock or its aterm's word, an [`exitwatch::ExitMoment`]'s for the exit or its cancel).
#[cfg(unix)]
pub mod ready;
/// Unix-only: the process-wide reaper of children nobody else waits for (a dropped `spawnfd` child still running, a PTY shell past its deadline).
#[cfg(unix)]
pub mod reaper;
/// Secret bytes: scrubbed on drop, a redacted `Debug`, no `Clone`; and the process's no-core-dump switch.
pub mod secret;
/// macOS's own services' footprints read without root: each resource coalition of the system's accounts alone, for the service-memory judge.
#[cfg(not(target_arch = "wasm32"))]
pub mod services;
/// Unix-only: whether an account other than this one can write a file or directory (its mode bits, its owner, its access ACL, its filesystem and the user-private-group rule).
#[cfg(unix)]
pub mod shared_write;
/// Unix-only: anonymous POSIX shared memory, the crash-restore journal's region: created read-write with a read-only custody descriptor the keeper holds unmapped, and mapped read-only by the window that restores a dead one's sessions.
#[cfg(unix)]
pub mod shm;
/// Which Windows console a child process gets: a windowless one when this process has none (the window), so no console flashes on screen, and nothing changed when it has one or off Windows.
pub mod spawn;
/// Child spawns that inherit only what they are handed: [`spawnfd::command`], the `Command` every child is built with (on Darwin one `posix_spawn`, no fork); the fabric bridge's spawn with two descriptors at fixed numbers; the inheritance strip for a spawn that must fork (the PTY child's); and [`spawnfd::transparent`], the pass-throughs' spawn, whose child inherits this process's whole table.
#[cfg(unix)]
pub mod spawnfd;
/// Off Unix, [`spawnfd::command`] (there: the Windows console rule of [`spawn`]), [`spawnfd::transparent`], [`spawnfd::InheritNothing`] and std's `Command`, `Stdio` and `Child` under spawnfd's names, so a shared spawn site reads the same on every platform.
#[cfg(not(unix))]
#[path = "spawnfd_nonunix.rs"]
pub mod spawnfd;
/// Pure-Rust, read-only SQLite 3 B-tree and WAL snapshot reader (`#![forbid(unsafe_code)]`), paired with [`foreign_db::read_bytes`].
pub mod sqlite_read;
/// A verb's answer on stdout, where a reader that closed the pipe (`| head -1`) ends the answer instead of panicking the process.
pub mod stdout;
/// The kernel facts a `sudo` askpass relay stands on, and a scrubbed secret for the password in flight (`docs/DESIGN-sudo-askpass-2026-10-15.md`).
pub mod sudo;
/// A Cargo test binary's own per-user root, under which [`control_socket_dir`] and `aterm_types::dirs` answer in a test, so no test reaches the person's live socket, state, logs or config.
pub mod test_binary;
/// A child that ends with the process that started it, killed outright included: Linux's parent-death signal, and on Darwin this executable's [`tether::VERB`], which leaves a watcher behind as it execs the child ([`spawnfd::Tether`]).
pub mod tether;
/// A zone's offset from UTC at an instant, read in this process: the local clock's by
/// `localtime_r`, a named zone's from its TZif file, never `date +%z`. See [`zone`].
pub mod zone;

/// The per-user directory holding aterm's control socket and token, resolved identically for the server and every client.
#[must_use]
pub fn control_socket_dir() -> Option<std::path::PathBuf> {
    control_socket_dir_inner(test_binary::scratch_root())
}

/// [`control_socket_dir`]'s per-user default when `$XDG_RUNTIME_DIR` is unset, as `~/<that>` for a person to read.
#[cfg(target_os = "macos")]
pub const CONTROL_SOCKET_DIR_DEFAULT: &str = "~/Library/Application Support/aterm";
/// [`control_socket_dir`]'s per-user default when `$XDG_RUNTIME_DIR` is unset, as `~/<that>` for a person to read.
#[cfg(not(target_os = "macos"))]
pub const CONTROL_SOCKET_DIR_DEFAULT: &str = "${XDG_STATE_HOME:-~/.local/state}/aterm/run";

/// Whether [`control_socket_dir`] is private: `$XDG_RUNTIME_DIR` is set, so it is not the per-user default under `$HOME`, or this is a test binary's scratch one.
#[must_use]
pub fn control_socket_dir_is_private() -> bool {
    #[cfg(windows)]
    {
        false
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_RUNTIME_DIR").is_some() || test_binary::scratch_root().is_some()
    }
}

/// The Windows control directory's name under the per-user temp folder for an unelevated process.
#[cfg(windows)]
pub const CONTROL_DIR_NAME: &str = "aterm";

/// The Windows control directory's name under the per-user temp folder for an elevated process ([`control_socket_dir`]).
#[cfg(windows)]
pub const ELEVATED_CONTROL_DIR_NAME: &str = "aterm-elevated";

/// [`control_socket_dir`], with `test_root` (a test binary's scratch root,
/// [`test_binary::scratch_root`]) standing in for the per-user default.
fn control_socket_dir_inner(test_root: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let default = windows_control_dir(
        [
            std::env::var_os("TMP"),
            std::env::var_os("TEMP"),
            std::env::var_os("LOCALAPPDATA"),
        ],
        elevation::is_elevated(),
    );
    #[cfg(not(windows))]
    let default = {
        use std::path::PathBuf;
        if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
            match test_root {
                Some(root) if test_binary::is_login_runtime_dir(&xdg) => {
                    return Some(root.join("run"));
                }
                _ => return Some(PathBuf::from(xdg).join("aterm")),
            }
        }
        per_user_default(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
    };
    match (default, test_root) {
        (Some(_), Some(root)) => Some(root.join("run")),
        (default, _) => default,
    }
}

/// [`control_socket_dir`] with `$XDG_RUNTIME_DIR` unset, over `$XDG_STATE_HOME`
/// and `$HOME`.
#[cfg(not(windows))]
fn per_user_default(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if cfg!(any(target_os = "macos", not(unix))) {
        let _ = xdg_state_home;
        return Some(
            PathBuf::from(home?)
                .join("Library")
                .join("Application Support")
                .join("aterm"),
        );
    }
    let state = match xdg_state_home.map(PathBuf::from) {
        Some(root) if root.is_absolute() => root,
        _ => PathBuf::from(home?).join(".local").join("state"),
    };
    Some(state.join("aterm").join("run"))
}

/// [`control_socket_dir`] on Windows over `[%TMP%, %TEMP%, %LOCALAPPDATA%]` and
/// whether this process is elevated: the first non-empty temp variable, else
/// `%LOCALAPPDATA%\Temp`, then [`CONTROL_DIR_NAME`] or, elevated,
/// [`ELEVATED_CONTROL_DIR_NAME`].
#[cfg(windows)]
fn windows_control_dir(
    [tmp, temp, local_app_data]: [Option<std::ffi::OsString>; 3],
    elevated: bool,
) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let non_empty = |v: Option<std::ffi::OsString>| v.filter(|s| !s.is_empty());
    let base = match non_empty(tmp).or_else(|| non_empty(temp)) {
        Some(temp) => PathBuf::from(temp),
        None => PathBuf::from(non_empty(local_app_data)?).join("Temp"),
    };
    Some(base.join(if elevated {
        ELEVATED_CONTROL_DIR_NAME
    } else {
        CONTROL_DIR_NAME
    }))
}

#[cfg(all(test, windows))]
mod windows_control_dir_tests {
    use super::windows_control_dir;
    use std::path::PathBuf;

    /// An elevated process resolves its own directory beside the shared one,
    /// under the same temp base, so the two never share `latest`, a token or a
    /// socket; the base keeps its order (`%TMP%`, `%TEMP%`, `%LOCALAPPDATA%\Temp`).
    #[test]
    fn an_elevated_process_gets_a_control_dir_of_its_own() {
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        let env = [os(r"C:\t"), os(r"C:\u"), os(r"C:\l")];
        assert_eq!(
            windows_control_dir(env.clone(), false),
            Some(PathBuf::from(r"C:\t\aterm"))
        );
        assert_eq!(
            windows_control_dir(env, true),
            Some(PathBuf::from(r"C:\t\aterm-elevated"))
        );
        assert_eq!(
            windows_control_dir([os(""), os(r"C:\u"), None], true),
            Some(PathBuf::from(r"C:\u\aterm-elevated"))
        );
        assert_eq!(
            windows_control_dir([None, None, os(r"C:\l")], false),
            Some(PathBuf::from(r"C:\l\Temp\aterm"))
        );
        assert_eq!(windows_control_dir([None, os(""), None], true), None);
    }
}

#[cfg(all(test, not(windows)))]
mod control_dir_tests {
    use super::per_user_default;
    use std::path::PathBuf;

    /// In a test binary the per-user default is the binary's scratch root's `run`
    /// folder, so a test that names no folder never reaches the person's live
    /// socket; one the environment names (`$XDG_RUNTIME_DIR`, when this run has
    /// one of its own) still wins.
    #[test]
    fn a_test_binary_resolves_a_control_dir_of_its_own() {
        let root = crate::test_binary::scratch_root().expect("a test binary's root");
        let runtime = std::env::var_os("XDG_RUNTIME_DIR");
        let chosen = runtime
            .clone()
            .filter(|dir| !crate::test_binary::is_login_runtime_dir(dir));
        let Some(chosen) = chosen else {
            assert_eq!(super::control_socket_dir(), Some(root.join("run")));
            let person = super::control_socket_dir_inner(None);
            let persons = match runtime {
                Some(login) => Some(PathBuf::from(login).join("aterm")),
                None => {
                    per_user_default(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
                }
            };
            assert_eq!(person, persons);
            assert!(person.is_some_and(|dir| !dir.starts_with(root)));
            return;
        };
        assert_eq!(
            super::control_socket_dir(),
            Some(PathBuf::from(chosen).join("aterm"))
        );
    }

    /// With `$XDG_RUNTIME_DIR` unset the control folder is aterm's state folder
    /// on macOS, and on every other Unix a `run` folder under aterm's XDG state
    /// folder — `$XDG_STATE_HOME` when absolute, else `~/.local/state` — never a
    /// `~/Library` in a Linux home.
    #[test]
    fn the_default_control_dir_follows_each_platforms_state_folder() {
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        let home = os("/home/u");
        if cfg!(target_os = "macos") {
            assert_eq!(
                per_user_default(os("/x/state"), home),
                Some(PathBuf::from("/home/u/Library/Application Support/aterm"))
            );
            return;
        }
        assert_eq!(
            per_user_default(os("/x/state"), home.clone()),
            Some(PathBuf::from("/x/state/aterm/run"))
        );
        for state in [None, os(""), os("relative/state")] {
            assert_eq!(
                per_user_default(state.clone(), home.clone()),
                Some(PathBuf::from("/home/u/.local/state/aterm/run")),
                "XDG_STATE_HOME {state:?} is no state root"
            );
        }
        assert_eq!(per_user_default(None, None), None);
        assert_eq!(
            per_user_default(os("/x/state"), None),
            Some(PathBuf::from("/x/state/aterm/run")),
            "an absolute XDG_STATE_HOME needs no HOME"
        );
    }
}
