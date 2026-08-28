// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `softwareupdate` protocol's install lane (macOS): Apple's Command Line Tools,
//! installed HEADLESSLY by `softwareupdate` — never by `xcode-select --install`, which
//! opens a dialog no background pass may spawn.
//!
//! The recipe Apple's own tooling uses, and the one Homebrew's `install.sh` runs:
//!
//! 1. touch [`PLACEHOLDER`] — the file `softwareupdate` reads as "list the Command
//!    Line Tools packages too" (they are hidden from `-l` otherwise);
//! 2. `softwareupdate -l` ([`list_argv`]), parsed for every label starting with the
//!    row's `label_prefix` (`Command Line Tools for Xcode`), the NEWEST picked
//!    ([`pick_label`]);
//! 3. `softwareupdate -i <label>` ([`install_argv`]) under the caller's elevation
//!    ([`crate::elevate::elevated_argv`]);
//! 4. the placeholder removed — on EVERY path, success or failure, by a guard's `Drop`;
//! 5. the `provides` probe ([`crate::elevate::first_provided`]): the absolute path
//!    `/Library/Developer/CommandLineTools/usr/bin/git` proves the install; a `git`
//!    anywhere else never does (the row's admission refuses bare names for this reason).
//!
//! Elevation is the flow's decision: the unattended pass defers upstream with the
//! canonical `needs admin` state and never reaches step 1.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::elevate::{Elevation, Io, Runner};
use crate::manifest::Artifact;

/// The protocol's spelling, as the canonical state prints it.
pub const PROTOCOL: &str = "softwareupdate";
/// Apple's software update tool.
pub const SOFTWAREUPDATE: &str = "/usr/sbin/softwareupdate";
/// The on-demand marker that makes `softwareupdate -l` list the Command Line Tools.
pub const PLACEHOLDER: &str = "/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress";

/// `softwareupdate -l`.
#[must_use]
pub fn list_argv() -> Vec<String> {
    vec![String::from(SOFTWAREUPDATE), String::from("-l")]
}

/// `softwareupdate -i <label>`.
#[must_use]
pub fn install_argv(label: &str) -> Vec<String> {
    vec![
        String::from(SOFTWAREUPDATE),
        String::from("-i"),
        String::from(label),
    ]
}

/// The newest label in a `softwareupdate -l` listing that starts with `prefix`, or
/// `None` when the listing carries none (or is not a listing at all).
///
/// Both spellings Apple has used are read: the modern `* Label: Command Line Tools for
/// Xcode-16.4` and the older `   * Command Line Tools (macOS Mojave version 10.14) for
/// Xcode-10.1`. "Newest" is the dotted version after the label's LAST `-`, compared
/// numerically component by component (`16.4` > `16.10`? no — `16.10` > `16.4`, as
/// versions go); a label with no version sorts lowest. Ties keep the first seen.
#[must_use]
pub fn pick_label(listing: &str, prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    let mut best: Option<(Vec<u64>, String)> = None;
    for raw in listing.lines() {
        let line = raw.trim();
        let candidate = if let Some(rest) = line.strip_prefix("* Label:") {
            rest.trim()
        } else if let Some(rest) = line.strip_prefix("* ") {
            rest.trim()
        } else {
            continue;
        };
        if !candidate.starts_with(prefix) {
            continue;
        }
        // The label becomes ONE `softwareupdate -i` argument (sh-quoted whole on the
        // osascript door), so it cannot be re-split; it may still not carry a control
        // byte — a listing line is one line, but a tab or DEL inside it is not a label
        // Apple ever printed, and a label that is not printable is not one we install.
        if candidate.bytes().any(|b| b < b' ' || b == 0x7f) {
            continue;
        }
        let version = version_key(candidate);
        let newer = best.as_ref().is_none_or(|(v, _)| version > *v);
        if newer {
            best = Some((version, candidate.to_string()));
        }
    }
    best.map(|(_, label)| label)
}

/// The numeric dotted version after `label`'s last `-` (`Xcode-16.4` → `[16, 4]`); a
/// non-numeric component ends the key there, and no `-` at all is the empty key.
fn version_key(label: &str) -> Vec<u64> {
    let Some((_, tail)) = label.rsplit_once('-') else {
        return Vec::new();
    };
    let mut key = Vec::new();
    for part in tail.split('.') {
        match part.parse::<u64>() {
            Ok(n) => key.push(n),
            Err(_) => break,
        }
    }
    key
}

/// The placeholder file, created on construction and REMOVED on drop — so the marker
/// never outlives the lane, whichever way the lane ends.
struct Placeholder {
    path: PathBuf,
}

