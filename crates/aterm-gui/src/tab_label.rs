// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Strips local `user@host:` prefixes and default console executable paths from OSC tab titles.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long [`local_host_names`] holds this machine's names before it reads
const NAMES_FOR: Duration = Duration::from_secs(5);

/// This machine's host name as configured and its short form.
type Names = (String, String);

/// This machine's names: the hostname as configured, plus its short form
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

/// This machine's host name: the one shared answer
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

/// The kernel's name for this machine now (`gethostname(3)`), or `None`:
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

/// Every name this process has read the kernel give this machine.
static MACHINE_NAMES: std::sync::Mutex<MachineNames> =
    std::sync::Mutex::new(MachineNames { seen: Vec::new() });

/// Bounded ring of hostnames observed for this machine across network changes.
#[derive(Debug, Default)]
pub(crate) struct MachineNames {
    seen: Vec<String>,
}

impl MachineNames {
    /// Names kept, at most.
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

/// Whether `host` is the machine named `name`, read whole: the two names alike
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

/// [`informative`] with Git Bash's home (its POSIX spelling, `/c/Users//me`)
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
    pub(crate) fn is_empty(self) -> bool {
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

/// Git Bash's home in its own POSIX spelling (`/c/Users//me`), read once: the
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

/// A home directory in MSYS's POSIX spelling: `C:\Users\me` reads
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

/// The part of `path` past Git Bash's `home` (`""` for home itself, else from
fn msys_home_suffix<'p>(path: &'p str, home: &str) -> Option<&'p str> {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return None;
    }
    let head = path.get(..home.len())?;
    let rest = &path[home.len()..];
    (head.eq_ignore_ascii_case(home) && (rest.is_empty() || rest.starts_with('/'))).then_some(rest)
}

/// The Windows profile folder (`%USERPROFILE%`, `C:\Users\me`), read once, its
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

/// The part of a Windows title past the profile folder `home` (`C:\Users\me`)
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

/// The MSYS2 environments whose name Git for Windows' bash writes before the
const MSYS_TITLE_PREFIXES: [&str; 7] = [
    "MINGW64:",
    "MINGW32:",
    "UCRT64:",
    "CLANG64:",
    "CLANG32:",
    "CLANGARM64:",
    "MSYS:",
];

/// The path of Git Bash's title, its environment prefix removed:
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

/// [`without_local_identity`] with this machine's names handed in, so a test chooses
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

/// Whether `host` names this machine, comparing the short forms too because a
fn host_is_local(host: &str, local_full: &str, local_short: &str) -> bool {
    if local_full.is_empty() && local_short.is_empty() {
        return false;
    }
    let host_short = host.split_once('.').map_or(host, |(label, _)| label);
    let eq = |a: &str, b: &str| !a.is_empty() && a.eq_ignore_ascii_case(b);
    eq(host, local_full) || eq(host, local_short) || eq(host_short, local_short)
}

#[cfg(test)]
mod tests {
    use super::{
        host_is_local, informative, informative_under, msys_home_suffix, msys_spelling,
        windows_home_suffix, without_console_program_path, without_identity_of,
    };

    const FULL: &str = "m17-tower.local";
    const SHORT: &str = "m17-tower";

    /// [`informative_under`] with no Git Bash home and no profile folder, as a
    fn shed(title: &str) -> String {
        informative_under(title, None, None).to_string()
    }

