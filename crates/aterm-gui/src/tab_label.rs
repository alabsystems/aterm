// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Strips local `user@host:` prefixes and default console executable paths from OSC tab titles.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

const NAMES_FOR: Duration = Duration::from_secs(5);

type Names = (String, String);

fn local_host_names() -> Arc<Names> {
    static NAMES: Mutex<Option<(Instant, Arc<Names>)>> = Mutex::new(None);
    let mut held = NAMES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((read, names)) = held.as_ref()
        && read.elapsed() < NAMES_FOR
    {
        return Arc::clone(names);
    }
    let full = resolve_hostname().unwrap_or_default();
    let short = full
        .split_once('.')
        .map_or(full.as_str(), |(label, _)| label)
        .to_owned();
    let names = Arc::new((full, short));
    *held = Some((Instant::now(), Arc::clone(&names)));
    names
}

fn resolve_hostname() -> Option<String> {
    aterm_uds::process::host_name()
}

/// Strips a leading `user@host:` prefix from `title` when `host` names this machine.
#[must_use]
pub(crate) fn without_local_identity(title: &str) -> &str {
    let names = local_host_names();
    without_identity_of(title, &names.0, &names.1)
}

/// Whether `host` matches one of this machine's recorded hostnames or `localhost`.
#[must_use]
pub(crate) fn names_this_machine(host: &str) -> bool {
    let names = local_host_names();
    names_machine(host, &names.0) || {
        let mut names = MACHINE_NAMES.lock().unwrap_or_else(|p| p.into_inner());
        names.note(kernel_host_name());
        names.names(host)
    }
}

/// Records the kernel's current hostname into the process-wide machine-name ring.
pub(crate) fn note_this_machine_name() {
    MACHINE_NAMES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .note(kernel_host_name());
}

fn kernel_host_name() -> Option<String> {
    #[cfg(unix)]
    {
        aterm_uds::process::kernel_host_name()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

static MACHINE_NAMES: std::sync::Mutex<MachineNames> =
    std::sync::Mutex::new(MachineNames { seen: Vec::new() });

/// Bounded ring of hostnames observed for this machine across network changes.
#[derive(Debug, Default)]
pub(crate) struct MachineNames {
    seen: Vec<String>,
}

impl MachineNames {
    const KEPT: usize = 16;

    /// Records `name` into the ring if non-empty and not already present.
    pub(crate) fn note(&mut self, name: Option<String>) {
        let Some(name) = name.filter(|name| !name.is_empty()) else {
            return;
        };
        if self
            .seen
            .iter()
            .any(|seen| seen.eq_ignore_ascii_case(&name))
        {
            return;
        }
        if self.seen.len() >= Self::KEPT {
            self.seen.remove(0);
        }
        self.seen.push(name);
    }

    /// Whether `host` matches any recorded name in the ring.
    pub(crate) fn names(&self, host: &str) -> bool {
        self.seen.iter().any(|name| names_machine(host, name))
    }

    /// Every name kept, oldest first.
    #[cfg(any(unix, test))]
    fn all(&self) -> &[String] {
        &self.seen
    }

    /// Prepends `older` hostnames carried from a seamless-update predecessor.
    pub(crate) fn adopt_older(&mut self, older: impl IntoIterator<Item = String>) {
        let mut merged: Vec<String> = Vec::new();
        for name in older.into_iter().chain(std::mem::take(&mut self.seen)) {
            if !name.is_empty() && !merged.iter().any(|kept| kept.eq_ignore_ascii_case(&name)) {
                merged.push(name);
            }
        }
        let excess = merged.len().saturating_sub(Self::KEPT);
        merged.drain(..excess);
        self.seen = merged;
    }
}

fn names_machine(host: &str, name: &str) -> bool {
    fn bare(name: &str) -> &str {
        let name = name.strip_suffix('.').unwrap_or(name);
        let lower = |suffix: &str| {
            name.len() > suffix.len()
                && name.is_char_boundary(name.len() - suffix.len())
                && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        };
        [".localdomain", ".local"]
            .into_iter()
            .find(|suffix| lower(suffix))
            .map_or(name, |suffix| &name[..name.len() - suffix.len()])
    }
    let (host, name) = (bare(host.trim()), bare(name.trim()));
    !name.is_empty() && host.eq_ignore_ascii_case(name)
}

/// Returns recorded machine hostnames for seamless-update handoff.
#[cfg(any(unix, test))]
pub(crate) fn machine_names_for_handoff() -> Vec<String> {
    let mut names = MACHINE_NAMES.lock().unwrap_or_else(|p| p.into_inner());
    names.note(kernel_host_name());
    names.all().to_vec()
}

/// Adopts machine hostnames carried from a seamless-update predecessor.
pub(crate) fn adopt_machine_names(carried: &[String]) {
    MACHINE_NAMES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .adopt_older(
            carried
                .iter()
                .map(|name| name.trim())
                .filter(|name| {
                    name.len() <= 255
                        && !name.contains(|c: char| c.is_whitespace() || c.is_control())
                })
                .map(String::from),
        );
}

/// Returns the initial kernel hostname observed by this process.
#[cfg(all(test, unix))]
pub(crate) fn this_machine_name() -> &'static str {
    Box::leak(local_host_names().0.clone().into_boxed_str())
}

/// Strips local identity and default console executable paths from `title`.
#[must_use]
pub(crate) fn informative(title: &str) -> Informative<'_> {
    informative_under(title, msys_home(), windows_home())
}

