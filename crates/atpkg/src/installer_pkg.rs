// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `pkg` protocol's install lane (macOS): a Developer-ID-signed installer package —
//! Homebrew's `Homebrew.pkg` is the first member — applied by Apple's `installer` WITH
//! ELEVATION, proven by the row's `provides` paths, and never landing in the store.
//!
//! The flow ([`crate::flow`]) owns the download (the `https` lane: the signed `url`, the
//! signed `size` as the exact cap, the signed `sha256` over the bytes) and the elevation
//! decision; this module owns everything between a downloaded file and a proven install:
//!
//! 1. `pkgutil --check-signature <file>` ([`check_signature_argv`]), parsed
//!    ([`parse_check_signature`]) and ADMITTED ([`admit_signature`]) only when the
//!    package is signed by a developer certificate Apple issued for distribution AND the
//!    leaf certificate is `Developer ID Installer: <name> (<team>)` with `<team>` equal to
//!    the signed row's `signer_team`. An unsigned package, an untrusted one, or one from
//!    any other team is refused BEFORE the installer runs — the sha256 pinned the bytes,
//!    the team pins who made them.
//! 2. `installer -pkg <file> -target /` ([`install_argv`]) under the caller's elevation
//!    ([`crate::elevate::elevated_argv`]: `sudo` on the terminal door, `osascript` on the
//!    GUI door). The unattended pass never reaches here — it defers upstream.
//! 3. the `provides` probe ([`crate::elevate::first_provided`]): the first path that
//!    exists is the `installed via pkg: <path>` state's path; none is an error naming
//!    them all.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::elevate::{Elevation, Io, Runner};
use crate::manifest::Artifact;

/// The protocol's spelling, as the canonical state prints it.
pub const PROTOCOL: &str = "pkg";
/// Apple's package signature inspector.
pub const PKGUTIL: &str = "/usr/sbin/pkgutil";
/// Apple's installer.
pub const INSTALLER: &str = "/usr/sbin/installer";

/// `pkgutil --check-signature <pkg>`.
#[must_use]
pub fn check_signature_argv(pkg: &Path) -> Vec<String> {
    vec![
        String::from(PKGUTIL),
        String::from("--check-signature"),
        pkg.to_string_lossy().into_owned(),
    ]
}

/// `installer -pkg <pkg> -target /` — the root volume, exactly as Homebrew's
/// `Distribution.xml` (`rootVolumeOnly="true"`) requires.
#[must_use]
pub fn install_argv(pkg: &Path) -> Vec<String> {
    vec![
        String::from(INSTALLER),
        String::from("-pkg"),
        pkg.to_string_lossy().into_owned(),
        String::from("-target"),
        String::from("/"),
    ]
}

/// What `pkgutil --check-signature` said, in the three facts admission reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signature {
    /// The `Status:` line's text (`signed by a developer certificate issued by Apple for
    /// distribution`, `no signature`, `signed by untrusted certificate`, …).
    pub status: String,
    /// `(name, team)` when the certificate chain's FIRST entry — the leaf — is a
    /// `Developer ID Installer: <name> (<team>)` certificate.
    pub installer_leaf: Option<(String, String)>,
    /// Whether the `Notarization:` line says the notary service trusts it.
    pub notarized: bool,
}

/// The `Status:` text of a package signed by a Developer ID certificate — the EXACT
/// line, not a prefix of it: `pkgutil` has other "signed by …" spellings (an untrusted
/// certificate, an Apple-internal signature), and none of them is this one.
const DEVELOPER_SIGNED: &str = "signed by a developer certificate issued by Apple for distribution";

/// Parse `pkgutil --check-signature`'s report. Tolerant of indentation and of the
/// fingerprint blocks between chain entries; only the leaf (`1.`) certificate counts.
#[must_use]
pub fn parse_check_signature(out: &str) -> Signature {
    let mut sig = Signature::default();
    let mut in_chain = false;
    let mut leaf_seen = false;
    for raw in out.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Status:") {
            sig.status = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("Notarization:") {
            sig.notarized = rest.contains("trusted");
            continue;
        }
        if line.starts_with("Certificate Chain:") {
            in_chain = true;
            continue;
        }
        if !in_chain || leaf_seen {
            continue;
        }
        // The first numbered entry after the chain head is the leaf.
        let Some((num, entry)) = line.split_once(". ") else {
            continue;
        };
        if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        leaf_seen = true;
        if let Some(subject) = entry.trim().strip_prefix("Developer ID Installer:") {
            let subject = subject.trim();
            if let Some((name, team)) = subject.rsplit_once(" (")
                && let Some(team) = team.strip_suffix(')')
            {
                sig.installer_leaf = Some((name.trim().to_string(), team.to_string()));
            }
        }
    }
    sig
}

