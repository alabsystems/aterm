// Copyright 2026 Andrew Yates, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Platform directory resolution (zero external dependencies).
//!
//! Replaces the `dirs` crate with direct environment variable lookups
//! and platform-specific conventions.

use std::path::PathBuf;

/// Return the user's home directory.
///
/// - **Unix/macOS**: `$HOME`, falling back to `/etc/passwd` lookup
/// - **Windows**: `%USERPROFILE%`
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        // An EMPTY or RELATIVE `$HOME` is not a home: `PathBuf::from("")` made every
        // caller's prefix relative to the cwd (the aterm front door laid
        // `<cwd>/Library/Application Support/aterm/pkg/agents` under it, 2026-09-18),
        // so such a value falls through to the passwd lookup like an unset one.
        home_from_env(std::env::var_os("HOME").as_deref())
            .or_else(|| passwd_home_dir(current_uid()))
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    // wasm and other targets have no OS home dir.
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid() is always safe — no failure mode, no args.
    unsafe { libc_getuid() }
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// `$HOME` as a home, or `None`: an EMPTY or RELATIVE value is no home at all.
/// `PathBuf::from("")` used to make every caller's prefix relative to the cwd (the
/// aterm front door laid `<cwd>/Library/Application Support/aterm/pkg/agents` under
/// it, 2026-09-18), so such a value falls through to the passwd lookup like an
/// unset one. Pure, so the rule is testable without touching the process env.
#[cfg(unix)]
fn home_from_env(value: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|home| home.is_absolute())
}

/// Parse `/etc/passwd` to find the home directory for a given UID.
///
/// Byte-wise (`fs::read`, not `read_to_string`): `/etc/passwd` is Unix
/// boundary data, so it stays byte-exact end to end — the `:`-delimited
/// format needs no UTF-8 decode (the uid field is ASCII digits and the home
/// field goes straight into a `PathBuf`, which is bytes on Unix). On the
/// UTF-8 files every real system has, this walks the exact same fields to
/// the exact same result as the old string parse; it merely never takes the
/// strict-UTF-8 reject the hardened Trust gate refutes (`read_to_string`
/// failing the whole lookup over a stray legacy byte in, say, another
/// user's GECOS field).
#[cfg(unix)]
fn passwd_home_dir(uid: u32) -> Option<PathBuf> {
    let contents = std::fs::read("/etc/passwd").ok()?;
    home_from_passwd(&contents, uid)
}

/// Testable helper: extract home dir for `uid` from passwd-format bytes.
#[cfg(unix)]
// Skip: the passwd line split walks std iterators (absent bodies); every malformed line is skipped.
#[cfg_attr(trust_verify, trust::skip)]
fn home_from_passwd(contents: &[u8], uid: u32) -> Option<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let uid_str = uid.to_string();
    // `split(b'\n')` + trailing-`\r` strip is `str::lines` on bytes; the one
    // difference — a trailing empty piece after a final newline — parses to a
    // single empty field and is skipped by the `>= 6` guard, like any other
    // malformed line.
    for line in contents.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        // `splitn(7, ..)`: passwd lines have exactly 7 `:`-fields, and only
        // fields 2 (uid) and 5 (home) are read — `splitn` yields the first 6
        // splits identically to `split` and can only differ in field 6 (the
        // shell, never read here), so the lookup is unchanged while the
        // `collect` gets a literal element bound for the Trust gate.
        let fields: Vec<&[u8]> = line.splitn(7, |&b| b == b':').collect();
        // Total `get`s (not indexing): the `len() >= 6` guard proves the bounds
        // but does not reach the index in the verifier's model; the None arms
        // are unreachable and simply skip the line, as a short line would.
        let (Some(&uid_field), Some(&home)) = (fields.get(2), fields.get(5)) else {
            continue;
        };
        if uid_field == uid_str.as_bytes() && !home.is_empty() {
            // Byte-exact `OsStr` bridge: on Unix `PathBuf::from(str)` and
            // `PathBuf::from(OsStr::from_bytes(..))` build the identical
            // path from the identical bytes.
            return Some(PathBuf::from(OsStr::from_bytes(home)));
        }
    }
    None
}