/// [`informative`] with Git Bash's home (its POSIX spelling, `/c/Users//me`) and the Windows profile
/// folder (`C:\Users\me`) handed in.
#[must_use]
fn informative_under<'a>(
    title: &'a str,
    msys_home: Option<&str>,
    windows_home: Option<&str>,
) -> Informative<'a> {
    let shed = without_console_program_path(without_local_identity(title));
    let Some(path) = msys_title_path(shed) else {
        return match windows_home.and_then(|home| windows_home_suffix(shed, home)) {
            Some(rest) => Informative { home: true, rest },
            None => Informative {
                home: false,
                rest: shed,
            },
        };
    };
    match msys_home.and_then(|home| msys_home_suffix(path, home)) {
        Some(rest) => Informative { home: true, rest },
        None => Informative {
            home: false,
            rest: path,
        },
    }
}

/// Informative portion of an OSC title after stripping redundant local prefixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Informative<'a> {
    /// The title began with Git Bash's home, which reads `~`.
    home: bool,
    /// The rest, verbatim.
    rest: &'a str,
}

impl Informative<'_> {
    /// Whether no informative title text remains.
    #[must_use]
    pub(crate) const fn is_empty(self) -> bool {
        !self.home && self.rest.is_empty()
    }

    /// Appends the formatted title into `slot`.
    pub(crate) fn push_to(self, slot: &mut String) {
        if self.home {
            slot.push('~');
        }
        slot.push_str(self.rest);
    }
}

impl std::fmt::Display for Informative<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.home {
            f.write_str("~")?;
        }
        f.write_str(self.rest)
    }
}

fn msys_home() -> Option<&'static str> {
    #[cfg(windows)]
    {
        static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        HOME.get_or_init(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(std::path::PathBuf::from)
                .or_else(aterm_types::dirs::home_dir)
                .and_then(|home| msys_spelling(&home.to_string_lossy()))
        })
        .as_deref()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// A home directory in MSYS's POSIX spelling.
#[cfg(any(windows, test))]
fn msys_spelling(home: &str) -> Option<String> {
    let home = home.trim();
    if home.starts_with('/') {
        let kept = home.trim_end_matches('/');
        return (!kept.is_empty()).then(|| kept.to_owned());
    }
    let bytes = home.as_bytes();
    let drive = bytes.first().filter(|b| b.is_ascii_alphabetic())?;
    if bytes.get(1) != Some(&b':') || !matches!(bytes.get(2), None | Some(b'\\' | b'/')) {
        return None;
    }
    let rest = home[2..].replace('\\', "/");
    Some(format!(
        "/{}{}",
        char::from(drive.to_ascii_lowercase()),
        rest.trim_end_matches('/')
    ))
}