/// Admit `sig` against the signed row's `signer_team`, or say exactly why not. `Ok`
/// carries the leaf's display (`Developer ID Installer: Patrick Linnane (927JGANW46)`).
///
/// # Errors
/// Not developer-signed (unsigned, untrusted, an Apple-internal signature), no
/// `Developer ID Installer` leaf, or a leaf from another team.
pub fn admit_signature(sig: &Signature, signer_team: &str) -> Result<String, String> {
    if sig.status != DEVELOPER_SIGNED {
        let mut m = String::from("refusing to install: pkgutil reports \"");
        m.push_str(if sig.status.is_empty() {
            "(no Status line)"
        } else {
            &sig.status
        });
        m.push_str(
            "\" — only a package signed by a Developer ID Installer certificate is \
                    installed",
        );
        return Err(m);
    }
    let Some((name, team)) = &sig.installer_leaf else {
        return Err(String::from(
            "refusing to install: the certificate chain's leaf is not a Developer ID \
             Installer certificate",
        ));
    };
    if team != signer_team {
        let mut m = String::from("refusing to install: signed by Developer ID Installer team ");
        m.push_str(team);
        m.push_str(" (");
        m.push_str(name);
        m.push_str("), not the pinned signer_team ");
        m.push_str(signer_team);
        return Err(m);
    }
    let mut d = String::from("Developer ID Installer: ");
    d.push_str(name);
    d.push_str(" (");
    d.push_str(team);
    d.push(')');
    Ok(d)
}

/// Run `pkgutil --check-signature` over `pkg` through `runner` and admit the result.
///
/// # Errors
/// `pkgutil` could not run or failed, or [`admit_signature`] refused.
pub fn verify_signature(
    runner: &dyn Runner,
    pkg: &Path,
    signer_team: &str,
) -> Result<String, String> {
    let ran = runner.run(&check_signature_argv(pkg), Io::Capture)?;
    if !ran.success() {
        let mut m = String::from("pkgutil --check-signature failed");
        if let Some(code) = ran.code {
            m.push_str(" (exit ");
            m.push_str(&crate::dec_u64(u64::from(code.unsigned_abs())));
            m.push(')');
        }
        let tail = ran.stderr.trim();
        if !tail.is_empty() {
            m.push_str(": ");
            m.push_str(tail.lines().last().unwrap_or(tail));
        }
        return Err(m);
    }
    admit_signature(
        &parse_check_signature(strip_package_header(&ran.stdout, pkg)),
        signer_team,
    )
}

/// `pkgutil --check-signature` opens its report with `Package "<path as given>":` —
/// the path echoed VERBATIM. Consume exactly that header (the path we passed, quoted)
/// before the parser reads a line, so nothing the file's NAME says can be mistaken for
/// what the signature says. A report without the expected header is parsed as is.
fn strip_package_header<'a>(out: &'a str, pkg: &Path) -> &'a str {
    // pkgutil echoes the package either as the path it was handed (older releases)
    // or as the bare file name (macOS 26.6 prints `Package "Homebrew.pkg":`); both
    // spellings are the header and nothing else is.
    let full = pkg.to_string_lossy();
    let base = pkg
        .file_name()
        .map(|f| f.to_string_lossy())
        .unwrap_or_else(|| full.clone());
    for name in [full.as_ref(), base.as_ref()] {
        let head = format!("Package \"{name}\":");
        if let Some(rest) = out.strip_prefix(&head) {
            return rest;
        }
    }
    out
}

/// Whether `pkg` is a path the lane may hand to `pkgutil`/`installer`: no control byte
/// (a newline in the path would let the name forge the report's chain — see
/// [`strip_package_header`]; the row's `asset` was refused for the same reason at
/// admission, this is the lane's own belt).
fn pkg_path_ok(pkg: &Path) -> bool {
    !pkg.to_string_lossy().bytes().any(|b| b < b' ' || b == 0x7f)
}

