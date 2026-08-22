// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The one place the engine's RFC 8089 URI-path cwd becomes a NATIVE path.
//!
//! `handle_osc_7` in `aterm-core` parses `file:///C:/Users//m6-an` and stores the
//! bare decoded URI path — `/C:/Users//m6-an`. That is not an oversight: RFC 8089
//! says the path component of a `file:` URI on a drive-letter system carries a
//! leading `/` before the drive, and `aterm-core` is deliberately platform-free
//! (it also targets wasm, where "drive letter" is meaningless). Its comments say
//! outright that deciding what the path means locally is the consumer's job.
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

use std::borrow::Cow;

use aterm_core::terminal::Terminal;

/// The GUI's view of a terminal's shell-reported working directory: the engine
/// value, converted to a native path for the host platform.
///
/// An extension trait rather than a free function at each call site so the
/// conversion cannot be forgotten by a reader who sees only
/// `current_working_directory()` and assumes it is already usable. Returns
/// [`Cow`] because the overwhelmingly common case — every path on macOS/Linux,
/// and an already-native `C:\…` on Windows — borrows the engine's own string
/// with no allocation, which matters: the tab-label refill runs per tab per
/// repaint-fingerprint recompute and is written to avoid per-frame `String`s.
pub(crate) trait ReportedCwd {
    /// The shell-reported cwd as a native path, or `None` if never reported.
    fn native_working_directory(&self) -> Option<Cow<'_, str>>;
}

impl ReportedCwd for Terminal {
    fn native_working_directory(&self) -> Option<Cow<'_, str>> {
        self.current_working_directory().map(native_path)
    }
}

/// Convert one RFC 8089 URI path to a native path for the host platform.
///
/// Off Windows this is the identity — see the module docs for why that must be
/// unconditional and not merely "usually true".
///
/// On Windows two shapes are rewritten and nothing else is:
///
/// * A drive-letter URI path (`/C:/Users//x`, or the legacy `/C|/Users//x` that
///   predates RFC 3986 allowing a bare `:` in a path segment) loses its leading
///   `/` and gains backslash separators: `C:\Users\x`.
/// * A UNC payload (`//server/share/dir` — the host-preserving form
///   `handle_osc_7` queues for `file://server/share/dir`) keeps its leading
///   double slash, which is what makes it a network path on both Windows and
///   POSIX, and only has its separators normalised: `\\server\share\dir`.
///
/// Everything else is returned byte-identical, deliberately. An already-native
/// `C:\Users\x` — the cwd can arrive from the spawn cwd or a restore manifest,
/// not just OSC 7 — has no leading slash and so is never touched, which makes
/// the function idempotent and safe to apply to a value of unknown provenance.
/// A POSIX absolute path like `/home/user/proj`, which a WSL or SSH shell will
/// happily report into a Windows aterm, is likewise left alone: turning it into
/// `\home\user\proj` would make a legible remote path illegible for no gain.
///
/// No percent-decoding happens here; `parse_file_uri` already did it, and
/// decoding twice would turn a directory literally named `100%25` into `100%`.
#[must_use]
pub(crate) fn native_path(path: &str) -> Cow<'_, str> {
    #[cfg(windows)]
    {
        if let Some(drive) = drive_letter_uri(path) {
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
    }
    Cow::Borrowed(path)
}

