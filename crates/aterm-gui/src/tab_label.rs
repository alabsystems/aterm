// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Drop the part of an OSC title that names the machine the user is already on.
//!
//! ## The defect this exists to prevent
//!
//! The default prompt on Debian, Ubuntu, Fedora and macOS sets the terminal title
//! to some spelling of `user@host: cwd` — bash's stock `PROMPT_COMMAND` writes
//! `\u@\h: \w` and the others differ only in punctuation. Every tab of a local
//! session therefore opens with the SAME twenty-odd characters, and they are the
//! characters a tab has least room for.
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
//! The title is display-only chrome. The terminal's OSC 0/2 title remains
//! authoritative identity and is not rewritten: `ctl title` and `meta` still
//! report exactly what the program set (the roster's `ls` title drops only
//! the console program path, keeping `user@host:` — see
//! `control_session::roster_title`). A title a person or a program
//! actually chose (`vim README.md`, a TUI's own name) matches none of the
//! shapes below and survives in full.

use std::sync::OnceLock;

/// This machine's names, resolved once: the hostname as configured, plus its
/// short form (the label before the first dot, which is what bash's `\h` writes).
fn local_host_names() -> &'static (String, String) {
    static NAMES: OnceLock<(String, String)> = OnceLock::new();
    NAMES.get_or_init(|| {
        let full = resolve_hostname().unwrap_or_default();
        let short = full
            .split_once('.')
            .map_or(full.as_str(), |(label, _)| label)
            .to_owned();
        (full, short)
    })
}

/// Read the host's own name without spawning anything: the filesystem or an API
/// answers on every platform (it once ran `hostname`, a process on the UI thread at
/// the first tab label). `$HOSTNAME` is consulted first because a user who overrides
/// it is describing the same machine the prompt will name.
fn resolve_hostname() -> Option<String> {
    resolve_hostname_over(std::env::var_os("HOSTNAME"))
}

