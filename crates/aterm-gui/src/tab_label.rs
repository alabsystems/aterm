// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Drop the parts of an OSC title that name only what the user already knows.
//!
//! The default prompt on Debian, Ubuntu, Fedora and macOS titles the terminal with a
//! spelling of `user@host: cwd` (bash's `\u@\h: \w`), so every local tab opens with the
//! same twenty-odd characters and [`crate::tab_bar`]'s distinctness pass has nothing to
//! tell apart. `user@host:` is removed only when `host` names this machine; an ssh
//! session into `prod-db-01` keeps saying so.
//!
//! On Windows, ConPTY reports a fresh console's title as the full path of its program
//! (`C:\Program Files\…\pwsh.exe`), and a shell that sets no title (stock `cmd.exe`,
//! `pwsh` without a prompt hook) keeps it for life. That path is dropped too
//! ([`without_console_program_path`]) and the cwd rung labels the tab; the running
//! command `cmd.exe` appends (`C:\…\cmd.exe - ping x`) is kept.
//!
//! Measured on this machine at 0.61.0: two tabs in `~/aterm` both labelled
//! `user@host: ~/aterm · Ready`. Byte-identical, so [`crate::tab_bar`]'s
//! distinctness pass correctly concluded there was nothing to tell apart and fell
//! back to painting the bare tab ordinals `1` and `2`. With seven tabs open the
//! whole strip was numbers. The strip was not wrong — its input carried no
//! information, because the only distinguishing rung (the directory) sat past the
//! width the shared prefix had already spent.
//!
//! ## What is dropped, and what is never dropped
//!
//! `user@host:` is removed only when `host` names THIS machine, where the user
//! gains nothing from being told where they are. A host that does NOT match is
//! the case where the prefix is the most valuable text in the tab — an ssh
//! session into `prod-db-01` must keep saying so — and it is left untouched.
//!
//! ## The second shape: the console's own program path (Windows)
//!
//! ConPTY reports the console title to the terminal as OSC 0/2, and a fresh
//! console's title is the FULL PATH of the program it was created for —
//! measured on this machine at 0.90.0: a new pane's first title was
//! `C:\Program Files\WindowsApps\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\pwsh.exe`,
//! which the strip painted whole and `ls` printed percent-encoded, until the
//! prompt's own title (`~\aterm`) replaced it a moment later. A shell that sets
//! no title (stock `cmd.exe`, `pwsh` without a prompt hook) keeps that path for
//! its whole life. The path names only the program the user just launched, so
//! it is dropped the same way the local identity is
//! ([`without_console_program_path`]) and the cwd rung labels the tab.
//! `cmd.exe` appends the running command to its title (`C:\…\cmd.exe - ping x`);
//! that suffix IS information and is kept.
//!
//! ## The third shape: the profile folder spelled out (Windows)
//!
//! aterm's cmd.exe integration titles a tab with the whole directory (`$P`,
//! `C:\Users\you\aterm`), as cmd's prompt has no string operations, where the
//! pwsh prompt's title reads the profile folder as `~` (`~\aterm`) and Git
//! Bash's reads its home as `~` (`~/aterm`). A title that starts with the
//! profile folder reads it as `~` in the tab, so a cmd tab beside a pwsh tab
//! in the same folder says the same thing ([`windows_home_suffix`]).
//!
//! The title is display-only chrome. The terminal's OSC 0/2 title remains
//! authoritative identity and is not rewritten: `ctl title` and `meta` still
//! report exactly what the program set (the roster's `ls` title drops only
//! the console program path, keeping `user@host:` — see
//! `control_session::roster_title`). A title a person or a program
//! actually chose (`vim README.md`, a TUI's own name) matches none of the
//! shapes below and survives in full.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long [`local_host_names`] holds this machine's names before it reads
/// them again: a rename reaches the tab titles within it, and the per-frame
/// refill pays one `gethostname(2)` that often, never per frame.
const NAMES_FOR: Duration = Duration::from_secs(5);

/// This machine's host name as configured and its short form.
type Names = (String, String);

