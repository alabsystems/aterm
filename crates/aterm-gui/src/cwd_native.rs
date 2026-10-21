// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Converts the engine's RFC 8089 URI-path cwd to a native path.
//!
//! `handle_osc_7` in `aterm-core` stores the decoded URI path of `file:///C:/Users//x`, which is
//! `/C:/Users//x`: the engine is platform-free (it also targets wasm), and its stored cwd must not
//! differ by host platform, or a cast recorded on Windows would decode differently on macOS. So
//! the GUI converts once, at its boundary, through [`ReportedCwd::native_working_directory`].
//! Unconverted on Windows, `Command::current_dir("/C:/Users//x")` names a `C:` directory under the
//! current drive's root.
//!
//! aterm-gui is that consumer, and it was not doing the job: on Windows the
//! `/C:/Users//m6-an` string went straight to the tab strip, the tab tooltip,
//! `aterm-ctl cwd`, `aterm-ctl meta`, the restore manifest, and the
//! spawn-in-the-focused-pane's-directory path. Users saw a path that is not a
//! path, and `Command::current_dir("/C:/Users//m6-an")` is a directory named
//! `C:` under the root of the current drive — usually nonexistent.
//!
//! ## Why this is not fixed in `aterm-core`
//!
//! Considered and rejected: teaching `handle_osc_7` about drive letters behind
//! `cfg(windows)`. That would make the ENGINE's stored cwd differ by host
//! platform, which breaks the recorded-session / asciicast replay contract (a
//! cast recorded on Windows must decode identically on macOS) and drags a
//! Windows path concept into a crate that compiles to wasm. Converting once at
//! the GUI boundary keeps the engine byte-for-byte platform-neutral and still
//! gives every downstream consumer the right value, because they all read the
//! cwd through [`ReportedCwd::native_working_directory`].
//!
//! ## Why the rewrite is `cfg(windows)`-gated
//!
//! On unix `/C:/foo` is a perfectly ordinary absolute path: a directory
//! literally named `C:` at the filesystem root. `mkdir '/C:'` is legal on macOS
//! and Linux, and a shell sitting in it reports exactly the string this module
//! rewrites. An ungated rewrite would silently corrupt that path into `C:\foo`
//! — a nonexistent location — so the whole transform is compiled out entirely
//! off Windows and [`native_path`] is a provable identity there.
//!
//! ## A report from another machine is left as written, for every consumer
//!
//! A drive-letter (`/C:/x`) or MSYS (`/c/x`) report whose URI names another
//! host (`file://build-box/C:/x`: a shell over SSH on another Windows box)
//! names THAT box's `C:\x`, and it is never rewritten
//! ([`native_path_reported`]): every consumer above — the tab strip and its
//! tooltip, `aterm-ctl cwd` and `meta`, the restore manifest, the spawn in the
//! focused pane's directory, the disk watch — gets the URI path as the
//! engine stored it, as for a foreign POSIX path (`/home/u/x`). Deliberate,
//! on Windows, for all of them alike: a rewritten `C:\x` would be an
//! absolute path on THIS machine's drive, so a new pane would start in a
//! local folder the remote shell never named (a restored session too), and
//! the disk watch would place the tab there instead of reading its working
//! directory as unknown — the rule `crate::path_link` keeps for a citation,
//! that a remote shell's directory never resolves against this machine's
//! disk. What it costs is the label: such a tab reads `/C:/x`, the remote
//! folder in its URI spelling, not as that box's shell spells it. Which host
//! is this machine is [`reported_here`]'s to say.

use std::borrow::Cow;

use aterm_core::terminal::Terminal;

/// The GUI's view of a terminal's shell-reported working directory: the engine value as a
/// native path. An extension trait so a caller reaches the cwd through the conversion. Returns
/// [`Cow`] so the common untouched path borrows: the tab-label refill avoids per-frame `String`s.
pub(crate) trait ReportedCwd {
    /// The shell-reported cwd as a native path, or `None` if never reported.
    fn native_working_directory(&self) -> Option<Cow<'_, str>>;
}