/// The drive letter of a `/C:/…`-shaped URI path, or `None` for any other
/// string.
///
/// The shape test is strict on purpose. Byte 3 must be a separator or the end
/// of the string, so `/C:extra` — a POSIX directory whose name merely STARTS
/// with a drive-looking prefix — is not mistaken for a drive root. The letter
/// must be ASCII alphabetic: `/1:/x` is not a drive, and a non-ASCII leading
/// char must not be sliced through mid-codepoint (hence the byte comparisons,
/// which only ever accept ASCII before indexing at 3).
#[cfg(windows)]
fn drive_letter_uri(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'/' || !bytes[1].is_ascii_alphabetic() {
        return None;
    }
    // RFC 8089 §F.2 keeps the legacy `|` drive separator alive: pre-RFC-3986
    // producers wrote `file:///C|/dir` because `:` was not yet allowed in a
    // path segment. Accepting it costs one byte compare.
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
        // The reported defect, verbatim: the shell emits `file:///C:/Users//x`,
        // the engine stores the RFC 8089 path, and the tab strip must show a
        // path a user could paste into Explorer.
        assert_eq!(native_path("/C:/Users//x"), "C:\\Users\\x");
        assert_eq!(native_path("/c:/Users//x"), "c:\\Users\\x");
        // Legacy `|` separator (RFC 8089 §F.2).
        assert_eq!(native_path("/C|/Users//x"), "C:\\Users\\x");
        // Drive root, with and without the trailing separator: both are the
        // root, and a bare `C:` would instead mean "current dir on drive C".
        assert_eq!(native_path("/C:/"), "C:\\");
        assert_eq!(native_path("/C:"), "C:\\");
    }

    #[cfg(windows)]
    #[test]
    fn an_already_native_windows_path_passes_through_unchanged() {
        // cwd does not only come from OSC 7 — the spawn cwd and the restore
        // manifest hand us native paths — so the conversion must be idempotent
        // and must not double-convert.
        assert_eq!(native_path("C:\\Users\\x"), "C:\\Users\\x");
        assert_eq!(
            native_path(native_path("/C:/Users//x").as_ref()),
            "C:\\Users\\x"
        );
        // Forward slashes with no leading slash are already usable on Windows
        // and are not a URI path; leaving them alone keeps the rule "only a
        // LEADING slash signals RFC 8089" exact.
        assert_eq!(native_path("C:/Users//x"), "C:/Users//x");
    }

    #[cfg(windows)]
    #[test]
    fn a_unc_payload_keeps_its_leading_double_slash() {
        // `handle_osc_7` queues a non-local host as `//host/share/dir`. The
        // double slash IS the network-path marker: strip or halve it and the
        // path silently becomes a local one. Only the separators change.
        assert_eq!(native_path("//server/share"), "\\\\server\\share");
        assert_eq!(native_path("//server/share/dir"), "\\\\server\\share\\dir");
        assert_eq!(native_path("\\\\server\\share"), "\\\\server\\share");
    }

    #[cfg(windows)]
    #[test]
    fn non_drive_shaped_paths_are_left_alone_on_windows_too() {
        // A WSL or SSH shell reporting a POSIX cwd into a Windows aterm: not a
        // drive, so not our business. `/C:extra` is a directory whose name
        // merely starts like a drive.
        assert_eq!(native_path("/home/user/proj"), "/home/user/proj");
        assert_eq!(native_path("/C:extra/x"), "/C:extra/x");
        assert_eq!(native_path("/1:/x"), "/1:/x");
        assert_eq!(native_path(""), "");
    }

    #[cfg(unix)]
    #[test]
    fn the_drive_rewrite_never_runs_on_unix() {
        // THE load-bearing test. `mkdir '/C:'` is legal on macOS and Linux, and
        // a shell inside it reports exactly this. Rewriting it to `C:\foo`
        // would point every consumer — spawn-in-cwd, Copy CWD, the restore
        // manifest — at a directory that does not exist.
        assert_eq!(native_path("/C:/foo"), "/C:/foo");
        assert_eq!(native_path("/C:/Users//x"), "/C:/Users//x");
        assert_eq!(native_path("/C|/foo"), "/C|/foo");
        // And a network path stays byte-identical, forward slashes and all.
        assert_eq!(native_path("//server/share"), "//server/share");
        assert_eq!(native_path("/Users//x"), "/Users//x");
    }

    #[test]
    fn conversion_is_borrow_only_for_paths_it_does_not_touch() {
        // The hot tab-label path must not allocate per frame for the ordinary
        // case; `Cow::Borrowed` is the observable proof.
        assert!(matches!(
            native_path("/Users//x"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert!(matches!(native_path(""), std::borrow::Cow::Borrowed(_)));
    }
}