/// This machine's names: the hostname as configured, plus its short form
/// (the label before the first dot, which is what bash's `\h` writes), read
/// again once they are [`NAMES_FOR`] old — so a machine renamed while aterm
/// runs (System Settings › Sharing, `scutil --set`, a DHCP lease's name)
/// stops being matched by its old name within that, never at a relaunch.
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
/// ([`aterm_uds::process::host_name`]) — `$HOSTNAME`, else the OS's own
/// (`gethostname(2)`, the name a shell's `\h` and `\H` print), with no spawn
/// on any platform. The name its LAN reaches it by, which the survey's
/// `instance host=` and a fleet issuer's book carry, is
/// [`aterm_uds::process::lan_host_name`]: no prompt prints that one.
fn resolve_hostname() -> Option<String> {
    aterm_uds::process::host_name()
}

/// The title with a `user@host:` prefix naming this machine removed.
///
/// Returns the input unchanged when the title is not of that shape or names a different
/// host. `""` means the title said nothing but the local identity: the caller falls
/// through to the working directory ([`crate::app_tabs::resolved_terminal_title_rung`]).
#[must_use]
pub(crate) fn without_local_identity(title: &str) -> &str {
    let names = local_host_names();
    without_identity_of(title, &names.0, &names.1)
}

/// Whether `host` names THIS machine: the question a file citation asks of the
/// host a session's OSC 7 named (`crate::path_link`), where a remote shell's
/// directory must never resolve against this machine's disk. Asked of the
/// WHOLE name ([`names_machine`]: the same name in any case, with or without a
/// `.local` / `.localdomain` suffix), never of its first label alone as the
/// strip's looser rule reads a prompt ([`host_is_local`]): `<this-name>.corp
/// .example` and `10.0.0.7` beside a machine named `10.0.0.5` are other
/// machines. Any of the machine's names counts: the one the strip uses (a
/// `$HOSTNAME` override first) and, on unix, every name the kernel has given
/// it while this process or the processes it was updated from looked
/// ([`MachineNames`]: `gethostname(3)`, what zsh's `$HOST` and bash's
/// `$HOSTNAME` hold, so what a local shell reports; carried across a seamless
/// update, [`machine_names_for_handoff`]).
///
/// The kernel's name is READ AGAIN on every ask, never kept from the first:
/// on a Mac with no `HostName` set the network names the machine
/// (`m19Macb128GB2TB.localdomain` on one network, `<name>.local` on another), so a
/// shell started after a move reports a name the first read never saw. The
/// names read before are kept too, as a shell started before the move still
/// reports its old one; each session's spawn notes the name its shell starts
/// under ([`note_this_machine_name`]). A machine whose own names cannot be
/// read calls no host its own (fail closed).
#[must_use]
pub(crate) fn names_this_machine(host: &str) -> bool {
    let names = local_host_names();
    names_machine(host, &names.0) || {
        let mut names = MACHINE_NAMES.lock().unwrap_or_else(|p| p.into_inner());
        names.note(kernel_host_name());
        names.names(host)
    }
}

/// Note the kernel's name for this machine as it is now: a session's spawn
/// calls it, so the name its shell captures at start is one
/// [`names_this_machine`] knows however the network renames the machine
/// later.
pub(crate) fn note_this_machine_name() {
    MACHINE_NAMES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .note(kernel_host_name());
}

/// The kernel's name for this machine now (`gethostname(3)`), or `None`:
/// never `$HOSTNAME`, which [`resolve_hostname`] reads first, as a local
/// shell reports the kernel's name whatever this process's environment says.
/// The read is the one shared answer's ([`aterm_uds::process::kernel_host_name`]).
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

/// The kernel names a machine has had while this process looked, oldest
/// first, at most [`MachineNames::KEPT`] (a machine renamed more often than
/// that forgets its oldest, which then names no shell: fail closed).
#[derive(Debug, Default)]
pub(crate) struct MachineNames {
    seen: Vec<String>,
}

impl MachineNames {
    /// Names kept, at most.
    const KEPT: usize = 16;

    /// Keep `name` (the kernel's now), once.
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