    /// A WINDOWS TITLE UNDER THE PROFILE FOLDER READS `~`, as the pwsh prompt's
    #[test]
    fn windows_titles_read_the_profile_folder_as_a_tilde() {
        let home = Some(r"C:\Users\m6-an");
        let label = |title: &str| informative_under(title, None, home).to_string();
        for (title, want) in [
            (r"C:\Users\m6-an\aterm", r"~\aterm"),
            (r"C:\Users\m6-an", "~"),
            (r"c:\users\M6-AN\src\x", r"~\src\x"),
            (r"C:\Users\m6-an/src", "~/src"),
            (
                r"C:\Users\m6-an\aterm - ping -n 4 127.0.0.1",
                r"~\aterm - ping -n 4 127.0.0.1",
            ),
            (r"C:\Users\m6-an - ping x", "~ - ping x"),
            (r"C:\Users\m6-an2\src", r"C:\Users\m6-an2\src"),
            (r"C:\Users\m6-an.old", r"C:\Users\m6-an.old"),
            (r"D:\work", r"D:\work"),
            (r"C:\Windows", r"C:\Windows"),
            (r"~\aterm", r"~\aterm"),
            (r"vim C:\Users\m6-an\x.rs", r"vim C:\Users\m6-an\x.rs"),
            (r"C:\Windows\system32\cmd.exe", ""),
            ("MINGW64:/c/Users//m6-an/src", "/c/Users//m6-an/src"),
        ] {
            assert_eq!(label(title), want, "{title:?}");
        }
        assert!(!informative_under(r"C:\Users\m6-an", None, home).is_empty());
        assert_eq!(
            informative_under(r"C:\Users\m6-an\x", None, Some(r"C:\Users\m6-an\")).to_string(),
            r"~\x",
            "a profile folder spelled with its separator"
        );
        assert_eq!(
            informative_under("MINGW64:/c/Users//m6-an/src", Some("/c/Users//m6-an"), home)
                .to_string(),
            "~/src",
            "Git Bash keeps its own rule"
        );
        assert_eq!(
            windows_home_suffix(r"C:\x", "C:"),
            None,
            "a drive is no home"
        );
        assert_eq!(windows_home_suffix(r"C:\x", r"C:\"), None, "nor its root");
        assert_eq!(
            shed(r"C:\Users\m6-an\aterm"),
            r"C:\Users\m6-an\aterm",
            "CONTROL"
        );
    }

    /// Git Bash's `MINGW64:` (and its sibling environments') prefix is dropped
    #[test]
    fn git_bash_titles_lose_their_environment_prefix() {
        assert_eq!(
            shed("MINGW64:/c/Users//m6-an/src/aterm"),
            "/c/Users//m6-an/src/aterm"
        );
        assert_eq!(shed("UCRT64:/tmp"), "/tmp");
        assert_eq!(shed("MSYS:/home/me"), "/home/me");
        assert_eq!(shed("MINGW64: not a path"), "MINGW64: not a path");
        assert_eq!(shed("vim MINGW64:/x"), "vim MINGW64:/x");
        assert_eq!(
            shed("MINGW64:/c/Users//m6-an/ünï 日本"),
            "/c/Users//m6-an/ünï 日本"
        );
    }

    /// GIT BASH'S HOME READS `~` in the tab, as its own prompt writes it and as
    #[test]
    fn git_bash_titles_read_home_as_a_tilde() {
        let home = Some("/c/Users//m6-an");
        let label = |title: &str| informative_under(title, home, None).to_string();
        assert_eq!(
            label("MINGW64:/c/Users//m6-an/aterm-winperfect"),
            "~/aterm-winperfect"
        );
        assert_eq!(label("MINGW64:/c/Users//m6-an"), "~");
        assert_eq!(label("MINGW64:/C/users/M6-AN/src"), "~/src");
        assert_eq!(label("UCRT64:/c/Users//m6-an/ünï 日本"), "~/ünï 日本");
        assert_eq!(label("MINGW64:/c/Users//m6-an2/src"), "/c/Users//m6-an2/src");
        assert_eq!(label("MINGW64:/d/work"), "/d/work");
        assert_eq!(
            label("/c/Users//m6-an/notes"),
            "/c/Users//m6-an/notes",
            "not Git Bash's title"
        );
        assert!(!informative_under("MINGW64:/c/Users//m6-an", home, None).is_empty());
        assert_eq!(
            shed("MINGW64:/c/Users//m6-an/aterm-winperfect"),
            "/c/Users//m6-an/aterm-winperfect",
            "CONTROL"
        );
        let mut slot = String::from("stale");
        slot.clear();
        informative_under("MINGW64:/c/Users//m6-an/src", home, None).push_to(&mut slot);
        assert_eq!(slot, "~/src");
    }

    /// The home Git Bash adopts, in its own spelling: a Windows `HOME` or
    #[test]
    fn a_home_is_spelled_as_git_bash_spells_it() {
        assert_eq!(
            msys_spelling("C:\\Users\\m6-an").as_deref(),
            Some("/c/Users//m6-an")
        );
        assert_eq!(msys_spelling("D:/home/me/").as_deref(), Some("/d/home/me"));
        assert_eq!(msys_spelling("/home/me").as_deref(), Some("/home/me"));
        assert_eq!(msys_spelling("C:").as_deref(), Some("/c"));
        assert_eq!(msys_spelling("\\\\server\\share\\me"), None);
        assert_eq!(msys_spelling("relative\\me"), None);
        assert_eq!(msys_home_suffix("/c/users/x/y", "/C/Users//X/"), Some("/y"));
        assert_eq!(msys_home_suffix("/c/Users//xy", "/c/Users//x"), None);
        assert_eq!(msys_home_suffix("/c", "/c/Users//x"), None);
    }

    /// The network renames a Mac with no `HostName` set while aterm runs: a
    #[test]
    fn every_name_the_kernel_gave_this_machine_stays_its_own() {
        let mut names = super::MachineNames::default();
        assert!(!names.names("m19Macb128GB2TB"), "nothing read yet");
        names.note(None);
        names.note(Some(String::new()));
        assert!(!names.names(""), "an unreadable name names nothing");
        names.note(Some("m19Macb128GB2TB.localdomain".into()));
        names.note(Some("m19-Macbook-M5-Max-128GB-2TB.local".into()));
        for host in [
            "m19Macb128GB2TB.localdomain",
            "m19Macb128GB2TB",
            "M19MACB128GB2TB",
            "m19-Macbook-M5-Max-128GB-2TB.local",
            "m19-Macbook-M5-Max-128GB-2TB",
        ] {
            assert!(names.names(host), "{host}");
        }
        assert!(!names.names("build-box.example"), "never this machine's");
        assert!(!names.names("m19"), "a prefix is not the name");
        for i in 0..super::MachineNames::KEPT {
            names.note(Some(format!("renamed-{i}.local")));
        }
        assert!(!names.names("m19Macb128GB2TB"), "the oldest name is gone");
        assert!(names.names("renamed-0"), "the kept ones stay");
    }

    /// A citation's host is this machine only by its WHOLE name, in any case,
    #[test]
    fn a_citation_host_is_this_machine_only_by_its_whole_name() {
        for (host, name) in [
            ("m19Macb128GB2TB", "m19Macb128GB2TB.localdomain"),
            ("m19Macb128GB2TB.local", "m19Macb128GB2TB.localdomain"),
            ("M19MACB128GB2TB.LOCAL", "m19Macb128GB2TB"),
            ("m19Macb128GB2TB.localdomain.", "m19Macb128GB2TB"),
            ("10.0.0.5", "10.0.0.5"),
            ("build-box.example", "build-box.example"),
        ] {
            assert!(super::names_machine(host, name), "{host} is {name}");
        }
        for (host, name) in [
            ("m19Macb128GB2TB.other.example", "m19Macb128GB2TB.local"),
            ("10.0.0.7", "10.0.0.5"),
            ("m19", "m19Macb128GB2TB.local"),
            ("build-box", "build-box.example"),
            ("", "m19Macb128GB2TB.local"),
            ("anything", ""),
            ("", ""),
        ] {
            assert!(!super::names_machine(host, name), "{host} is not {name}");
        }
        let mut names = super::MachineNames::default();
        names.note(Some("m17-tower.local".into()));
        assert!(names.names("m17-tower"));
        assert!(
            !names.names("m17-tower.corp.example"),
            "the first label alone"
        );
    }

    /// A seamless update's predecessor's names are kept as OLDER than the
    #[test]
    fn the_names_a_predecessor_read_are_kept_as_older() {
        let mut names = super::MachineNames::default();
        names.note(Some("new-name.local".into()));
        names.adopt_older([
            "old-name.localdomain".to_string(),
            "NEW-NAME.local".to_string(),
        ]);
        assert_eq!(names.all().len(), 2, "once each: {:?}", names.all());
        assert_eq!(names.all()[0], "old-name.localdomain", "the older first");
        assert!(names.names("old-name"), "the predecessor's name");
        assert!(!names.names("other-box"), "never read by either");
        names.adopt_older((0..super::MachineNames::KEPT).map(|i| format!("gone-{i}")));
        assert_eq!(names.all().len(), super::MachineNames::KEPT);
        assert!(names.names("new-name"), "the newest stay");
        assert!(!names.names("gone-0"), "the oldest went first");
    }

    fn strip(title: &str) -> &str {
        without_identity_of(title, FULL, SHORT)
    }

    /// Only this machine's own `user@host:` prefix is shed: the stock prompt
    #[test]
    fn only_this_machines_prompt_identity_is_shed() {
        for (title, want) in [
            ("user@m17-tower: ~/aterm", "~/aterm"),
            ("user@m17-tower.local: ~/aterm", "~/aterm"),
            ("user@m17-tower:~/aterm", "~/aterm"),
            ("user@m17-tower", ""),
            ("user@m17-tower:", ""),
            ("  user@m17-tower:   ", ""),
            ("root@prod-db-01: /var/log", "root@prod-db-01: /var/log"),
            ("user@m17-tower-2: ~/aterm", "user@m17-tower-2: ~/aterm"),
            ("vim README.md", "vim README.md"),
            ("~/aterm", "~/aterm"),
            ("", ""),
            (
                "sent to user@m17-tower: ok",
                "sent to user@m17-tower: ok",
            ),
            ("@m17-tower: ~/aterm", "@m17-tower: ~/aterm"),
            ("user@: ~/aterm", "user@: ~/aterm"),
        ] {
            assert_eq!(strip(title), want, "{title:?}");
        }
    }

    #[test]
    fn an_unknown_local_name_matches_nothing() {
        assert_eq!(
            without_identity_of("user@m17-tower: ~/aterm", "", ""),
            "user@m17-tower: ~/aterm"
        );
        assert!(!host_is_local("m17-tower", "", ""));
    }

    #[test]
    fn case_and_domain_spellings_of_this_machine_all_match() {
        assert!(host_is_local("M17-Tower", FULL, SHORT));
        assert!(host_is_local("m17-tower.lan", FULL, SHORT));
        assert!(!host_is_local("m17-towerx", FULL, SHORT));
    }

    /// ConPTY's title for a fresh console, the program's own path, leaves `""`: the
    #[test]
    fn only_a_bare_console_program_path_is_shed() {
        for path in [
            "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\\pwsh.exe",
            "C:\\Windows\\system32\\cmd.exe",
            "c:/windows/system32/CMD.EXE",
            "\\\\build-01\\tools\\shell.exe",
            "  C:\\Windows\\system32\\cmd.exe  ",
            "C:\\Windows\\system32\\cmd.exe - ",
        ] {
            assert_eq!(without_console_program_path(path), "", "{path:?}");
            assert!(informative(path).is_empty(), "{path:?}");
        }
        for (title, want) in [
            (
                "C:\\Windows\\system32\\cmd.exe - ping  build-01",
                "ping  build-01",
            ),
            (
                "C:\\Windows\\system32\\cmd.exe - C:\\tools\\run.exe",
                "C:\\tools\\run.exe",
            ),
            ("~\\aterm", "~\\aterm"),
            ("/usr/bin/bash", "/usr/bin/bash"),
            ("pwsh.exe", "pwsh.exe"),
            ("bin\\tool.exe", "bin\\tool.exe"),
            (
                "C:\\apps\\foo.exe.bak\\notes.txt",
                "C:\\apps\\foo.exe.bak\\notes.txt",
            ),
            ("C:\\Users\\x\\aterm", "C:\\Users\\x\\aterm"),
            (
                "running C:\\tools\\run.exe now",
                "running C:\\tools\\run.exe now",
            ),
            ("C:\\tools\\run.exe.", "C:\\tools\\run.exe."),
            ("C:\\tools\\run.exe -v", "C:\\tools\\run.exe -v"),
            ("C:", "C:"),
            ("", ""),
            ("user@m17-tower: ~/aterm", "user@m17-tower: ~/aterm"),
        ] {
            assert_eq!(without_console_program_path(title), want, "{title:?}");
        }
    }

    /// Windows names this machine from the OS, with no `hostname.exe` child, in a form
    #[cfg(windows)]
    #[test]
    fn windows_names_this_machine_without_a_child() {
        let name = super::resolve_hostname().expect("Windows names this machine");
        assert!(
            !name.is_empty() && !name.contains(char::is_whitespace),
            "{name:?}"
        );
        let short = name
            .split_once('.')
            .map_or(name.as_str(), |(label, _)| label);
        let prompt = format!("me@{}: ~", name.to_ascii_uppercase());
        assert_eq!(without_identity_of(&prompt, &name, short), "~");
    }
}