/// The lane, from a DOWNLOADED and sha256-verified `pkg` to a proven install: verify the
/// signature, run the installer under `elevation`, probe `provides`. Never deletes the
/// file (the flow reclaims it on every path) and never decides elevation (the flow
/// already deferred when the policy said so — a [`Elevation::Deferred`] here is a
/// programming error and is refused, not silently run unelevated).
///
/// # Errors
/// A signature refusal (the installer is never run), a spawn/exit failure of the
/// installer, or an install that left none of the `provides` paths behind.
pub fn install(
    runner: &dyn Runner,
    elevation: Elevation,
    artifact: &Artifact,
    pkg: &Path,
    prefix: &Path,
    path_var: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let Some(argv) = crate::elevate::elevated_argv(elevation, &install_argv(pkg)) else {
        return Err(String::from(
            "the pkg lane was entered without an elevation policy — nothing was run",
        ));
    };
    if !pkg_path_ok(pkg) {
        let mut m = String::from(
            "refusing to install: the package path carries a control \
                                  byte (pkgutil would echo it into its report): ",
        );
        m.push_str(&pkg.to_string_lossy());
        return Err(m);
    }
    verify_signature(runner, pkg, &artifact.signer_team)?;
    let ran = runner.run(&argv, Io::Inherit)?;
    if !ran.success() {
        let mut m = String::from("installer failed");
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
pub(crate) mod fixtures {
    //! `pkgutil --check-signature` reports, for the lane's tests and the flow's.

    /// Over the real Homebrew 6.0.20 package, verbatim (2026-08-27, fingerprints elided
    /// to their first bytes).
    pub const GOOD: &str = r#"Package "Homebrew.pkg":
   Status: signed by a developer certificate issued by Apple for distribution
   Notarization: trusted by the Apple notary service
   Signed with a trusted timestamp on: 2026-08-27 10:22:15 +0000
   Certificate Chain:
    1. Developer ID Installer: Patrick Linnane (927JGANW46)
       Expires: 2027-02-01 22:12:15 +0000
       SHA256 Fingerprint:
           68 0F F3 72 93 85 F6 F6 35 51 1E E9 AE 9B C6 DE D5 1A BB 82 F4 44
           B3 F1 09 22 E9 60 F8 B0 02 9C
       ------------------------------------------------------------------------
    2. Developer ID Certification Authority
       Expires: 2027-02-01 22:12:15 +0000
       SHA256 Fingerprint:
           7A FC 9D 01 A6 2F 03 A2 DE 96 37 93 6D 4A FE 68 09 0D 2D E1 8D 03
           F2 9C 88 CF B0 B1 BA 63 58 7F
       ------------------------------------------------------------------------
    3. Apple Root CA
       Expires: 2035-02-09 21:40:36 +0000
"#;

    /// Signed, notarized — by another team.
    pub const WRONG_TEAM: &str = r#"Package "Other.pkg":
   Status: signed by a developer certificate issued by Apple for distribution
   Notarization: trusted by the Apple notary service
   Certificate Chain:
    1. Developer ID Installer: Somebody Else (ABCDE12345)
       Expires: 2027-02-01 22:12:15 +0000
    2. Developer ID Certification Authority
    3. Apple Root CA
"#;

    /// No signature at all.
    pub const UNSIGNED: &str = r#"Package "unsigned.pkg":
   Status: no signature
"#;

    /// A self-signed certificate wearing the pinned team's name.
    pub const UNTRUSTED: &str = r#"Package "self.pkg":
   Status: signed by untrusted certificate
   Certificate Chain:
    1. Developer ID Installer: Patrick Linnane (927JGANW46)
"#;

    /// A chain whose LEAF is not an installer certificate, with the pinned team further
    /// down: the leaf is what signs, so this must not admit.
    pub const LEAF_NOT_INSTALLER: &str = r#"Package "odd.pkg":
   Status: signed by a developer certificate issued by Apple for distribution
   Certificate Chain:
    1. Developer ID Application: Patrick Linnane (927JGANW46)
    2. Developer ID Installer: Patrick Linnane (927JGANW46)
"#;
}

#[cfg(test)]
mod tests {
    use super::fixtures::{GOOD, LEAF_NOT_INSTALLER, UNSIGNED, UNTRUSTED, WRONG_TEAM};
    use super::*;
    use crate::elevate::testkit::{Recorder, failed, ok};
    use std::rc::Rc;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    fn row(provides: Vec<String>) -> Artifact {
        let mut a = crate::vendor::testkit::pkg_row();
        a.provides = provides;
        a
    }

    #[test]
    fn the_signature_report_parses_and_admits_only_the_pinned_installer_team() {
        let good = parse_check_signature(GOOD);
        assert_eq!(
            good.status,
            "signed by a developer certificate issued by Apple for distribution"
        );
        assert!(good.notarized);
        assert_eq!(
            good.installer_leaf,
            Some(("Patrick Linnane".to_string(), "927JGANW46".to_string()))
        );
        assert_eq!(
            admit_signature(&good, "927JGANW46").unwrap(),
            "Developer ID Installer: Patrick Linnane (927JGANW46)"
        );
        let wrong = admit_signature(&good, "ABCDE12345").unwrap_err();
        assert!(wrong.contains("team 927JGANW46"), "{wrong}");
        assert!(
            wrong.contains("not the pinned signer_team ABCDE12345"),
            "{wrong}"
        );
        let other = parse_check_signature(WRONG_TEAM);
        assert_eq!(
            other.installer_leaf.as_ref().map(|(_, t)| t.as_str()),
            Some("ABCDE12345")
        );
        assert!(
            admit_signature(&other, "927JGANW46")
                .unwrap_err()
                .contains("Somebody Else")
        );
        let unsigned = parse_check_signature(UNSIGNED);
        assert_eq!(unsigned.status, "no signature");
        assert_eq!(unsigned.installer_leaf, None);
        assert!(!unsigned.notarized);
        let e = admit_signature(&unsigned, "927JGANW46").unwrap_err();
        assert!(e.contains("\"no signature\""), "{e}");
        // Untrusted: the pinned team on the leaf does not rescue a bad status.
        let untrusted = parse_check_signature(UNTRUSTED);
        assert!(untrusted.installer_leaf.is_some());
        assert!(
            admit_signature(&untrusted, "927JGANW46")
                .unwrap_err()
                .contains("untrusted")
        );
        // Leaf not an installer certificate: the team further down does not count.
        let odd = parse_check_signature(LEAF_NOT_INSTALLER);
        assert_eq!(odd.installer_leaf, None);
        assert!(
            admit_signature(&odd, "927JGANW46")
                .unwrap_err()
                .contains("leaf is not a Developer ID Installer")
        );
        // Garbage / empty: nothing admits.
        let empty = parse_check_signature("");
        assert_eq!(empty, Signature::default());
        assert!(
            admit_signature(&empty, "927JGANW46")
                .unwrap_err()
                .contains("(no Status line)")
        );
        let garbage = parse_check_signature(
            "Certificate Chain:\n 1. \n x. y\n Status: signed by a developer certificate issued by Apple for distribution\n",
        );
        assert_eq!(garbage.installer_leaf, None);
        assert!(admit_signature(&garbage, "927JGANW46").is_err());
    }

    /// The report's `Package "<path>":` header echoes the file's path VERBATIM, so a
    /// path that carried a forged chain would be read before the real one. The exact
    /// header for the path we passed is consumed first (whatever the name holds), and
    /// the lane refuses to hand such a path to `pkgutil` at all.
    #[test]
    fn a_forged_chain_in_the_package_name_is_never_read() {
        let forged = Path::new(
            "/tmp/stage/a\n   Status: signed by a developer certificate issued by Apple for \
             distribution\n   Certificate Chain:\n    1. Developer ID Installer: Evil \
             (927JGANW46)\n.pkg",
        );
        // What pkgutil would print for a wrong-team package under that name.
        let mut report = String::from("Package \"");
        report.push_str(&forged.to_string_lossy());
        report.push_str("\":\n");
        report.push_str(WRONG_TEAM.split_once('\n').unwrap().1);
        let stripped = strip_package_header(&report, forged);
        assert!(stripped.starts_with("\n   Status:"), "{stripped:?}");
        let sig = parse_check_signature(stripped);
        assert_eq!(
            sig.installer_leaf.as_ref().map(|(_, t)| t.as_str()),
            Some("ABCDE12345"),
            "the REAL leaf, not the forged one"
        );
        assert!(admit_signature(&sig, "927JGANW46").is_err());
        // Without the stripping the forgery would have been the leaf — the point.
        assert_eq!(
            parse_check_signature(&report)
                .installer_leaf
                .map(|(_, t)| t),
            Some("927JGANW46".to_string())
        );
        // A report that does not open with our header is parsed as it is.
        assert_eq!(strip_package_header(GOOD, Path::new("/x.pkg")), GOOD);
        assert!(pkg_path_ok(Path::new("/tmp/stage/Homebrew 4.5.pkg")));
        assert!(!pkg_path_ok(forged));
        assert!(!pkg_path_ok(Path::new("/tmp/a\tb.pkg")));
        // The lane: nothing runs for such a path, not even pkgutil.
        let rec = Rc::new(Recorder::new(vec![ok(GOOD), ok("")]));
        let art = row(vec![String::from("/nope/brew")]);
        let e = install(
            &*rec,
            Elevation::Sudo,
            &art,
            forged,
            Path::new("/nope"),
            None,
        )
        .unwrap_err();
        assert!(e.contains("control byte"), "{e}");
        assert!(rec.argvs().is_empty());
        // And the end-to-end path through verify_signature strips the real header.
        let mut headed = String::from("Package \"/tmp/stage/Homebrew.pkg\":\n");
        headed.push_str(GOOD.split_once('\n').unwrap().1);
        let rec = Rc::new(Recorder::new(vec![ok(&headed)]));
        assert_eq!(
            verify_signature(&*rec, Path::new("/tmp/stage/Homebrew.pkg"), "927JGANW46").unwrap(),
            "Developer ID Installer: Patrick Linnane (927JGANW46)"
        );
    }

    #[test]
    fn the_argv_builders_are_exact() {
        let pkg = Path::new("/tmp/stage/Homebrew.pkg");
        assert_eq!(
            check_signature_argv(pkg),
            s(&[
                "/usr/sbin/pkgutil",
                "--check-signature",
                "/tmp/stage/Homebrew.pkg"
            ])
        );
        assert_eq!(
            install_argv(pkg),
            s(&[
                "/usr/sbin/installer",
                "-pkg",
                "/tmp/stage/Homebrew.pkg",
                "-target",
                "/"
            ])
        );
    }

    /// The TTY path: pkgutil first (captured), then EXACTLY `sudo /usr/sbin/installer
    /// -pkg <file> -target /` (inherited), then the provides probe — which the fake
    /// installer satisfies by creating the path.
    #[test]
    fn the_sudo_path_runs_pkgutil_then_the_exact_installer_argv_then_proves_the_install() {
        let root = std::env::temp_dir().join(format!("atpkg-pkglane-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let pkg = root.join("Homebrew.pkg");
        std::fs::write(&pkg, b"not really a pkg").unwrap();
        let brew = root.join("opt/homebrew/bin/brew");
        let brew_s = brew.to_string_lossy().into_owned();
        let mut rec = Recorder::new(vec![ok(GOOD), ok("")]);
        let created = brew.clone();
        rec.on_run = Some(Box::new(move |argv: &[String]| {
            if argv.iter().any(|a| a == "/usr/sbin/installer") {
                std::fs::create_dir_all(created.parent().unwrap()).unwrap();
                std::fs::write(&created, "brew").unwrap();
            }
        }));
        let rec = Rc::new(rec);
        let art = row(vec![String::from("/nope/brew"), brew_s.clone()]);
        let got = install(
            &*rec,
            Elevation::Sudo,
            &art,
            &pkg,
            &root.join("prefix"),
            None,
        )
        .unwrap();
        assert_eq!(got, brew);
        let calls = rec.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            (check_signature_argv(&pkg), Io::Capture),
            "signature check first, captured"
        );
        assert_eq!(
            calls[1],
            (
                s(&[
                    "/usr/bin/sudo",
                    "/usr/sbin/installer",
                    "-pkg",
                    &pkg.to_string_lossy(),
                    "-target",
                    "/"
                ]),
                Io::Inherit
            ),
            "the elevated installer, inheriting the terminal"
        );
        assert!(
            pkg.exists(),
            "the lane never deletes the file — the flow does"
        );
        // The osascript door wraps the same installer argv.
        let rec2 = Rc::new(Recorder::new(vec![ok(GOOD), ok("")]));
        let got2 = install(
            &*rec2,
            Elevation::Osascript,
            &art,
            &pkg,
            &root.join("prefix"),
            None,
        )
        .unwrap();
        assert_eq!(got2, brew);
        let argvs = rec2.argvs();
        assert_eq!(argvs[1][0], "/usr/bin/osascript");
        assert!(
            argvs[1][2].contains("'/usr/sbin/installer' '-pkg'"),
            "{}",
            argvs[1][2]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Refusals, each BEFORE the installer runs where it can be: a wrong team and an
    /// unsigned package never reach `installer`; an installer failure is an error; an
    /// install that leaves no `provides` path is an error NAMING the paths; and the lane
    /// refuses to run without an elevation policy at all.
    #[test]
    fn refusals_name_the_reason_and_never_run_the_installer_early() {
        let root = std::env::temp_dir().join(format!("atpkg-pkglane-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let pkg = root.join("x.pkg");
        std::fs::write(&pkg, b"x").unwrap();
        let art = row(vec![String::from("/nope/brew"), String::from("/also/nope")]);
        for (report, needle) in [
            (WRONG_TEAM, "not the pinned signer_team 927JGANW46"),
            (UNSIGNED, "no signature"),
            (UNTRUSTED, "untrusted"),
        ] {
            let rec = Rc::new(Recorder::new(vec![ok(report)]));
            let e = install(&*rec, Elevation::Sudo, &art, &pkg, &root, None).unwrap_err();
            assert!(e.contains(needle), "{e}");
            assert_eq!(
                rec.argvs().len(),
                1,
                "pkgutil only — the installer never ran"
            );
            assert_eq!(rec.argvs()[0][0], "/usr/sbin/pkgutil");
        }
        // pkgutil itself failing is a refusal too.
        let rec = Rc::new(Recorder::new(vec![failed(1, "could not open package")]));
        let e = install(&*rec, Elevation::Sudo, &art, &pkg, &root, None).unwrap_err();
        assert!(
            e.contains("pkgutil --check-signature failed (exit 1): could not open package"),
            "{e}"
        );
        // The installer failing.
        let rec = Rc::new(Recorder::new(vec![
            ok(GOOD),
            failed(1, "The Installer encountered an error"),
        ]));
        let e = install(&*rec, Elevation::Sudo, &art, &pkg, &root, None).unwrap_err();
        assert!(e.starts_with("installer failed (exit 1)"), "{e}");
        // Success reported, nothing provided: the error names every path.
        let rec = Rc::new(Recorder::new(vec![ok(GOOD), ok("")]));
        let e = install(&*rec, Elevation::Sudo, &art, &pkg, &root, None).unwrap_err();
        assert_eq!(
            e,
            "pkg reported success, but none of the provides paths exists: /nope/brew, /also/nope"
        );
        // No elevation policy: nothing runs at all.
        let rec = Rc::new(Recorder::new(vec![ok(GOOD)]));
        let e = install(&*rec, Elevation::Deferred, &art, &pkg, &root, None).unwrap_err();
        assert!(e.contains("without an elevation policy"), "{e}");
        assert!(rec.argvs().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
    /// macOS 26.6's pkgutil prints the header with the bare file name, not the path it
    /// was handed (observed on m27 2026-08-27): both spellings are the header.
    #[test]
    fn the_header_is_stripped_whether_pkgutil_echoes_the_path_or_its_basename() {
        let body =
            "\n   Status: signed by a developer certificate issued by Apple for distribution";
        let by_path = format!("Package \"/tmp/stage/Homebrew.pkg\":{body}");
        let by_name = format!("Package \"Homebrew.pkg\":{body}");
        let pkg = Path::new("/tmp/stage/Homebrew.pkg");
        assert_eq!(strip_package_header(&by_path, pkg), body);
        assert_eq!(strip_package_header(&by_name, pkg), body);
        assert_eq!(strip_package_header(body, pkg), body);
    }
}