    /// Whether `host` is one of the names kept, by [`names_machine`].
    pub(crate) fn names(&self, host: &str) -> bool {
        self.seen.iter().any(|name| names_machine(host, name))
    }

    /// Every name kept, oldest first.
    #[cfg(any(unix, test))]
    fn all(&self) -> &[String] {
        &self.seen
    }

    /// Keep `older` (a predecessor's names, oldest first) BEFORE the names
    /// already kept, once each; past [`Self::KEPT`] the oldest go.
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
/// (ASCII case aside) once a trailing `.` and one `.local` or `.localdomain`
/// suffix are taken off each, the forms a Mac's own name comes in (`m19.local`,
/// `m19.localdomain`, `m19`). Nothing names a machine whose name is empty.
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

/// The names this process has read the kernel give this machine, for a seamless
/// update's manifest (`SessionHandoff::machine_names`): a shell carried across it
/// still reports the name it started under, which its successor never read.
#[cfg(any(unix, test))]
pub(crate) fn machine_names_for_handoff() -> Vec<String> {
    let mut names = MACHINE_NAMES.lock().unwrap_or_else(|p| p.into_inner());
    names.note(kernel_host_name());
    names.all().to_vec()
}

/// The names a seamless update's predecessor had read ([`machine_names_for_handoff`]),
/// kept beside this process's own: the carried shells report them. At most
/// [`MachineNames::KEPT`] are kept in all (the oldest go first), and a value that is
/// not a host name (empty, spaced, too long) is dropped.
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

/// This machine's own name as [`names_this_machine`] reads it (`""` when it
/// cannot be read), for a test that must spell a local OSC 7 (its one caller
/// is unix-only). The names are read again every [`NAMES_FOR`], so the one
/// read now is leaked to outlive the test that asked (tests only).
#[cfg(all(test, unix))]
pub(crate) fn this_machine_name() -> &'static str {
    Box::leak(local_host_names().0.clone().into_boxed_str())
}

/// The title with everything that names only what the user already knows
/// removed: a `user@host:` prefix naming this machine
/// ([`without_local_identity`]), the console's own program path
/// ([`without_console_program_path`]), Git Bash's environment prefix
/// ([`msys_title_path`]) and, after that prefix, Git Bash's home, which
/// reads `~` as its own prompt writes it; on Windows, the user's profile
/// folder at the head of any other title, which reads `~` as the pwsh
/// prompt's title writes it ([`windows_home_suffix`]: cmd.exe titles its tab
/// with the whole directory, `C:\Users\you\aterm`, and `C:\Users\you\aterm -
/// ping x` while a command runs) ([`Informative`]). THE strip's rung, used
/// by every label path (`app_tabs::resolved_terminal_title_rung`, the
/// per-frame refill and the pane headers), so the painted strip and the
/// chrome push cannot label one tab differently. An empty result is the
/// fall-through signal to the cwd rung, as for either shed alone.
#[must_use]
pub(crate) fn informative(title: &str) -> Informative<'_> {
    informative_under(title, msys_home(), windows_home())
}

/// [`informative`] with Git Bash's home (its POSIX spelling, `/c/Users//me`)
/// and the Windows profile folder (`C:\Users\me`) handed in, so a test can
/// name them rather than read this machine's.
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

/// What a tab shows of a title ([`informative`]): a slice of the title,
/// behind a `~` where Git Bash's home was cut off its front. Borrowed, so the
/// per-frame refill that calls it on every terminal tab allocates nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Informative<'a> {
    /// The title began with Git Bash's home, which reads `~`.
    home: bool,
    /// The rest, verbatim.
    rest: &'a str,
}