impl Placeholder {
    fn touch(path: &Path) -> Result<Self, String> {
        // `fs::write` goes via `call2` — see `lib.rs` on the hardened `write` matcher.
        crate::call2(std::fs::write, path, b"").map_err(|e| {
            let mut m = String::from("cannot create the softwareupdate placeholder ");
            m.push_str(&path.to_string_lossy());
            m.push_str(": ");
            m.push_str(&e.to_string());
            m
        })?;
        Ok(Placeholder {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for Placeholder {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The lane: list, pick, install under `elevation`, prove. `placeholder` is
/// [`PLACEHOLDER`] in production and a scratch path in tests. Never decides elevation
/// (a [`Elevation::Deferred`] here is refused, not run unelevated).
///
/// # Errors
/// The placeholder could not be created, `softwareupdate -l` failed or listed no label
/// under `label_prefix`, the elevated install failed, or nothing proves the install.
pub fn install(
    runner: &dyn Runner,
    elevation: Elevation,
    artifact: &Artifact,
    placeholder: &Path,
    prefix: &Path,
    path_var: Option<&OsStr>,
) -> Result<PathBuf, String> {
    if elevation == Elevation::Deferred {
        return Err(String::from(
            "the softwareupdate lane was entered without an elevation policy — nothing was run",
        ));
    }
    // Created first, dropped last: `-l` needs it, and `-i` keeps needing it.
    let _marker = Placeholder::touch(placeholder)?;
    let listed = runner.run(&list_argv(), Io::Capture)?;
    if !listed.success() {
        let mut m = String::from("softwareupdate -l failed");
        if let Some(code) = listed.code {
            m.push_str(" (exit ");
            m.push_str(&crate::dec_u64(u64::from(code.unsigned_abs())));
            m.push(')');
        }
        let tail = listed.stderr.trim();
        if !tail.is_empty() {
            m.push_str(": ");
            m.push_str(tail.lines().last().unwrap_or(tail));
        }
        return Err(m);
    }
    // Apple prints the listing on stdout and its banner on stderr; read both.
    let mut listing = listed.stdout.clone();
    listing.push('\n');
    listing.push_str(&listed.stderr);
    let Some(label) = pick_label(&listing, &artifact.label_prefix) else {
        let mut m = String::from("softwareupdate -l lists no label starting with \"");
        m.push_str(&artifact.label_prefix);
        m.push_str("\" — Apple's catalog offers no Command Line Tools for this macOS right now");
        return Err(m);
    };
    let Some(argv) = crate::elevate::elevated_argv(elevation, &install_argv(&label)) else {
        return Err(String::from("no elevation policy"));
    };
    let ran = runner.run(&argv, Io::Inherit)?;
    if !ran.success() {
        let mut m = String::from("softwareupdate -i \"");
        m.push_str(&label);
        m.push_str("\" failed");
        if let Some(code) = ran.code {
            m.push_str(" (exit ");
            m.push_str(&crate::dec_u64(u64::from(code.unsigned_abs())));
            m.push(')');
        } else {
            m.push_str(" (killed by a signal)");
        }
        m.push_str(" — nothing recorded; re-run: aterm pkg install <name>");
        return Err(m);
    }
    crate::elevate::first_provided(prefix, &artifact.provides, path_var)
        .ok_or_else(|| crate::elevate::nothing_provided(PROTOCOL, &artifact.provides))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elevate::testkit::{Recorder, failed, ok};
    use std::rc::Rc;

    /// A modern listing with three Command Line Tools labels (out of order) and an
    /// unrelated package.
    const MODERN: &str = "Software Update Tool

Finding available software
Software Update found the following new or updated software:
* Label: Command Line Tools for Xcode-16.2
\tTitle: Command Line Tools for Xcode, Version: 16.2, Size: 730KiB, Recommended: YES,
* Label: macOS Sequoia 15.6.1-24G90
\tTitle: macOS Sequoia 15.6.1, Version: 15.6.1, Size: 3067143KiB, Recommended: YES, Action: restart,
* Label: Command Line Tools for Xcode-16.10
\tTitle: Command Line Tools for Xcode, Version: 16.10, Size: 731KiB, Recommended: YES,
* Label: Command Line Tools for Xcode-16.4
\tTitle: Command Line Tools for Xcode, Version: 16.4, Size: 730KiB, Recommended: YES,
";

    /// The pre-10.15 spelling.
    const OLD: &str = "Software Update Tool

Finding available software
Software Update found the following new or updated software:
   * Command Line Tools (macOS Mojave version 10.14) for Xcode-10.1
\tCommand Line Tools (macOS Mojave version 10.14) for Xcode (10.1), 199140K [recommended]
   * Command Line Tools (macOS Mojave version 10.14) for Xcode-10.3
\tCommand Line Tools (macOS Mojave version 10.14) for Xcode (10.3), 199140K [recommended]
";

    const NONE: &str = "Software Update Tool

Finding available software
No new software available.
";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    fn row(provides: Vec<String>) -> Artifact {
        let mut a = crate::vendor::testkit::su_row();
        a.provides = provides;
        a
    }

    #[test]
    fn the_newest_label_under_the_prefix_is_picked_and_garbage_yields_none() {
        assert_eq!(
            pick_label(MODERN, "Command Line Tools for Xcode").as_deref(),
            Some("Command Line Tools for Xcode-16.10"),
            "16.10 is newer than 16.4 — numeric, not lexical"
        );
        assert_eq!(
            pick_label(OLD, "Command Line Tools").as_deref(),
            Some("Command Line Tools (macOS Mojave version 10.14) for Xcode-10.3")
        );
        assert_eq!(pick_label(NONE, "Command Line Tools for Xcode"), None);
        assert_eq!(pick_label("", "Command Line Tools for Xcode"), None);
        assert_eq!(
            pick_label(
                "garbage\n* Label: \n*\nCommand Line Tools for Xcode-1\n",
                "Command Line Tools for Xcode"
            ),
            None,
            "a label must be a bulleted entry"
        );
        assert_eq!(
            pick_label(MODERN, ""),
            None,
            "an empty prefix matches nothing"
        );
        // A crafted listing line: a label with a control byte INSIDE it (a tab, a DEL, a
        // carriage return mid-line) is never picked, however new its version reads; the
        // clean one beside it still is. (A trailing CR is a CRLF line ending, trimmed.)
        let crafted = "* Label: Command Line Tools for Xcode-99.0\t--foo\n\
                       * Label: Command Line Tools for Xcode-98.0\u{7f}\n\
                       * Label: Command Line Tools for Xcode-97.0\r-x\n\
                       * Label: Command Line Tools for Xcode-16.4\r\n";
        assert_eq!(
            pick_label(crafted, "Command Line Tools for Xcode").as_deref(),
            Some("Command Line Tools for Xcode-16.4")
        );
        assert_eq!(
            pick_label(
                "* Label: Command Line Tools for Xcode-99.0\t--foo\n",
                "Command Line Tools for Xcode"
            ),
            None
        );
        assert_eq!(
            pick_label(MODERN, "macOS Sequoia").as_deref(),
            Some("macOS Sequoia 15.6.1-24G90"),
            "a non-numeric tail sorts as the empty version but still resolves alone"
        );
        assert_eq!(
            version_key("Command Line Tools for Xcode-16.4"),
            vec![16, 4]
        );
        assert_eq!(version_key("no dash"), Vec::<u64>::new());
        assert_eq!(version_key("x-16.4beta"), vec![16]);
    }

    #[test]
    fn the_argv_builders_are_exact() {
        assert_eq!(list_argv(), s(&["/usr/sbin/softwareupdate", "-l"]));
        assert_eq!(
            install_argv("Command Line Tools for Xcode-16.4"),
            s(&[
                "/usr/sbin/softwareupdate",
                "-i",
                "Command Line Tools for Xcode-16.4"
            ])
        );
        assert_eq!(
            PLACEHOLDER,
            "/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress"
        );
    }

    /// The TTY path, end to end: the placeholder EXISTS while `-l` and `-i` run, the
    /// install argv is EXACTLY `sudo /usr/sbin/softwareupdate -i <newest label>`, the
    /// provides path proves the install, and the placeholder is gone afterwards.
    #[test]
    fn the_sudo_path_lists_picks_installs_proves_and_removes_the_placeholder() {
        let root = std::env::temp_dir().join(format!("atpkg-su-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let placeholder = root.join("in-progress");
        let git = root.join("CommandLineTools/usr/bin/git");
        let git_s = git.to_string_lossy().into_owned();
        let mut rec = Recorder::new(vec![ok(MODERN), ok("")]);
        let seen_placeholder = Rc::new(std::cell::Cell::new(0u32));
        let (ph, seen, created) = (placeholder.clone(), seen_placeholder.clone(), git.clone());
        rec.on_run = Some(Box::new(move |argv: &[String]| {
            if ph.is_file() {
                seen.set(seen.get() + 1);
            }
            if argv.iter().any(|a| a == "-i") {
                std::fs::create_dir_all(created.parent().unwrap()).unwrap();
                std::fs::write(&created, "git").unwrap();
            }
        }));
        let rec = Rc::new(rec);
        let art = row(vec![git_s]);
        let got = install(
            &*rec,
            Elevation::Sudo,
            &art,
            &placeholder,
            &root.join("prefix"),
            None,
        )
        .unwrap();
        assert_eq!(got, git);
        assert_eq!(
            seen_placeholder.get(),
            2,
            "the placeholder was there for -l AND -i"
        );
        assert!(!placeholder.exists(), "removed after success");
        let calls = rec.calls.borrow();
        assert_eq!(calls[0], (list_argv(), Io::Capture));
        assert_eq!(
            calls[1],
            (
                s(&[
                    "/usr/bin/sudo",
                    "/usr/sbin/softwareupdate",
                    "-i",
                    "Command Line Tools for Xcode-16.10"
                ]),
                Io::Inherit
            )
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every failure path removes the placeholder too, names its reason, and never
    /// proceeds past the step that failed; Deferred runs nothing and touches nothing.
    #[test]
    fn failures_remove_the_placeholder_and_deferred_touches_nothing() {
        let root = std::env::temp_dir().join(format!("atpkg-su-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let placeholder = root.join("in-progress");
        let art = row(vec![String::from("/nope/CommandLineTools/usr/bin/git")]);
        // No label at all.
        let rec = Rc::new(Recorder::new(vec![ok(NONE)]));
        let e = install(&*rec, Elevation::Sudo, &art, &placeholder, &root, None).unwrap_err();
        assert!(
            e.contains("lists no label starting with \"Command Line Tools for Xcode\""),
            "{e}"
        );
        assert_eq!(rec.argvs().len(), 1, "-i never ran");
        assert!(!placeholder.exists(), "removed after the listing failure");
        // -l failing.
        let rec = Rc::new(Recorder::new(vec![failed(
            1,
            "Can't connect to the Software Update server",
        )]));
        let e = install(&*rec, Elevation::Sudo, &art, &placeholder, &root, None).unwrap_err();
        assert!(
            e.contains("softwareupdate -l failed (exit 1): Can't connect"),
            "{e}"
        );
        assert!(!placeholder.exists());
        // -i failing.
        let rec = Rc::new(Recorder::new(vec![ok(MODERN), failed(1, "Installing: 0%")]));
        let e = install(&*rec, Elevation::Sudo, &art, &placeholder, &root, None).unwrap_err();
        assert!(
            e.starts_with(
                "softwareupdate -i \"Command Line Tools for Xcode-16.10\" failed (exit 1)"
            ),
            "{e}"
        );
        assert!(!placeholder.exists());
        // Success reported, nothing provided.
        let rec = Rc::new(Recorder::new(vec![ok(MODERN), ok("")]));
        let e = install(&*rec, Elevation::Sudo, &art, &placeholder, &root, None).unwrap_err();
        assert_eq!(
            e,
            "softwareupdate reported success, but none of the provides paths exists: \
             /nope/CommandLineTools/usr/bin/git"
        );
        assert!(!placeholder.exists());
        // Deferred: refused before the placeholder is even created.
        let rec = Rc::new(Recorder::new(vec![ok(MODERN)]));
        let e = install(&*rec, Elevation::Deferred, &art, &placeholder, &root, None).unwrap_err();
        assert!(e.contains("without an elevation policy"), "{e}");
        assert!(rec.argvs().is_empty());
        assert!(!placeholder.exists());
        // A placeholder that cannot be created is a refusal before anything runs.
        let rec = Rc::new(Recorder::new(vec![ok(MODERN)]));
        let e = install(
            &*rec,
            Elevation::Sudo,
            &art,
            &root.join("no/such/dir/marker"),
            &root,
            None,
        )
        .unwrap_err();
        assert!(
            e.contains("cannot create the softwareupdate placeholder"),
            "{e}"
        );
        assert!(rec.argvs().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
    /// macOS 26.6 spells the label with a SPACE before the version and repeats it after
    /// the dash (`Command Line Tools for Xcode 26.6-26.6`, observed on m27 2026-08-27);
    /// the picker keys on the dotted version after the last dash, so it still picks the
    /// newest.
    #[test]
    fn picks_the_newest_under_the_macos_26_spelling() {
        let listing = "Software Update Tool\n\nFinding available software\n\
            Software Update found the following new or updated software:\n\
            * Label: Command Line Tools for Xcode 26.5-26.5\n\
            \tTitle: Command Line Tools for Xcode, Version: 26.5, Size: 920416KiB, Recommended: YES,\n\
            * Label: Command Line Tools for Xcode 26.6-26.6\n\
            \tTitle: Command Line Tools for Xcode, Version: 26.6, Size: 921000KiB, Recommended: YES,\n";
        assert_eq!(
            pick_label(listing, "Command Line Tools for Xcode").as_deref(),
            Some("Command Line Tools for Xcode 26.6-26.6")
        );
    }
}