/// [`resolve_hostname`] with `$HOSTNAME` passed in: no test may set it.
fn resolve_hostname_over(env: Option<std::ffi::OsString>) -> Option<String> {
    if let Some(name) = env
        && let Some(name) = name.to_str()
        && !name.trim().is_empty()
    {
        return Some(name.trim().to_owned());
    }
    #[cfg(target_os = "linux")]
    if let Ok(name) = std::fs::read_to_string("/proc/sys/kernel/hostname")
        && !name.trim().is_empty()
    {
        return Some(name.trim().to_owned());
    }
    #[cfg(windows)]
    {
        windows_host_name()
    }
    #[cfg(unix)]
    {
        unix_host_name()
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// The name `hostname` prints, read the way it reads it: `gethostname(3)`.
#[cfg(unix)]
fn unix_host_name() -> Option<String> {
    unsafe extern "C" {
        fn gethostname(name: *mut libc::c_char, len: libc::size_t) -> libc::c_int;
    }
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for its whole length, which is what is passed.
    if unsafe { gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..len]).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// Windows: the DNS host name, as `hostname.exe` prints it
/// (`GetComputerNameExW(ComputerNameDnsHostname)`), else `%COMPUTERNAME%` (the
/// NetBIOS name: upper-cased, at most 15 characters, so a longer name would not
/// match). NEVER a spawn here: `$HOSTNAME` is unset on Windows, so this ran
/// `hostname.exe` once per window process — on the UI thread, at the first tab
/// label — and the window, which has no console, opened a visible console window
/// for it (measured 2026-09-30). Measured on that machine: `hostname` printed the
/// name in its own case, `%COMPUTERNAME%` the same name upper-cased.
#[cfg(windows)]
fn windows_host_name() -> Option<String> {
    /// `COMPUTER_NAME_FORMAT::ComputerNameDnsHostname` (sysinfoapi.h).
    const COMPUTER_NAME_DNS_HOSTNAME: u32 = 1;
    /// A DNS host label is at most 63 characters; this holds any name.
    const CAPACITY: usize = 256;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetComputerNameExW(name_type: u32, buffer: *mut u16, size: *mut u32) -> i32;
    }
    let mut buf = [0u16; CAPACITY];
    let mut len = CAPACITY as u32;
    // SAFETY: `buf` is `len` writable UTF-16 units and outlives the call. On
    // success the API writes the name and its NUL inside them and sets `len` to
    // the name's length without the NUL; on failure nothing written is read.
    let ok = unsafe { GetComputerNameExW(COMPUTER_NAME_DNS_HOSTNAME, buf.as_mut_ptr(), &mut len) };
    let dns = (ok != 0)
        .then(|| buf.get(..len as usize).map(String::from_utf16_lossy))
        .flatten()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    dns.or_else(|| {
        std::env::var("COMPUTERNAME")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    })
}

/// The title with a `user@host:` prefix naming this machine removed.
///
/// Returns the input unchanged when the title is not of that shape, or names a
/// different host. An `""` result means the title said nothing BUT the local
/// identity, which is the caller's signal to fall through to its next rung (the
/// working directory) — see [`crate::app_tabs::resolved_terminal_title_rung`].
#[must_use]
pub(crate) fn without_local_identity(title: &str) -> &str {
    let (full, short) = local_host_names();
    without_identity_of(title, full, short)
}

/// Whether `host` names THIS machine, by the rule the strip reads a prompt's
/// `user@host:` with ([`host_is_local`]): the question a file citation asks of
/// the host a session's OSC 7 named (`crate::path_link`), where a remote
/// shell's directory must never resolve against this machine's disk. Any of
/// the machine's names counts: the one the strip uses (a `$HOSTNAME` override
/// first) and, on unix, every name the kernel has given it while this process
/// looked ([`MachineNames`]: `gethostname(3)`, what zsh's `$HOST` and bash's
/// `$HOSTNAME` hold, so what a local shell reports).
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
    let (full, short) = local_host_names();
    host_is_local(host, full, short) || {
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

/// The kernel's name for this machine now (`gethostname(3)`), or `None`.
fn kernel_host_name() -> Option<String> {
    #[cfg(unix)]
    {
        unix_host_name()
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

    /// Whether `host` is one of the names kept, by [`host_is_local`].
    pub(crate) fn names(&self, host: &str) -> bool {
        self.seen.iter().any(|full| {
            let short = full
                .split_once('.')
                .map_or(full.as_str(), |(label, _)| label);
            host_is_local(host, full, short)
        })
    }
}

/// This machine's own name as [`names_this_machine`] reads it (`""` when it
/// cannot be read), for a test that must spell a local OSC 7.
#[cfg(test)]
pub(crate) fn this_machine_name() -> &'static str {
    &local_host_names().0
}

/// The title with everything that names only what the user already knows
/// removed: a `user@host:` prefix naming this machine
/// ([`without_local_identity`]) and the console's own program path
/// ([`without_console_program_path`]). THE strip's rung, used by both label
/// paths (`app_tabs::resolved_terminal_title_rung` and the per-frame refill),
/// so the painted strip and the chrome push cannot label one tab differently.
/// `""` is the fall-through signal to the cwd rung, as for either shed alone.
#[must_use]
pub(crate) fn informative(title: &str) -> &str {
    without_console_program_path(without_local_identity(title))
}

/// The title with a bare Windows executable path removed — ConPTY's title for
/// a console whose program set none (see the module docs for the measurement).
///
/// The shape is an ABSOLUTE Windows path (`X:\…` or a UNC `\\…`) whose first
/// `.exe` (ASCII case-insensitive) either ends the title, which yields `""`
/// (the caller's fall-through to the cwd rung), or is followed by `cmd.exe`'s
/// ` - <command>` convention, which yields the command alone. Any other title
/// is returned unchanged: a relative or POSIX path, an `.exe` mid-path
/// (`C:\apps\foo.exe.bak\bar.txt`), or prose that merely contains one. The
/// shape cannot occur on Unix, so that platform's labels are untouched.
///
/// No allocation: the per-frame refill calls this on every terminal tab.
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

/// [`without_local_identity`] with this machine's names handed in.
///
/// The decision is the whole subject and it turns on which host the title names,
/// so a test drives it against hosts it chooses rather than against whatever
/// machine happens to be running the suite.
#[must_use]
fn without_identity_of<'a>(title: &'a str, local_full: &str, local_short: &str) -> &'a str {
    let trimmed = title.trim();
    // `user@host: rest`, or `user@host` alone at the end of the title. Splitting
    // on the FIRST colon is what keeps `make: *** No rule` out of this: its head
    // (`make`) carries no `@`, so the shape check below rejects it outright.
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
        host_is_local, informative, resolve_hostname_over, without_console_program_path,
        without_identity_of,
    };

    const FULL: &str = "m17-tower.local";
    const SHORT: &str = "m17-tower";

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

    fn strip(title: &str) -> &str {
        without_identity_of(title, FULL, SHORT)
    }

    /// A `$HOSTNAME` a person set names the machine; with none, the name is
    /// read in-process and is the one `hostname` prints.
    #[test]
    fn the_hostname_override_wins_and_the_machine_s_own_name_is_hostname_s() {
        assert_eq!(
            resolve_hostname_over(Some(" my-override.local ".into())).as_deref(),
            Some("my-override.local")
        );
        #[cfg(target_os = "macos")]
        {
            let printed = aterm_uds::spawnfd::command("hostname")
                .output()
                .expect("`hostname` runs");
            let printed = String::from_utf8_lossy(&printed.stdout).trim().to_owned();
            assert_eq!(
                resolve_hostname_over(None),
                Some(printed).filter(|p| !p.is_empty())
            );
            assert_eq!(
                resolve_hostname_over(Some("  ".into())),
                resolve_hostname_over(None)
            );
        }
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
            assert_eq!(informative(path), "", "{path:?}");
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

    /// Windows names this machine from the OS, with no `hostname.exe` child (the
    /// window has no console, so that child was a console window on screen): a
    /// one-label name, as `hostname` prints it, that a prompt naming this host
    /// in any case matches.
    #[cfg(windows)]
    #[test]
    fn windows_names_this_machine_without_a_child() {
        let name = super::windows_host_name().expect("Windows names this machine");
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