impl Informative<'_> {
    /// Nothing informative is left: the caller falls through to the cwd rung.
    #[must_use]
    pub(crate) fn is_empty(self) -> bool {
        !self.home && self.rest.is_empty()
    }

    /// Append the text to `slot`.
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
/// `HOME` the environment sets, which Git Bash adopts (a Windows spelling
/// converted), else `%USERPROFILE%`, where Git Bash puts it by default. `None`
/// off Windows, where no title carries an MSYS prefix, and when neither is set.
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
/// `/c/Users//me` (the drive letter lower-cased, as Git Bash's `$PWD` writes
/// it); a path already POSIX is kept. Trailing separators are dropped. `None`
/// for anything else (a UNC share, a relative path).
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
/// the `/` that follows it), compared without regard to ASCII case as Windows
/// paths are; `None` when `path` is not at or under it.
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
/// trailing separators dropped: the home a pwsh tab's title reads as `~`
/// (`$HOME` there is the profile folder). `None` off Windows, where a shell
/// writes `~` itself, and when it is unset or not a drive path.
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
/// that heads it: `""` for the folder itself, else from the separator after
/// it (`\aterm`), or from cmd.exe's ` - ` before a running command
/// (`C:\Users\me - ping x` reads ` - ping x`). Compared without regard to
/// ASCII case, as Windows paths are, so a title's own spelling of the folder
/// matches. `None` when the title does not start with the whole folder: a
/// folder beside it (`C:\Users\me2`), one elsewhere, a relative path or prose.
/// No allocation: the per-frame refill calls it on every terminal tab.
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
/// working directory in its title (`$TITLEPREFIX:$PWD`).
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
/// `MINGW64:/c/Users//me/src` reads `/c/Users//me/src`. Every Git Bash tab opens
/// with the same nine characters, which a chip has least room for, and the
/// environment is the one the user launched; what is left is a path, so the
/// strip keeps its end (`tab_bar`'s path cut) rather than the shared head.
/// Only the prefix followed by a POSIX path is the prompt's title
/// (`$TITLEPREFIX:$PWD`); any other shape is `None`. No allocation.
fn msys_title_path(title: &str) -> Option<&str> {
    MSYS_TITLE_PREFIXES
        .iter()
        .find_map(|prefix| title.strip_prefix(prefix))
        .filter(|rest| rest.starts_with('/'))
}

/// The title with a bare Windows executable path removed: ConPTY's title for a
/// console whose program set none.
///
/// The shape is an absolute Windows path (`X:\…` or a UNC `\\…`) whose first `.exe`
/// (ASCII case-insensitive) either ends the title, yielding `""` (the fall-through to
/// the cwd rung), or is followed by `cmd.exe`'s ` - <command>`, yielding the command.
/// Any other title is returned unchanged. No allocation: the per-frame refill calls
/// this on every terminal tab.
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
        // ` - <command>`. The outer trim has already taken the space after a
        // dash with nothing behind it, so a bare ` -` is the same shape.
        if let Some(command) = rest.strip_prefix(" -")
            && (command.is_empty() || command.starts_with(' '))
        {
            return command.trim();
        }
    }
    title
}

