// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The CANONICAL per-program states — one spelling each, used verbatim by `status.toml`,
//! the pass log, `atpkg doctor`, `atpkg which`, and the Packages rows
//! (`docs/DESIGN-which-copy-runs-2026-08-27.md` §2).
//!
//! A program is always in exactly one of these:
//!
//! * `managed <build> — pinned by index <N>` — atpkg installed it and keeps it current;
//! * `system: <path> — not managed by aterm` (`+ (managed copy retired <date>)`) — a
//!   binary the user already had satisfies the member; atpkg downloads nothing;
//! * `managed <build> — SHADOWED by <path>` — atpkg's copy is installed, but a binary
//!   earlier on `PATH` runs instead (a warning, never a fault, never "fixed");
//! * `extra — not installed (opt in: aterm pkg install <name>)` — listed, pinned, waiting
//!   for consent;
//! * `installed via <protocol>: <path>` — obtained through another protocol (`pkg`,
//!   `system-pm`) and proven present by one of the row's `provides` paths;
//! * `needs admin — run: aterm pkg install <name>` — the row needs elevation the
//!   unattended pass cannot supply;
//! * `unavailable on <target>: <hint>` — the pinned build carries no row for this target;
//! * `blocked by <dep>: <dep state>` — the program `requires` `<dep>`
//!   ([`crate::manifest::Program::requires`]) and `<dep>` is not installed, system-
//!   satisfied or installed through its protocol; the tail is the DEPENDENCY's own row,
//!   so the line says whose act unblocks it. Deferred, retried every pass, never a fault.
//!
//! Every constructor here is the ONLY place its spelling lives; the parsers beside them
//! (`system_path`, `managed_pin`, …) read the same words back so `doctor` and `which` can
//! never drift from what the pass wrote. Strings are built by hand (no `format!`) — see
//! `lib.rs` on the strict Trust gate.

use std::path::Path;

/// The head of a managed row: `managed <build> …`.
pub const MANAGED_PREFIX: &str = "managed ";
/// The head of a system-satisfied row: `system: <path> …`.
pub const SYSTEM_PREFIX: &str = "system: ";
/// The tail every system-satisfied row carries before the optional retirement note.
pub const SYSTEM_TAIL: &str = " — not managed by aterm";
/// The head of an extra that has not been opted in to.
pub const EXTRA_PREFIX: &str = "extra — not installed";
/// The head of a member obtained through another protocol.
pub const INSTALLED_VIA_PREFIX: &str = "installed via ";
/// The head of a member waiting on elevation.
pub const NEEDS_ADMIN_PREFIX: &str = "needs admin";
/// The head of a member the pinned build does not serve on this target.
pub const UNAVAILABLE_PREFIX: &str = "unavailable on ";
/// The hint an index row that names none falls back to.
pub const UNAVAILABLE_DEFAULT_HINT: &str = "no build is published for this target";
/// The head of a member waiting on one of its `requires`: `blocked by <dep>: <dep state>`.
/// Distinct from the `blocked:` FAULT prefix `doctor` matches (`blocked: no build for this
/// architecture`, the toolset-wide verdict): a space, not a colon, follows the word.
pub const BLOCKED_PREFIX: &str = "blocked by ";

/// `managed <build> — pinned by index <N>`.
#[must_use]
pub fn managed(build: u64, index_build: u64) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&crate::dec_u64(build));
    s.push_str(" — pinned by index ");
    s.push_str(&crate::dec_u64(index_build));
    s
}

/// `system: <path> — not managed by aterm`, plus ` (managed copy retired <date>)` when
/// `retired` names the day atpkg retired its own copy in favour of this one.
#[must_use]
pub fn system(path: &Path, retired: Option<&str>) -> String {
    let mut s = String::from(SYSTEM_PREFIX);
    s.push_str(&path.display().to_string());
    s.push_str(SYSTEM_TAIL);
    if let Some(date) = retired.filter(|d| !d.is_empty()) {
        s.push_str(" (managed copy retired ");
        s.push_str(date);
        s.push(')');
    }
    s
}

/// `managed <build> — SHADOWED by <path>`.
#[must_use]
pub fn shadowed(build: u64, path: &Path) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&crate::dec_u64(build));
    s.push_str(" — SHADOWED by ");
    s.push_str(&path.display().to_string());
    s
}

/// `extra — not installed (opt in: aterm pkg install <name>)`.
#[must_use]
pub fn extra_not_installed(name: &str) -> String {
    let mut s = String::from(EXTRA_PREFIX);
    s.push_str(" (opt in: aterm pkg install ");
    s.push_str(name);
    s.push(')');
    s
}

/// `installed via <protocol>: <path>`.
#[must_use]
pub fn installed_via(protocol: &str, path: &Path) -> String {
    let mut s = String::from(INSTALLED_VIA_PREFIX);
    s.push_str(protocol);
    s.push_str(": ");
    s.push_str(&path.display().to_string());
    s
}

/// `needs admin — run: aterm pkg install <name>`.
#[must_use]
pub fn needs_admin(name: &str) -> String {
    let mut s = String::from(NEEDS_ADMIN_PREFIX);
    s.push_str(" — run: aterm pkg install ");
    s.push_str(name);
    s
}

