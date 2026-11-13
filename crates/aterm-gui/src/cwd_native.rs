// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Converts the engine's RFC 8089 URI-path cwd to a native path.

use std::borrow::Cow;

use aterm_core::terminal::Terminal;

/// The GUI's view of a terminal's shell-reported working directory: the engine value as a native path.
pub(crate) trait ReportedCwd {
    /// The shell-reported cwd as a native path, or `None` if never reported.
    fn native_working_directory(&self) -> Option<Cow<'_, str>>;

    /// Whether this machine made the report behind [`Self::native_working_directory`]
    /// ([`reported_here`]): a shell over ssh reports its own host's directory, which this
    /// machine's files do not hold.
    fn reported_cwd_here(&self) -> bool;
}

impl ReportedCwd for Terminal {
    fn native_working_directory(&self) -> Option<Cow<'_, str>> {
        let path = self.current_working_directory()?;
        Some(native_path_reported(path, || self.reported_cwd_here()))
    }

    fn reported_cwd_here(&self) -> bool {
        reported_here(
            self.reported_cwd_host(),
            crate::tab_label::names_this_machine,
            netbios_name(),
        )
    }
}

/// Whether a report naming `host` (`None`: it named none, or `localhost`)
/// was made on THIS machine: one naming no host was, and so was one naming a
/// host `names` calls this machine's ([`crate::tab_label::names_this_machine`]:
/// its DNS name, in any case) or, on Windows, its NetBIOS name `netbios` —
/// what aterm's PowerShell integration writes in its URI
/// (`[Environment]::MachineName`: the DNS name upper-cased and cut to fifteen
/// characters, so a longer DNS name is not it).
fn reported_here(host: Option<&str>, names: impl Fn(&str) -> bool, netbios: Option<&str>) -> bool {
    host.is_none_or(|host| {
        names(host)
            || netbios
                .map(str::trim)
                .is_some_and(|n| !n.is_empty() && host.trim().eq_ignore_ascii_case(n))
    })
}

/// This machine's NetBIOS name, read from the OS
/// (`aterm_uds::process::netbios_host_name`: the name
/// `[Environment]::MachineName` answers, whatever this process's environment
/// holds — a window started with an environment of its own may have no
/// `%COMPUTERNAME%`), read once: it changes only at a restart.
fn netbios_name() -> Option<&'static str> {
    #[cfg(windows)]
    {
        static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        NAME.get_or_init(aterm_uds::process::netbios_host_name)
            .as_deref()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Convert one RFC 8089 URI path to a native path for the host platform.
#[must_use]
pub(crate) fn native_path(path: &str) -> Cow<'_, str> {
    native_path_reported(path, || true)
}

/// [`native_path`] for a report `here` says whether this machine made (asked only of a drive-letter or an MSYS-shaped path, each rewritten only then; the engine stores the host a report named beside its path, and only the GUI knows its own names, [`reported_here`]).
#[must_use]
pub(crate) fn native_path_reported(path: &str, here: impl FnOnce() -> bool) -> Cow<'_, str> {
    #[cfg(not(windows))]
    let _ = here;
    #[cfg(windows)]
    {
        if let Some(drive) = drive_letter_uri(path) {
            // Another machine's drive is not this one's: left as written.
            if !here() {
                return Cow::Borrowed(path);
            }
            let rest = &path[3..];
            let mut out = String::with_capacity(path.len().max(3));
            out.push(drive);
            out.push(':');
            if rest.is_empty() {
                out.push('\\');
            } else {
                out.extend(rest.chars().map(|c| if c == '/' { '\\' } else { c }));
            }
            return Cow::Owned(out);
        }
        if path.starts_with("//") {
            return Cow::Owned(path.replace('/', "\\"));
        }
        if let Some((drive, rest)) = msys_drive(path).filter(|_| here()) {
            let mut out = String::with_capacity(rest.len() + 3);
            out.push(drive.to_ascii_uppercase());
            out.push(':');
            if rest.is_empty() {
                out.push('\\');
            } else {
                out.extend(rest.chars().map(|c| if c == '/' { '\\' } else { c }));
            }
            return Cow::Owned(out);
        }
    }
    Cow::Borrowed(path)
}

/// The drive letter and the rest (from its separator on, or empty) of an
/// MSYS/Cygwin drive path — `/c/…` or `/cygdrive/c/…` — or `None` for any
/// other string.
#[cfg(windows)]
fn msys_drive(path: &str) -> Option<(char, &str)> {
    let tail = path
        .strip_prefix("/cygdrive")
        .filter(|t| t.starts_with('/'))
        .unwrap_or(path);
    let bytes = tail.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'/' || !bytes[1].is_ascii_alphabetic() {
        return None;
    }
    match bytes.get(2) {
        None | Some(b'/') => Some((bytes[1] as char, &tail[2..])),
        Some(_) => None,
    }
}

/// The drive letter of a `/C:/…`-shaped URI path, or `None` for any other string.
#[cfg(windows)]
fn drive_letter_uri(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'/' || !bytes[1].is_ascii_alphabetic() {
        return None;
    }
    if bytes[2] != b':' && bytes[2] != b'|' {
        return None;
    }
    match bytes.get(3) {
        None | Some(b'/') | Some(b'\\') => Some(bytes[1] as char),
        Some(_) => None,
    }
}

#[cfg(test)]
#[path = "cwd_native_tests.rs"]
mod tests;