/// [`without_local_identity`] with this machine's names handed in, so a test chooses
/// the hosts rather than inheriting the machine running the suite.
#[must_use]
fn without_identity_of<'a>(title: &'a str, local_full: &str, local_short: &str) -> &'a str {
    let trimmed = title.trim();
    // `user@host: rest`, or `user@host` alone. Splitting on the first colon keeps
    // `make: *** No rule` out: its head carries no `@`.
    let (head, rest) = match trimmed.split_once(':') {
        Some((head, rest)) => (head, rest.trim_start()),
        None => (trimmed, ""),
    };
    let Some((user, host)) = head.split_once('@') else {
        return title;
    };
    // No default prompt puts whitespace in either token, and requiring that is
    // what stops a sentence that merely CONTAINS an address — `sent to
    // user@host: ok` — from being read as a prompt's identity.
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
/// prompt may write either (`\h` short, `\H` fully qualified) while the machine
/// reports the other.
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
    /// string.
    fn shed(title: &str) -> String {
        informative_under(title, None, None).to_string()
    }

    /// A WINDOWS TITLE UNDER THE PROFILE FOLDER READS `~`, as the pwsh prompt's
    /// title does: cmd.exe's `C:\Users\me\aterm` is `~\aterm`, the folder
    /// itself `~`, and a running command's `C:\Users\me\aterm - ping x` keeps
    /// its command (`~\aterm - ping x`), in any case of the drive or the
    /// folder. A folder beside it, one elsewhere, pwsh's own `~\aterm`, Git
    /// Bash's title, a title that only mentions the folder and the console's
    /// program path keep their own rules. NEGATIVE CONTROL: with no profile
    /// folder known (every host but Windows), the path is shown whole, as the
    /// 2026-10-18 build showed a cmd tab's (the verifier's
    /// `A8-cmd-title-full-path`).
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
    /// so the chip shows the path, whose end the strip keeps; a title that only
    /// starts with such a word, or names no path after it, is left alone.
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
    /// a pwsh tab's title already does: `MINGW64:/c/Users//me/src` is `~/src`,
    /// home itself `~`, in any case of the drive letter or the folder. A path
    /// beside home (`/c/Users//me2`), one elsewhere, and a title that is not Git
    /// Bash's keep their spelling. NEGATIVE CONTROL: with no home known the
    /// path is shown whole, as the 2026-10-18 build showed it.
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
        // In place, as the per-frame refill writes it.
        let mut slot = String::from("stale");
        slot.clear();
        informative_under("MINGW64:/c/Users//m6-an/src", home, None).push_to(&mut slot);
        assert_eq!(slot, "~/src");
    }

    /// The home Git Bash adopts, in its own spelling: a Windows `HOME` or
    /// `USERPROFILE` gets the lower-case drive and forward slashes, a POSIX one
    /// is kept, and a share or a relative path is no home it can name.
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
    /// shell started before the move reports the old name, one started after
    /// it the new one, and both are this machine. NEGATIVE CONTROLS: a name
    /// the kernel never gave it, and the oldest name once more renames than
    /// are kept have pushed it out, name another machine; nothing read names
    /// nothing.
    #[test]
    fn every_name_the_kernel_gave_this_machine_stays_its_own() {
        let mut names = super::MachineNames::default();
        assert!(!names.names("m19Macb128GB2TB"), "nothing read yet");
        names.note(None);
        names.note(Some(String::new()));
        assert!(!names.names(""), "an unreadable name names nothing");
        names.note(Some("m19Macb128GB2TB.localdomain".into()));
        // The move: a new name, read again at the next ask.
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
    /// with or without `.local` / `.localdomain`. NEGATIVE CONTROLS: a host
    /// that shares only the first label (`<name>.other.example`, `10.0.0.7`
    /// beside `10.0.0.5`), a prefix, and any host against an empty name.
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
    /// successor's own, once each, and past the bound the oldest go: a carried
    /// shell's start-up name stays this machine's. NEGATIVE CONTROL: a name
    /// neither process read names another machine.
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
    /// keeps its directory, a title that is only this machine leaves nothing
    /// (the caller's fall-through to the cwd rung), another machine keeps the
    /// identity that is the point of the tab, and a title a program chose is
    /// returned untouched.
    #[test]
    fn only_this_machines_prompt_identity_is_shed() {
        for (title, want) in [
            // The measured defect: bash's default `\u@\h: \w`, plus `\H` and a
            // no-space spelling.
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
            // Contains an address but is prose, not a prompt's identity.
            (
                "sent to user@m17-tower: ok",
                "sent to user@m17-tower: ok",
            ),
            // Shape-like but degenerate on one side.
            ("@m17-tower: ~/aterm", "@m17-tower: ~/aterm"),
            ("user@: ~/aterm", "user@: ~/aterm"),
        ] {
            assert_eq!(strip(title), want, "{title:?}");
        }
    }

    #[test]
    fn an_unknown_local_name_matches_nothing() {
        // A host that cannot name itself must not start eating every prefix.
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

    /// The measured defect: ConPTY's title for a fresh console is the program's
    /// own path, which names nothing the user did not just choose. Empty is the
    /// fall-through to the cwd rung, exactly as for the local identity.
    /// `cmd.exe`'s `<path> - <command>` keeps the command. Every title that is
    /// not an absolute Windows path to an `.exe` — including ones that CONTAIN
    /// one — is returned byte-identical.
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
    /// that a prompt naming this host in any case matches.
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
