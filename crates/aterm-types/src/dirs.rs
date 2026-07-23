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
        std::env::var_os("HOME")
            .map(PathBuf::from)
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

    #[test]
    fn test_config_dir_returns_some() {
        assert!(config_dir().is_some());
    }

    #[test]
    fn test_data_dir_returns_some() {
        assert!(data_dir().is_some());
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