/// `unavailable on <target>: <hint>` — `hint` empty ⇒ [`UNAVAILABLE_DEFAULT_HINT`].
#[must_use]
pub fn unavailable(target: &str, hint: &str) -> String {
    let mut s = String::from(UNAVAILABLE_PREFIX);
    s.push_str(target);
    s.push_str(": ");
    s.push_str(if hint.is_empty() {
        UNAVAILABLE_DEFAULT_HINT
    } else {
        hint
    });
    s
}

/// `blocked by <dep>: <dep state>` — the program requires `dep` and `dep` is not yet
/// installed, system-satisfied or installed through its protocol. `dep_state` is the
/// dependency's OWN canonical row (`needs admin — run: aterm pkg install clt`, `extra —
/// not installed (opt in: aterm pkg install codex)`, `error: …`), quoted verbatim, so the
/// blocked row names the act that unblocks it. A per-program DEFERRED state, not a fault:
/// the next pass retries, and nothing downloads for a blocked program.
#[must_use]
pub fn blocked(dep: &str, dep_state: &str) -> String {
    let mut s = String::from(BLOCKED_PREFIX);
    s.push_str(dep);
    s.push_str(": ");
    s.push_str(dep_state);
    s
}

/// `Some((dep, dep_state))` for a `blocked by <dep>: <dep state>` state; `None` for every
/// other state. The inverse of [`blocked`] (a dependency name never contains `: `).
#[must_use]
pub fn blocked_by(state: &str) -> Option<(&str, &str)> {
    let rest = state.strip_prefix(BLOCKED_PREFIX)?;
    let (dep, dep_state) = rest.split_once(": ")?;
    if dep.is_empty() || dep_state.is_empty() {
        return None;
    }
    Some((dep, dep_state))
}

/// The `<path>` of a `system: <path> — not managed by aterm…` state, or `None` for any
/// other state. The inverse of [`system`]; the retirement note is dropped.
#[must_use]
pub fn system_path(state: &str) -> Option<&str> {
    let rest = state.strip_prefix(SYSTEM_PREFIX)?;
    let end = rest.find(SYSTEM_TAIL)?;
    Some(&rest[..end])
}

/// The `<date>` of a system state's ` (managed copy retired <date>)` note, if present.
#[must_use]
pub fn system_retired(state: &str) -> Option<&str> {
    let rest = state.strip_prefix(SYSTEM_PREFIX)?;
    let note = rest.rsplit_once(" (managed copy retired ")?.1;
    note.strip_suffix(')')
}

/// `Some((protocol, path))` for an `installed via <protocol>: <path>` state; `None` for
/// every other state. The inverse of [`installed_via`].
#[must_use]
pub fn installed_via_path(state: &str) -> Option<(&str, &str)> {
    let rest = state.strip_prefix(INSTALLED_VIA_PREFIX)?;
    let (protocol, path) = rest.split_once(": ")?;
    if protocol.is_empty() || path.is_empty() {
        return None;
    }
    Some((protocol, path))
}

/// `Some((build, index))` for a `managed <build> — pinned by index <N>` state; `None`
/// for a SHADOWED managed row and for every other state.
#[must_use]
pub fn managed_pin(state: &str) -> Option<(u64, u64)> {
    let rest = state.strip_prefix(MANAGED_PREFIX)?;
    let (build, tail) = rest.split_once(" — pinned by index ")?;
    Some((build.parse().ok()?, tail.parse().ok()?))
}

/// `Some((build, path))` for a `managed <build> — SHADOWED by <path>` state.
#[must_use]
pub fn shadowed_by(state: &str) -> Option<(u64, &str)> {
    let rest = state.strip_prefix(MANAGED_PREFIX)?;
    let (build, path) = rest.split_once(" — SHADOWED by ")?;
    Some((build.parse().ok()?, path))
}