fn msys_home_suffix<'p>(path: &'p str, home: &str) -> Option<&'p str> {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return None;
    }
    let head = path.get(..home.len())?;
    let rest = &path[home.len()..];
    (head.eq_ignore_ascii_case(home) && (rest.is_empty() || rest.starts_with('/'))).then_some(rest)
}

fn windows_home() -> Option<&'static str> {
    #[cfg(windows)]
    {
        static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        HOME.get_or_init(|| {
            aterm_types::dirs::home_dir().and_then(|home| {
                let home = home.to_string_lossy();
                let home = home.trim().trim_end_matches(['\\', '/']);
                let bytes = home.as_bytes();
                (bytes.len() > 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && matches!(bytes[2], b'\\' | b'/'))
                .then(|| home.to_owned())
            })
        })
        .as_deref()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn windows_home_suffix<'t>(title: &'t str, home: &str) -> Option<&'t str> {
    let home = home.trim_end_matches(['\\', '/']);
    if home.len() <= "C:".len() {
        return None;
    }
    let head = title.get(..home.len())?;
    let rest = &title[home.len()..];
    (head.eq_ignore_ascii_case(home)
        && (rest.is_empty() || rest.starts_with(['\\', '/']) || rest.starts_with(" - ")))
    .then_some(rest)
}

const MSYS_TITLE_PREFIXES: [&str; 7] = [
    "MINGW64:",
    "MINGW32:",
    "UCRT64:",
    "CLANG64:",
    "CLANG32:",
    "CLANGARM64:",
    "MSYS:",
];

fn msys_title_path(title: &str) -> Option<&str> {
    MSYS_TITLE_PREFIXES
        .iter()
        .find_map(|prefix| title.strip_prefix(prefix))
        .filter(|rest| rest.starts_with('/'))
}

/// Strips a bare Windows console executable path from `title`.
#[must_use]
pub(crate) fn without_console_program_path(title: &str) -> &str {
    let trimmed = title.trim();
    let bytes = trimmed.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    if !drive && !trimmed.starts_with("\\\\") {
        return title;
    }
    for (at, _) in trimmed.match_indices('.') {
        let Some(ext) = trimmed.get(at..at + 4) else {
            continue;
        };
        if !ext.eq_ignore_ascii_case(".exe") {
            continue;
        }
        let rest = &trimmed[at + 4..];
        if rest.is_empty() {
            return "";
        }
        if let Some(command) = rest.strip_prefix(" -")
            && (command.is_empty() || command.starts_with(' '))
        {
            return command.trim();
        }
    }
    title
}

/// [`without_local_identity`] with this machine's names handed in.
#[must_use]
fn without_identity_of<'a>(title: &'a str, local_full: &str, local_short: &str) -> &'a str {
    let trimmed = title.trim();
    let (head, rest) = match trimmed.split_once(':') {
        Some((head, rest)) => (head, rest.trim_start()),
        None => (trimmed, ""),
    };
    let Some((user, host)) = head.split_once('@') else {
        return title;
    };
    if user.is_empty()
        || host.is_empty()
        || head.contains(char::is_whitespace)
        || !host_is_local(host, local_full, local_short)
    {
        return title;
    }
    rest
}

fn host_is_local(host: &str, local_full: &str, local_short: &str) -> bool {
    if local_full.is_empty() && local_short.is_empty() {
        return false;
    }
    let host_short = host.split_once('.').map_or(host, |(label, _)| label);
    let eq = |a: &str, b: &str| !a.is_empty() && a.eq_ignore_ascii_case(b);
    eq(host, local_full) || eq(host, local_short) || eq(host_short, local_short)
}

#[cfg(test)]
#[path = "tab_label_tests.rs"]
mod tests;