/// Return the user's configuration directory.
///
/// - **macOS**: `$HOME/Library/Application Support`
/// - **Linux**: `$XDG_CONFIG_HOME` or `$HOME/.config`
/// - **Windows**: `%APPDATA%`
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|h| h.join("Library/Application Support"))
    }
    #[cfg(target_os = "linux")]
    {
        xdg_dir("XDG_CONFIG_HOME").or_else(|| home_dir().map(|h| h.join(".config")))
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    // wasm and other targets have no OS config dir.
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

/// Return the user's CACHE directory — re-derivable bytes only, safe to
/// delete at any time (the OS may: macOS purges Caches under disk pressure,
/// which is exactly the contract callers must survive).
///
/// - **macOS**: `$HOME/Library/Caches`
/// - **Linux**: `$XDG_CACHE_HOME` or `$HOME/.cache`
/// - **Windows**: `%LOCALAPPDATA%` (Windows has no separate cache root; callers
///   should nest under an app-named `cache` subdirectory)
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|h| h.join("Library/Caches"))
    }
    #[cfg(target_os = "linux")]
    {
        xdg_dir("XDG_CACHE_HOME").or_else(|| home_dir().map(|h| h.join(".cache")))
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    // wasm and other targets have no OS cache dir.
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

/// Return the user's data directory.
///
/// - **macOS**: `$HOME/Library/Application Support`
/// - **Linux**: `$XDG_DATA_HOME` or `$HOME/.local/share`
/// - **Windows**: `%LOCALAPPDATA%`
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|h| h.join("Library/Application Support"))
    }
    #[cfg(target_os = "linux")]
    {
        xdg_dir("XDG_DATA_HOME").or_else(|| home_dir().map(|h| h.join(".local/share")))
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    // wasm and other targets have no OS data dir.
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

/// aterm's own STATE root — where it keeps what it owns across launches (the
/// operator profiles, the document journal, the session identities). ONE rule,
/// the one `aterm_agent::operator::default_state_root` and the GUI's document
/// journal already follow, so every state reader agrees on the directory:
///
/// - `$ATERM_STATE_HOME` when set (absolute; a relative value is refused as
///   `None`, never joined onto an arbitrary cwd) — the knob a headless or
///   test instance uses to keep its state in a temp root;
/// - **macOS**: `$HOME/Library/Application Support/aterm`;
/// - **Linux/other Unix**: `$XDG_STATE_HOME/aterm` (absolute) or
///   `$HOME/.local/state/aterm`;
/// - **Windows**: `%LOCALAPPDATA%\aterm`.
///
/// `None` only when nothing resolves (no override and no home).
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    resolve_state_dir(
        std::env::var_os("ATERM_STATE_HOME").map(PathBuf::from),
        StatePlatform {
            home: home_dir(),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        },
    )
}

/// The platform inputs [`resolve_state_dir`] reads, captured so the rule is
/// testable without mutating the process environment.
#[derive(Debug, Default, Clone)]
pub struct StatePlatform {
    /// `$HOME` (`%USERPROFILE%`), as [`home_dir`] gives it.
    pub home: Option<PathBuf>,
    /// `$XDG_STATE_HOME`, consulted only on non-macOS Unix.
    pub xdg_state_home: Option<PathBuf>,
    /// `%LOCALAPPDATA%`, consulted only on Windows.
    pub local_app_data: Option<PathBuf>,
}