impl ReportedCwd for Terminal {
    fn native_working_directory(&self) -> Option<Cow<'_, str>> {
        let path = self.current_working_directory()?;
        // The host the report named, where it named one: only a report from
        // this machine is a path on this machine's drives.
        let here = || {
            reported_here(
                self.reported_cwd_host(),
                crate::tab_label::names_this_machine,
                netbios_name(),
            )
        };
        Some(native_path_reported(path, here))
    }
}

/// Whether a report naming `host` (`None`: it named none, or `localhost`)
/// was made on THIS machine: one naming no host was, and so was one naming a
/// host `names` calls this machine's ([`crate::tab_label::names_this_machine`]:
/// its DNS name, in any case) or, on Windows, its NetBIOS name `netbios` —
/// what aterm's PowerShell integration writes in its URI
/// (`[Environment]::MachineName`: the DNS name upper-cased and cut to fifteen
/// characters, so a longer DNS name is not it). Any other host is another
/// machine.
fn reported_here(host: Option<&str>, names: impl Fn(&str) -> bool, netbios: Option<&str>) -> bool {
    host.is_none_or(|host| {
        names(host)
            || netbios
                .map(str::trim)
                .is_some_and(|n| !n.is_empty() && host.trim().eq_ignore_ascii_case(n))
    })
}

/// This machine's NetBIOS name, read from the OS
/// ([`aterm_uds::process::netbios_host_name`]: the name
/// `[Environment]::MachineName` answers, whatever this process's environment
/// holds — a window started with an environment of its own may have no
/// `%COMPUTERNAME%`), read once: it changes only at a restart. `None` off
/// Windows or where neither the OS nor the environment names one.
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
///
/// Off Windows this is the identity — see the module docs for why that must be
/// unconditional and not merely "usually true".
///
/// On Windows three shapes are rewritten and nothing else is:
///
/// * A drive-letter URI path (`/C:/Users//x`, or the legacy `/C|/Users//x` that
///   predates RFC 3986 allowing a bare `:` in a path segment) loses its leading
///   `/` and gains backslash separators: `C:\Users\x`. Only a report from
///   THIS machine is rewritten so ([`native_path_reported`]): a shell on
///   another Windows box reporting `file://other/C:/x` names that box's
///   `C:\x`, and left as written it is no absolute native path here — the
///   disk watch then reads that tab's working directory as unknown, never as
///   this machine's folder.
/// * A UNC payload (`//server/share/dir` — the host-preserving form
///   `handle_osc_7` queues for `file://server/share/dir`) keeps its leading
///   double slash, which is what makes it a network path on both Windows and
///   POSIX, and only has its separators normalised: `\\server\share\dir`.
/// * An MSYS/Cygwin drive path (`/c/Users//x`, `/cygdrive/c/Users//x`) — the
///   form Git Bash, and so Claude Code's Bash tool, reports its `$PWD` in —
///   becomes the drive path it names, the letter upper-cased as Windows spells
///   it: `C:\Users\x`. Without it a Git Bash tab's cwd is not an absolute
///   native path at all, so nothing that places a directory (spawn-in-cwd,
///   the disk watch's session cwds) can use it. Only a one-letter first
///   component (after `/cygdrive`) is a drive: `/cx/y`, `/usr/bin` and the
///   other MSYS-root paths name no drive and are left alone. Only a report
///   from THIS machine is rewritten so ([`native_path_reported`]): an SSH
///   shell on another box reporting `/c/x` names that box's folder, not this
///   one's `C:\x`.
///
/// Everything else is returned byte-identical, deliberately. An already-native
/// `C:\Users\x` — the cwd can arrive from the spawn cwd or a restore manifest,
/// not just OSC 7 — has no leading slash and so is never touched, which makes
/// the function idempotent and safe to apply to a value of unknown provenance.
/// A POSIX absolute path like `/home/user/proj`, which a WSL or SSH shell will
/// happily report into a Windows aterm, is likewise left alone (as is a
/// drive-letter or MSYS-shaped one a report from another host named):
/// turning it into `\home\user\proj` would make a legible remote path
/// illegible for no gain. A WSL shell reports its drives as `/mnt/c/…`, which
/// names no drive here.
///
/// Everything else is returned unchanged: a POSIX path from a WSL or SSH shell, and an
/// already-native `C:\Users\x` (the cwd also comes from the spawn cwd or a restore manifest),
/// which makes the function idempotent. No percent-decoding: `parse_file_uri` already did it,
/// and a second pass would turn `100%25` into `100%`.
#[must_use]
pub(crate) fn native_path(path: &str) -> Cow<'_, str> {
    native_path_reported(path, || true)
}