/// Whether `state` is a managed row of either spelling (pinned or SHADOWED).
#[must_use]
pub fn is_managed(state: &str) -> bool {
    state.starts_with(MANAGED_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_canonical_spelling_is_exact() {
        assert_eq!(managed(6808, 41), "managed 6808 — pinned by index 41");
        assert_eq!(
            system(Path::new("/opt/homebrew/bin/gh"), None),
            "system: /opt/homebrew/bin/gh — not managed by aterm"
        );
        assert_eq!(
            system(Path::new("/opt/homebrew/bin/gh"), Some("2026-08-27")),
            "system: /opt/homebrew/bin/gh — not managed by aterm (managed copy retired 2026-08-27)"
        );
        assert_eq!(
            system(Path::new("/opt/homebrew/bin/gh"), Some("")),
            "system: /opt/homebrew/bin/gh — not managed by aterm",
            "an empty date is no note"
        );
        assert_eq!(
            shadowed(6808, Path::new("/Users//dev/.local/bin/trust")),
            "managed 6808 — SHADOWED by /Users//dev/.local/bin/trust"
        );
        assert_eq!(
            extra_not_installed("codex"),
            "extra — not installed (opt in: aterm pkg install codex)"
        );
        assert_eq!(
            installed_via("pkg", Path::new("/opt/homebrew/bin/brew")),
            "installed via pkg: /opt/homebrew/bin/brew"
        );
        assert_eq!(
            needs_admin("homebrew"),
            "needs admin — run: aterm pkg install homebrew"
        );
        assert_eq!(
            unavailable("x86_64-unknown-linux-gnu", "Emacs is a macOS-only member"),
            "unavailable on x86_64-unknown-linux-gnu: Emacs is a macOS-only member"
        );
        assert_eq!(
            unavailable("aarch64-pc-windows-msvc", ""),
            "unavailable on aarch64-pc-windows-msvc: no build is published for this target"
        );
        assert_eq!(
            blocked("clt", &needs_admin("clt")),
            "blocked by clt: needs admin — run: aterm pkg install clt"
        );
        assert_eq!(
            blocked("codex", &extra_not_installed("codex")),
            "blocked by codex: extra — not installed (opt in: aterm pkg install codex)"
        );
    }

    #[test]
    fn the_parsers_read_back_exactly_what_the_constructors_wrote() {
        let plain = system(Path::new("/opt/homebrew/bin/gh"), None);
        assert_eq!(system_path(&plain), Some("/opt/homebrew/bin/gh"));
        assert_eq!(system_retired(&plain), None);
        let noted = system(Path::new("/opt/homebrew/bin/gh"), Some("2026-08-27"));
        assert_eq!(system_path(&noted), Some("/opt/homebrew/bin/gh"));
        assert_eq!(system_retired(&noted), Some("2026-08-27"));
        // A path that itself contains the tail's words still parses at the FIRST tail.
        let odd = system(Path::new("/x — not managed by aterm/gh"), None);
        assert_eq!(system_path(&odd), Some("/x"));
        assert_eq!(managed_pin(&managed(6808, 41)), Some((6808, 41)));
        assert_eq!(managed_pin(&shadowed(6808, Path::new("/x/trust"))), None);
        assert_eq!(
            shadowed_by(&shadowed(6808, Path::new("/x/trust"))),
            Some((6808, "/x/trust"))
        );
        assert_eq!(shadowed_by(&managed(6808, 41)), None);
        assert_eq!(
            installed_via_path(&installed_via("pkg", Path::new("/opt/homebrew/bin/brew"))),
            Some(("pkg", "/opt/homebrew/bin/brew"))
        );
        assert_eq!(
            installed_via_path(&installed_via(
                "softwareupdate",
                Path::new("/Library/Developer/CommandLineTools/usr/bin/git")
            )),
            Some((
                "softwareupdate",
                "/Library/Developer/CommandLineTools/usr/bin/git"
            ))
        );
        assert_eq!(installed_via_path(&needs_admin("brew")), None);
        assert_eq!(installed_via_path("installed via pkg: "), None);
        // The blocked row quotes the dependency's row VERBATIM, colons and all, and the
        // parser gives it back whole.
        let b = blocked("clt", &needs_admin("clt"));
        assert_eq!(
            blocked_by(&b),
            Some(("clt", "needs admin — run: aterm pkg install clt"))
        );
        let nested = blocked("brew", &blocked("clt", "error: x: y"));
        assert_eq!(
            blocked_by(&nested),
            Some(("brew", "blocked by clt: error: x: y"))
        );
        assert_eq!(blocked_by(&needs_admin("brew")), None);
        assert_eq!(blocked_by("blocked by clt: "), None);
        assert_eq!(blocked_by("blocked: no build for this architecture"), None);
        assert!(is_managed(&managed(1, 1)) && is_managed(&shadowed(1, Path::new("/p"))));
        for other in [
            "active",
            "error: x",
            &extra_not_installed("codex"),
            &needs_admin("brew"),
            &unavailable("t", ""),
            &installed_via("pkg", Path::new("/p")),
            &blocked("clt", &needs_admin("clt")),
        ] {
            assert_eq!(system_path(other), None, "{other}");
            assert_eq!(managed_pin(other), None, "{other}");
            assert!(!is_managed(other), "{other}");
            if !other.starts_with(INSTALLED_VIA_PREFIX) {
                assert_eq!(installed_via_path(other), None, "{other}");
            }
        }
    }

    /// The states doctor treats as FAULTS are prefix-matched (`error:`, `unavailable:`,
    /// `blocked:`, `aborted:`, `tombstoned:`); none of the canonical spellings may start
    /// with one of those, or a normal row would read as a problem.
    #[test]
    fn no_canonical_state_reads_as_a_doctor_fault() {
        for s in [
            managed(1, 1),
            system(Path::new("/p"), Some("2026-01-01")),
            shadowed(1, Path::new("/p")),
            extra_not_installed("codex"),
            installed_via("pkg", Path::new("/p")),
            needs_admin("brew"),
            unavailable("t", "h"),
            blocked("clt", &needs_admin("clt")),
        ] {
            for fault in [
                "error:",
                "unavailable:",
                "blocked:",
                "aborted:",
                "tombstoned:",
            ] {
                assert!(!s.starts_with(fault), "{s} would read as a {fault} fault");
            }
        }
    }
}