/// The pure half of [`state_dir`]: `override_root` is `$ATERM_STATE_HOME`.
#[must_use]
pub fn resolve_state_dir(
    override_root: Option<PathBuf>,
    platform: StatePlatform,
) -> Option<PathBuf> {
    if let Some(root) = override_root {
        // Relative is refused, not resolved: the operator crate errors on it,
        // and a state root that depends on the cwd is no root at all.
        return root.is_absolute().then_some(root);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (&platform.xdg_state_home, &platform.local_app_data);
        platform
            .home
            .map(|home| home.join("Library/Application Support/aterm"))
    }
    #[cfg(windows)]
    {
        let _ = (&platform.home, &platform.xdg_state_home);
        platform
            .local_app_data
            .filter(|root| root.is_absolute())
            .map(|root| root.join("aterm"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = &platform.local_app_data;
        if let Some(root) = platform.xdg_state_home.filter(|root| root.is_absolute()) {
            return Some(root.join("aterm"));
        }
        platform.home.map(|home| home.join(".local/state/aterm"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = platform;
        None
    }
}

/// aterm's per-user LOG directory — ONE rule for every process that writes a log there
/// (the window's and the session's `aterm.log`, and atpkg's `packages.log`, which every
/// package lane appends to), so the files a person is told to look at sit side by side:
///
/// - **macOS**: `$HOME/Library/Logs/aterm` (Console.app's convention);
/// - **Linux/other Unix**: `$XDG_STATE_HOME/aterm/logs` (absolute) or
///   `$HOME/.local/state/aterm/logs` — a Linux home has no business growing a `~/Library`;
/// - **Windows**: `%LOCALAPPDATA%\aterm\logs`, falling back through the home dir's
///   `AppData\Local`.
///
/// Resolution only: the caller creates the directory with the posture it needs (the
/// window's logger makes it `0700`). `None` only when nothing resolves.
#[must_use]
pub fn logs_dir() -> Option<PathBuf> {
    resolve_logs_dir(StatePlatform {
        home: home_dir(),
        xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
    })
}

/// The pure half of [`logs_dir`].
#[must_use]
pub fn resolve_logs_dir(platform: StatePlatform) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let _ = (&platform.xdg_state_home, &platform.local_app_data);
        platform.home.map(|home| home.join("Library/Logs/aterm"))
    }
    #[cfg(windows)]
    {
        let _ = &platform.xdg_state_home;
        platform
            .local_app_data
            .filter(|root| root.is_absolute())
            .or_else(|| platform.home.map(|h| h.join("AppData").join("Local")))
            .map(|root| root.join("aterm").join("logs"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = &platform.local_app_data;
        platform
            .xdg_state_home
            .filter(|root| root.is_absolute())
            .or_else(|| platform.home.map(|home| home.join(".local/state")))
            .map(|root| root.join("aterm/logs"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = platform;
        None
    }
}

/// Where session identities live: `<state_dir>/identities/<name>/` — one
/// directory per name, each holding the agents' relocated config dirs
/// (`.claude/`, `.codex/`, …; see `aterm_primer::agent_homes`).
#[must_use]
pub fn identities_dir() -> Option<PathBuf> {
    state_dir().map(|state| state.join("identities"))
}

/// Read an XDG env var, returning `None` if unset or not an absolute path.
#[cfg(target_os = "linux")]
fn xdg_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_home_dir_returns_some() {
        // HOME should be set in any reasonable test environment
        assert!(home_dir().is_some());
    }

    /// An empty or relative `$HOME` is not a home (2026-09-18): it fell through to
    /// a cwd-relative prefix before, and the front door laid `agents/` under it.
    #[cfg(unix)]
    #[test]
    fn an_empty_or_relative_home_is_no_home() {
        use std::ffi::OsStr;
        assert_eq!(home_from_env(None), None);
        assert_eq!(home_from_env(Some(OsStr::new(""))), None);
        assert_eq!(home_from_env(Some(OsStr::new("Library"))), None);
        assert_eq!(home_from_env(Some(OsStr::new("./home"))), None);
        assert_eq!(
            home_from_env(Some(OsStr::new("/Users//someone"))),
            Some(PathBuf::from("/Users//someone"))
        );
    }

    #[test]
    fn test_config_dir_returns_some() {
        assert!(config_dir().is_some());
    }

    #[test]
    fn test_data_dir_returns_some() {
        assert!(data_dir().is_some());
    }

    /// THE STATE ROOT RULE (session identities, 2026-09-17), pure: the
    /// override wins when absolute and is refused when relative; without it the
    /// platform default applies; and `identities_dir` is one segment below.
    #[test]
    fn state_dir_honours_an_absolute_override_and_refuses_a_relative_one() {
        let platform = StatePlatform {
            home: Some(PathBuf::from("/Users//who")),
            xdg_state_home: Some(PathBuf::from("/xdg/state")),
            local_app_data: Some(PathBuf::from(r"C:\Users\who\AppData\Local")),
        };
        assert_eq!(
            resolve_state_dir(Some(PathBuf::from("/tmp/aterm-state")), platform.clone()),
            Some(PathBuf::from("/tmp/aterm-state")),
            "the override is the root itself, no `aterm` appended"
        );
        assert_eq!(
            resolve_state_dir(Some(PathBuf::from("relative/state")), platform.clone()),
            None,
            "a relative override is refused, never joined onto the cwd"
        );
        let default = resolve_state_dir(None, platform.clone()).expect("a default resolves");
        #[cfg(target_os = "macos")]
        assert_eq!(
            default,
            PathBuf::from("/Users//who/Library/Application Support/aterm")
        );
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(default, PathBuf::from("/xdg/state/aterm"));
        #[cfg(windows)]
        assert_eq!(default, PathBuf::from(r"C:\Users\who\AppData\Local\aterm"));
        assert!(default.is_absolute());
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(
            resolve_state_dir(
                None,
                StatePlatform {
                    xdg_state_home: Some(PathBuf::from("relative")),
                    ..platform.clone()
                }
            ),
            Some(PathBuf::from("/Users//who/.local/state/aterm")),
            "a relative XDG_STATE_HOME falls through to the home default"
        );
        assert_eq!(
            resolve_state_dir(None, StatePlatform::default()),
            None,
            "nothing to resolve from is None, not a panic"
        );
    }

    /// THE LOG DIRECTORY RULE (packages.log beside aterm.log, 2026-09-23), pure: macOS
    /// keeps Console.app's `~/Library/Logs/aterm`, other Unix the XDG state dir's
    /// `aterm/logs` (a relative XDG value falls through to the home default), and nothing
    /// to resolve from is `None`.
    #[test]
    fn logs_dir_follows_each_platforms_convention() {
        let platform = StatePlatform {
            home: Some(PathBuf::from("/Users//who")),
            xdg_state_home: Some(PathBuf::from("/xdg/state")),
            local_app_data: Some(PathBuf::from("C:\\Users\\who\\AppData\\Local")),
        };
        #[cfg(target_os = "macos")]
        assert_eq!(
            resolve_logs_dir(platform.clone()),
            Some(PathBuf::from("/Users//who/Library/Logs/aterm"))
        );
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert_eq!(
                resolve_logs_dir(platform.clone()),
                Some(PathBuf::from("/xdg/state/aterm/logs"))
            );
            assert_eq!(
                resolve_logs_dir(StatePlatform {
                    xdg_state_home: Some(PathBuf::from("relative")),
                    ..platform.clone()
                }),
                Some(PathBuf::from("/Users//who/.local/state/aterm/logs"))
            );
        }
        let _ = &platform;
        assert_eq!(resolve_logs_dir(StatePlatform::default()), None);
    }

    #[test]
    fn identities_dir_is_one_segment_below_the_state_root() {
        if let (Some(state), Some(identities)) = (state_dir(), identities_dir()) {
            assert_eq!(identities, state.join("identities"));
            assert_eq!(
                identities.file_name().and_then(|n| n.to_str()),
                Some("identities")
            );
        }
    }

    #[test]
    fn test_home_dir_is_absolute() {
        if let Some(home) = home_dir() {
            assert!(home.is_absolute(), "home_dir should be absolute: {home:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_passwd_parsing_finds_uid() {
        let passwd = b"root:x:0:0:root:/root:/bin/bash\n\
                      nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin\n\
                      testuser:x:1000:1000:Test User:/home/testuser:/bin/zsh\n";
        assert_eq!(home_from_passwd(passwd, 0), Some(PathBuf::from("/root")));
        assert_eq!(
            home_from_passwd(passwd, 1000),
            Some(PathBuf::from("/home/testuser"))
        );
        assert_eq!(home_from_passwd(passwd, 9999), None);
    }

    #[cfg(unix)]
    #[test]
    fn test_passwd_parsing_empty_home_returns_none() {
        let passwd = ["broken:", "x", ":500:500:Broken User::/bin/sh\n"].concat();
        assert_eq!(home_from_passwd(passwd.as_bytes(), 500), None);
    }

    #[cfg(unix)]
    #[test]
    fn test_passwd_parsing_malformed_lines_skipped() {
        let passwd = b"short:x\n\
                      valid:x:42:42:User:/home/valid:/bin/sh\n\
                      \n";
        assert_eq!(
            home_from_passwd(passwd, 42),
            Some(PathBuf::from("/home/valid"))
        );
    }
}