/// [`native_path`] for a report `here` says whether this machine made (asked
/// only of a drive-letter or an MSYS-shaped path, each rewritten only then;
/// the engine stores the host a report named beside its path, and only the
/// GUI knows its own names, [`reported_here`]).
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
            // `/C:` and `/C:/` both mean the ROOT of drive C. Emitting the bare
            // `C:` instead would be a different location entirely: on Windows a
            // driveless-but-drive-qualified path means "the process's current
            // directory ON that drive", so the trailing separator is load-bearing.
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
            // `/c` and `/c/` are both the drive's ROOT; `C:` alone would be the
            // current directory on that drive (as for `/C:` above).
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
/// other string. As strict as [`drive_letter_uri`]: one ASCII letter, then a
/// separator or the end, so `/cx/y` and `/cygdrive/cx` name no drive.
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
        // Bytes 0 and 1 are ASCII, so 2 is a char boundary.
        None | Some(b'/') => Some((bytes[1] as char, &tail[2..])),
        Some(_) => None,
    }
}

/// The drive letter of a `/C:/…`-shaped URI path, or `None` for any other
/// string.
///
/// Byte 3 must be a separator or the end, so the POSIX directory `/C:extra` is not a drive
/// root. The letter must be ASCII alphabetic; comparing bytes before indexing at 3 never
/// slices through a multi-byte char.
#[cfg(windows)]
fn drive_letter_uri(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'/' || !bytes[1].is_ascii_alphabetic() {
        return None;
    }
    // RFC 8089 §F.2: pre-RFC-3986 producers wrote the legacy `file:///C|/dir`.
    if bytes[2] != b':' && bytes[2] != b'|' {
        return None;
    }
    match bytes.get(3) {
        None | Some(b'/') | Some(b'\\') => Some(bytes[1] as char),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::native_path;

    #[cfg(windows)]
    #[test]
    fn drive_letter_uri_path_becomes_a_native_windows_path() {
        assert_eq!(native_path("/C:/Users//x"), "C:\\Users\\x");
        assert_eq!(native_path("/c:/Users//x"), "c:\\Users\\x");
        // Legacy `|` separator (RFC 8089 §F.2).
        assert_eq!(native_path("/C|/Users//x"), "C:\\Users\\x");
        // Drive root, with and without the trailing separator.
        assert_eq!(native_path("/C:/"), "C:\\");
        assert_eq!(native_path("/C:"), "C:\\");
    }

    #[cfg(windows)]
    #[test]
    fn an_already_native_windows_path_passes_through_unchanged() {
        // The spawn cwd and restore manifest hand over native paths: idempotent.
        assert_eq!(native_path("C:\\Users\\x"), "C:\\Users\\x");
        assert_eq!(
            native_path(native_path("/C:/Users//x").as_ref()),
            "C:\\Users\\x"
        );
        // Only a leading slash marks an RFC 8089 path.
        assert_eq!(native_path("C:/Users//x"), "C:/Users//x");
    }

    #[cfg(windows)]
    #[test]
    fn a_unc_payload_keeps_its_leading_double_slash() {
        // The double slash is the network-path marker; only separators change.
        assert_eq!(native_path("//server/share"), "\\\\server\\share");
        assert_eq!(native_path("//server/share/dir"), "\\\\server\\share\\dir");
        assert_eq!(native_path("\\\\server\\share"), "\\\\server\\share");
    }

    #[cfg(windows)]
    #[test]
    fn non_drive_shaped_paths_are_left_alone_on_windows_too() {
        assert_eq!(native_path("/home/user/proj"), "/home/user/proj");
        assert_eq!(native_path("/C:extra/x"), "/C:extra/x");
        assert_eq!(native_path("/1:/x"), "/1:/x");
        assert_eq!(native_path(""), "");
    }

    #[cfg(windows)]
    #[test]
    fn an_msys_drive_path_becomes_the_drive_path_it_names() {
        // Git Bash reports its `$PWD` as `/c/Users//x`: a tab sitting in a
        // checkout must read as that checkout, on the drive spelled as Windows
        // spells it.
        assert_eq!(native_path("/c/Users//x"), "C:\\Users\\x");
        assert_eq!(native_path("/D/src/a b"), "D:\\src\\a b");
        assert_eq!(native_path("/cygdrive/c/Users//x"), "C:\\Users\\x");
        // The drive's root, with and without the separator, never the bare
        // `C:` (that drive's current directory).
        assert_eq!(native_path("/c"), "C:\\");
        assert_eq!(native_path("/c/"), "C:\\");
        assert_eq!(native_path("/cygdrive/e"), "E:\\");
        // Idempotent, like the other rewrites.
        assert_eq!(
            native_path(native_path("/c/Users//x").as_ref()),
            "C:\\Users\\x"
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_msys_drive_path_is_this_machines_only_when_this_machine_reported_it() {
        use super::{ReportedCwd as _, native_path_reported};
        use aterm_core::terminal::Terminal;
        // An SSH shell on another box sitting in its own `/c/x`: that box's
        // folder, never this machine's `C:\x`. NEGATIVE CONTROL: the same
        // report from this machine is rewritten.
        assert_eq!(native_path_reported("/c/x", || false), "/c/x");
        assert_eq!(
            native_path_reported("/cygdrive/c/x", || false),
            "/cygdrive/c/x"
        );
        assert_eq!(native_path_reported("/c/x", || true), "C:\\x");
        // The shapes that name no drive do not ask: `here` is asked of a drive
        // path only.
        assert_eq!(
            native_path_reported("//srv/share", || panic!("not asked")),
            "\\\\srv\\share"
        );
        assert_eq!(
            native_path_reported("/home/x", || panic!("not asked")),
            "/home/x"
        );
        // Through the engine: the host a report names decides.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]7;file://build-box.example/c/Users//x\x07");
        assert_eq!(
            term.native_working_directory().as_deref(),
            Some("/c/Users//x")
        );
        for local in [
            &b"\x1b]7;file:///c/Users//x\x07"[..],
            b"\x1b]7;file://localhost/c/Users//x\x07",
        ] {
            term.process(local);
            assert_eq!(
                term.native_working_directory().as_deref(),
                Some("C:\\Users\\x"),
                "{local:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_letter_path_is_this_machines_only_when_this_machine_reported_it() {
        use super::{ReportedCwd as _, native_path_reported};
        use aterm_core::terminal::Terminal;
        // A shell on another Windows box sitting in its own `C:\x`
        // (`file://other/C:/x`): that box's folder, never this machine's, so
        // it stays as written — no absolute native path here, which the disk
        // watch reads as a tab whose working directory is unknown. NEGATIVE
        // CONTROL: the same report from this machine is rewritten.
        assert_eq!(native_path_reported("/C:/x", || false), "/C:/x");
        assert_eq!(native_path_reported("/C|/x", || false), "/C|/x");
        assert!(!std::path::Path::new("/C:/x").is_absolute());
        assert_eq!(native_path_reported("/C:/x", || true), "C:\\x");
        // Through the engine: the host a report names decides.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]7;file://build-box.example/C:/Users//x\x07");
        assert_eq!(
            term.native_working_directory().as_deref(),
            Some("/C:/Users//x")
        );
        for local in [
            &b"\x1b]7;file:///C:/Users//x\x07"[..],
            b"\x1b]7;file://localhost/C:/Users//x\x07",
        ] {
            term.process(local);
            assert_eq!(
                term.native_working_directory().as_deref(),
                Some("C:\\Users\\x"),
                "{local:?}"
            );
        }
    }

    /// THIS MACHINE BY ANY OF ITS NAMES: a report naming no host, its DNS
    /// name (the names the strip knows) or — what aterm's PowerShell
    /// integration writes — its NetBIOS name, in any case, is this machine's.
    /// NEGATIVE CONTROLS: another host is not, nor is any host when the
    /// NetBIOS name is unknown or blank and the DNS names do not match.
    #[test]
    fn a_report_is_this_machines_by_any_of_its_names() {
        use super::reported_here;
        // A DNS name longer than NetBIOS keeps: the two differ.
        let dns = |h: &str| h.eq_ignore_ascii_case("build-machine-of-andrew.lan");
        let netbios = Some("BUILD-MACHINE-O");
        assert!(reported_here(None, dns, netbios));
        assert!(reported_here(
            Some("Build-Machine-Of-Andrew.lan"),
            dns,
            netbios
        ));
        assert!(reported_here(Some("BUILD-MACHINE-O"), dns, netbios));
        assert!(reported_here(Some("build-machine-o"), dns, netbios));
        assert!(!reported_here(Some("other-box"), dns, netbios));
        assert!(!reported_here(Some("BUILD-MACHINE-O"), dns, None));
        assert!(!reported_here(Some("BUILD-MACHINE-O"), dns, Some("  ")));
    }

    #[cfg(windows)]
    #[test]
    fn only_a_one_letter_msys_component_is_a_drive() {
        // NEGATIVE CONTROLS: MSYS-root paths and longer names name no drive.
        for left in [
            "/usr/bin",
            "/tmp",
            "/cx/y",
            "/c:x",
            "/cygdrive",
            "/cygdrive/",
            "/cygdrive/cx/y",
            "/cygdrivex/c/y",
            "/1/x",
            "/é/x",
        ] {
            assert_eq!(native_path(left), left, "{left}");
            assert!(matches!(native_path(left), std::borrow::Cow::Borrowed(_)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_drive_rewrite_never_runs_on_unix() {
        // `/C:` is a legal directory on unix; rewriting it would point every
        // consumer at a path that does not exist.
        assert_eq!(native_path("/C:/foo"), "/C:/foo");
        assert_eq!(native_path("/C:/Users//x"), "/C:/Users//x");
        assert_eq!(native_path("/C|/foo"), "/C|/foo");
        // `/c/x` is an ordinary directory here, not an MSYS drive.
        assert_eq!(native_path("/c/x"), "/c/x");
        assert_eq!(native_path("/cygdrive/c/x"), "/cygdrive/c/x");
        // And a network path stays byte-identical, forward slashes and all.
        assert_eq!(native_path("//server/share"), "//server/share");
        assert_eq!(native_path("/Users//x"), "/Users//x");
    }

    #[test]
    fn conversion_is_borrow_only_for_paths_it_does_not_touch() {
        // The tab-label refill must not allocate for an untouched path.
        assert!(matches!(
            native_path("/Users//x"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert!(matches!(native_path(""), std::borrow::Cow::Borrowed(_)));
    }
}
