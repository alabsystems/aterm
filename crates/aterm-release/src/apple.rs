// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Apple half of `targo --unverified ship provision`: this machine gets its own
//! Developer ID Application identity, and no private key crosses a machine boundary.
//!
//! Apple creates Developer ID certificates only through the developer website or Xcode: the
//! App Store Connect API refuses every `DEVELOPER_ID_*` type, a cloud-managed identity keeps
//! its key on Apple's servers where `codesign` cannot use it, and the website's private
//! endpoint needs a 2FA session and is forbidden by the site terms. So the command prints
//! the browser errand and waits, matching the arriving certificate by public key: one
//! invocation from an unprovisioned machine to ready.
//!
//! There is no progress marker: every fact is derived from disk and the keychain on every
//! pass, so Ctrl-C loses nothing and a re-run resumes where the world is. Without a
//! terminal it reports the errand and returns.
//!
//! Two rules:
//! 1. A slot is permanent: a team gets five Developer ID Application certificates, and
//!    revocation stops already-shipped apps from launching. So it asks before spending one,
//!    never regenerates over a CSR already out for signature, and never revokes.
//!    `--check` never reaches that question.
//! 2. A green verdict is demonstrated: `find-identity` proves a certificate is paired with
//!    a key, not that `codesign` can use the key unattended (the partition-list ACL
//!    decides). So every run that would report ready first signs something disposable,
//!    and the verdict carries the certificate's expiry.

#![cfg(unix)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aterm_update_core::pins;

use crate::publish::step;

/// The label this check prints under, in the audit too: one subject, one label.
pub(crate) const APPLE_LABEL: &str = "apple id";

/// Where this machine's own Apple key material lives: beside `~/.aterm/machine.key`, and
/// outside the repo so no `git clean` can sweep a key whose slot is spent.
fn apple_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".aterm").join("apple"))
}

fn key_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("devid-{id}.key"))
}

fn csr_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("devid-{id}.certSigningRequest"))
}

/// A certificate file and the encoding the matcher proved it parses as (DER or PEM), so
/// the installer cannot read it differently.
struct MatchedCert {
    path: PathBuf,
    form: &'static str,
}

/// What is observably true right now, re-derived every run (no stored flags).
pub(crate) struct Observed {
    /// 40-hex SHA-1 of each VALID `Developer ID Application` identity for our team.
    pub identities: Vec<String>,
    /// Set when `security find-identity` could not run: "could not look" is not "nothing
    /// there", and only the latter may lead to spending a slot.
    pub lookup_failed: Option<String>,
    /// Our team's Developer ID certificates present but not valid (expired, or without
    /// their private key), so an expired one is not mistaken for none.
    pub invalid_present: usize,
    pub key: Option<PathBuf>,
    pub csr: Option<PathBuf>,
}

impl Observed {
    pub fn look(id: &str) -> Observed {
        let dir = apple_dir();
        let (key, csr) = match &dir {
            Some(d) => {
                let k = key_path(d, id);
                let c = csr_path(d, id);
                (k.is_file().then_some(k), c.is_file().then_some(c))
            }
            None => (None, None),
        };
        let (identities, lookup_failed) = valid_identities();
        Observed {
            identities,
            lookup_failed,
            invalid_present: invalid_identity_count(),
            key,
            csr,
        }
    }
}

/// `security find-identity -v` filtered to our team, plus the reason we could not look.
fn valid_identities() -> (Vec<String>, Option<String>) {
    match Command::new("/usr/bin/security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
    {
        // One definition of "which lines count", shared with the audit that reports them.
        Ok(out) => (
            crate::provision::devid_identities(
                &String::from_utf8_lossy(&out.stdout),
                pins::APPLE_TEAM_ID,
            ),
            None,
        ),
        Err(e) => (Vec::new(), Some(format!("could not run `security`: {e}"))),
    }
}

/// How many of our team's Developer ID identities the keychain holds but will not vouch
/// for: all identities (no `-v`) minus the valid ones.
fn invalid_identity_count() -> usize {
    let all = Command::new("/usr/bin/security")
        .args(["find-identity", "-p", "codesigning"])
        .output()
        .map(|o| {
            crate::provision::devid_identities(
                &String::from_utf8_lossy(&o.stdout),
                pins::APPLE_TEAM_ID,
            )
            .len()
        })
        .unwrap_or(0);
    all.saturating_sub(valid_identities().0.len())
}

/// What the Apple step decided.
pub(crate) enum Outcome {
    /// An identity is installed and proved usable. `ids` holds only 40-hex SHA-1s;
    /// anything else rides in `note`.
    Ready {
        ids: Vec<String>,
        note: Option<String>,
    },
    /// Progress, waiting on the operator: not a fault to fix, which is `Blocked`.
    Waiting { what: String, next: String },
    /// `Waiting`'s shape for the read-only mode: nothing was attempted, only what a real
    /// run would do.
    Todo { what: String, next: String },
    /// Something is wrong and the tool will not proceed past it.
    Blocked { what: String, fix: String },
    /// Not a macOS host; the verdict says "skipped".
    Skipped(String),
}

/// The driver: it reads the world, and only one branch creates anything.
///
/// `cert_dir` is the one folder the operator named for the browser errand (`--cert-dir`):
/// the request is copied there and the certificate looked for there. `None` touches
/// nothing outside `~/.aterm/apple`, not even `~/Downloads`: reading a folder macOS guards
/// from an aterm shell raises a consent dialog for aterm (`atpkg::protected`), so naming
/// the folder is the consent.
pub(crate) fn acquire(id: &str, may_change: bool, cert_dir: Option<&Path>) -> Outcome {
    if !cfg!(target_os = "macos") {
        return Outcome::Skipped("cuts run on macOS (Tier APPLE); no keychain on this host".into());
    }
    let Some(dir) = apple_dir() else {
        return Outcome::Blocked {
            what: "HOME is not set, so ~/.aterm/apple has no location".into(),
            fix: "run provision as the user that will cut releases".into(),
        };
    };
    let mut seen = Observed::look(id);

    // "Could not look" reaches neither the slot-spending branch nor the read-only one,
    // which would report "no identity" about a keychain it never opened.
    if let Some(why) = seen.lookup_failed.as_deref() {
        return Outcome::Blocked {
            what: format!(
                "cannot read the keychain, so this machine's Apple state is unknown: {why}"
            ),
            fix: "install the Xcode command-line tools (`xcode-select --install`)".into(),
        };
    }

    // `--check` writes nothing: generating a CSR spends a permanent slot and writes a
    // private key, and importing mutates the keychain.
    if !may_change {
        return match (seen.identities.is_empty(), seen.key.is_some()) {
            (false, _) => verdict(seen.identities),
            // Only a key on disk is observable; whether its request was uploaded is
            // Apple's state.
            (true, true) => Outcome::Todo {
                what: format!("'{id}' holds a request key and no installed certificate"),
                next: "re-run without --check to import the certificate once you have \
                       downloaded it"
                    .into(),
            },
            (true, false) => Outcome::Todo {
                what: format!(
                    "no Developer ID Application identity for team {}",
                    pins::APPLE_TEAM_ID
                ),
                next: format!(
                    "re-run without --check: it will ask before spending one of five \
                     permanent certificate slots, then generate '{id}'s request"
                ),
            },
        };
    }

    // A leaf whose issuing intermediate is missing is invisible to `find-identity -v` (see
    // `install_issuer_intermediate`), so it arrives as "nothing installed" and would be
    // routed toward spending a second slot. `install()` repairs the chain on import; this
    // repairs a machine that already held the certificate and lost the intermediate (a new
    // login keychain, a migration, a cleanup). `invalid_present` separates the two.
    if seen.identities.is_empty()
        && seen.invalid_present > 0
        && let Some(note) = repair_installed_chain()
    {
        step(APPLE_LABEL, &note);
        // `look` sampled before the repair; re-read so the now-valid identity is seen.
        seen = Observed::look(id);
    }

    // An identity is installed: prove it can sign, every run.
    if !seen.identities.is_empty() {
        // The spent request is public and finished, so it is removed silently. The
        // private key is never touched automatically: its slot cannot be reclaimed.
        if let Some(csr) = &seen.csr
            && paired_with_installed(&seen.key, &seen.identities[0])
        {
            let _ = std::fs::remove_file(csr);
        }
        return verdict(seen.identities);
    }

    // K && C, no identity — install if Apple has answered, otherwise wait for it.
    if let (Some(key), Some(csr)) = (&seen.key, &seen.csr) {
        return await_then_install(key, csr, id, &dir, seen.invalid_present, cert_dir);
    }

    // A CSR with no key is unusable: a certificate issued against it could never sign,
    // and the slot it spent does not come back.
    if seen.key.is_none() && seen.csr.is_some() {
        let csr = csr_path(&dir, id);
        // Zero is the ordinary abandoned request. Above zero, the keychain holds a Developer
        // ID certificate it cannot vouch for, which may be the one issued against the lost
        // key.
        let fix = if seen.invalid_present > 0 {
            format!(
                "delete {} and re-run to mint a fresh key and request. The {} invalid \
                 Developer ID certificate{} in this keychain may include the one issued \
                 from it — that slot is spent, and revoking it would stop already-shipped \
                 apps from launching",
                csr.display(),
                seen.invalid_present,
                plural(seen.invalid_present),
            )
        } else {
            format!(
                "delete {} and re-run to mint a fresh key and request",
                csr.display()
            )
        };
        return Outcome::Blocked {
            what: format!(
                "a certificate request for '{id}' exists but its private key does not — \
                 nothing can ever be signed with a certificate issued from it"
            ),
            fix,
        };
    }

    // A key with no CSR is recoverable: rebuild the request from the key (spends nothing)
    // rather than deleting a key whose slot may be spent.
    if let Some(key) = &seen.key {
        return match write_csr(key, &csr_path(&dir, id)) {
            Ok(csr) => {
                step(
                    APPLE_LABEL,
                    &format!(
                        "rebuilt the certificate request for '{id}' from the key already here \
                     — no new key, no new slot"
                    ),
                );
                await_then_install(key, &csr, id, &dir, seen.invalid_present, cert_dir)
            }
            Err(e) => Outcome::Blocked {
                what: format!("could not rebuild the certificate request: {e}"),
                fix: format!("check {} is a readable private key", key.display()),
            },
        };
    }

    // !I && !K && !C — run 1. This is the only branch that spends a slot.
    match confirm_slot(id, seen.invalid_present) {
        Ok(true) => match generate(&dir, id) {
            Ok(csr) => await_then_install(
                &key_path(&dir, id),
                &csr,
                id,
                &dir,
                seen.invalid_present,
                cert_dir,
            ),
            Err(outcome) => outcome,
        },
        Ok(false) => Outcome::Waiting {
            what: "declined to generate a certificate signing request".into(),
            next: format!(
                "re-run `{} provision --id {id}` when ready to spend a slot",
                crate::publish::SHIP_COMMAND
            ),
        },
        Err(e) => Outcome::Blocked {
            what: format!("could not ask before spending a Developer ID slot: {e}"),
            fix: "run provision from a terminal — it will not spend a slot unasked".into(),
        },
    }
}

/// How long the command waits for the browser errand: long enough to sign in to Apple as
/// the Account Holder, short enough that a forgotten terminal does not hold a session all
/// night.
const WAIT_FOR_CERT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Print the browser errand and wait for the certificate, matched by public key as it
/// lands in `~/.aterm/apple` or the `--cert-dir` folder, then install it. Without a
/// terminal it reports the errand and returns.
fn await_then_install(
    key: &Path,
    csr: &Path,
    id: &str,
    dir: &Path,
    invalid: usize,
    cert_dir: Option<&Path>,
) -> Outcome {
    let watch = Watch::new(cert_dir);
    // Two causes, two remedies: an unreadable directory, or this machine's own request
    // key unreadable. The second is not a certificate problem; sending the operator to move
    // a .cer would loop past a key that may be corrupt.
    let blocked = |why: String| {
        let about_our_key = why.contains("could not read the public half");
        if about_our_key {
            Outcome::Blocked {
                what: format!(
                    "this machine's own certificate request key cannot be read, so nothing \
                     can be matched against it: {why}",
                ),
                fix: format!(
                    "prove it first: openssl rsa -in {} -noout -check\n\
                     if that fails the key is damaged. A NEW request costs another of team \
                     {}'s five permanent Developer ID slots, so recover the file before \
                     minting one — from a backup of ~/.aterm/apple, or from the machine \
                     that made it.",
                    key.display(),
                    pins::APPLE_TEAM_ID
                ),
            }
        } else {
            Outcome::Blocked {
                what: format!("could not look for the issued certificate: {why}"),
                fix: format!(
                    "make the directory readable, then re-run: ls -ld {}",
                    dir.display()
                ),
            }
        }
    };
    let rejected = match find_matching_cert(key, &watch) {
        Ok((Some(cer), _)) => return install(&cer, key, id),
        Err(why) => return blocked(why),
        Ok((None, rejected)) => rejected,
    };
    // Nobody to wait for (CI, a pipe), and no errand printed on this path, so the verdict
    // carries the only copy.
    if !has_terminal() {
        return Outcome::Waiting {
            what: waiting_text(id, invalid, &rejected, None, &watch),
            // The same errand from the same function, minus the wait paragraph.
            next: errand_lines(csr, false, &watch).join("\n"),
        };
    }
    // Said before the errand and the wait: "the file you downloaded is not this one" must
    // not hide behind thirty minutes of "Apple has not answered", or the operator creates
    // a second certificate to fix a download.
    //
    // The request is surfaced in the folder the operator named (and revealed in Finder)
    // before the errand names it, since an upload dialog cannot see hidden ~/.aterm. With
    // no folder named nothing is copied: the errand names the hidden path and how a dialog
    // reaches it (⌘⇧G). `~/Downloads` is never touched on the operator's behalf.
    let visible = watch
        .named
        .as_deref()
        .and_then(|named| surface_csr(csr, id, named));
    let upload = visible.as_deref().unwrap_or(csr);
    // Built here so it captures `upload`, the copy a dialog can see: the timeout verdict is
    // the last line on screen when the command exits.
    let waiting = |rejected: &[PathBuf]| Outcome::Waiting {
        what: waiting_text(id, invalid, rejected, Some(upload), &watch),
        next: format!(
            "upload {} at https://developer.apple.com/account/resources/certificates and \
             download the certificate THAT produces.\n\
             do NOT create a second certificate — that spends another of team {}'s five \
             permanent Developer ID Application slots, and it will not help: a second \
             certificate made from a second request has a second key, and this machine \
             only matches the request above.",
            upload.display(),
            pins::APPLE_TEAM_ID
        ),
    };
    let mut label = APPLE_LABEL;
    if let Some(note) = watch.consent_note() {
        step(label, &note);
        label = "";
    }
    if let Some(note) = rejected_note(&rejected, Some(upload), &watch) {
        step(label, &note);
        label = "";
    }
    // Nothing is printed after the errand: it ends with the trap, which must be the last
    // thing on screen (a trailing "waiting…" reads as permission to take whatever the
    // portal lists). The wait's facts live inside the errand, above the trap.
    for line in errand_lines(upload, true, &watch) {
        step(label, &line);
        label = "";
    }
    let started = std::time::Instant::now();
    let mut announced = 0u64;
    let mut rejected = rejected;
    while started.elapsed() < WAIT_FOR_CERT {
        std::thread::sleep(std::time::Duration::from_secs(3));
        match find_matching_cert(key, &watch) {
            Ok((Some(cer), _)) => {
                step(
                    APPLE_LABEL,
                    &format!("certificate found: {}", cer.path.display()),
                );
                return install(&cer, key, id);
            }
            Err(why) => return blocked(why),
            // Kept current and said the moment a file appears: the likeliest .cer is one
            // downloaded during the wait, and if it is the wrong one, "still waiting"
            // would hide that and send the operator to spend a second slot.
            Ok((None, seen)) => {
                let fresh: Vec<PathBuf> = seen
                    .iter()
                    .filter(|p| !rejected.contains(p))
                    .cloned()
                    .collect();
                if let Some(note) = rejected_note(&fresh, Some(upload), &watch) {
                    // A header, so the note reads as a refusal of the file just
                    // downloaded, not as progress.
                    step(
                        APPLE_LABEL,
                        "⚠ THE .cer THAT JUST ARRIVED IS NOT THIS REQUEST'S",
                    );
                    step("", &note);
                }
                rejected = seen;
            }
        }
        // A heartbeat naming its horizon, so a long wait never looks like a hang.
        let mins = started.elapsed().as_secs() / 60;
        if mins > announced {
            announced = mins;
            let of = WAIT_FOR_CERT.as_secs() / 60;
            step(
                "",
                &format!("still waiting for the .cer… {mins} min of {of}"),
            );
        }
    }
    // Said, so the verdict cannot be mistaken for a crash.
    step(
        APPLE_LABEL,
        &format!(
            "{} min elapsed — stopping the wait. Nothing was lost and nothing was spent.",
            WAIT_FOR_CERT.as_secs() / 60
        ),
    );
    waiting(&rejected)
}

/// Whether there is a human to ask: the prompts' test, so "will it wait?" and "will it
/// prompt?" never disagree.
fn has_terminal() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .is_ok_and(|f| std::io::IsTerminal::is_terminal(&f))
}

/// Prove that a pinned identity (the profile's `signing_identity_sha1`) is one of this
/// machine's valid Developer ID Application identities and can sign unattended: the test
/// signature [`verdict`] runs, aimed at the pinned certificate. `find-identity`'s order is
/// not stable across runs, so blessing only `ids[0]` would flip between pass and fail with
/// two valid certificates; a valid pin that signs is what the cut accepts
/// (`sign::select_devid_identity`).
pub(crate) fn pinned_identity_proves(sha1: &str) -> Result<(), String> {
    let (ids, lookup_failed) = valid_identities();
    if let Some(why) = lookup_failed {
        return Err(format!("cannot read the keychain: {why}"));
    }
    if !ids.iter().any(|id| id.eq_ignore_ascii_case(sha1)) {
        return Err(format!(
            "no valid Developer ID Application identity {sha1} for team {} in the keychain",
            pins::APPLE_TEAM_ID
        ));
    }
    prove_can_sign(sha1)
}

/// The single place a green Apple verdict is produced, so no path skips the proof. A
/// signature that fails is not ready.
fn verdict(ids: Vec<String>) -> Outcome {
    match prove_can_sign(&ids[0]) {
        Ok(()) => {
            // The expiry rides here, with the proof, so every green line carries it.
            let note = expiry_note(&ids[0]);
            Outcome::Ready { ids, note }
        }
        Err(why) => Outcome::Blocked {
            what: format!(
                "identity {} is installed but a test signature did not succeed ({why}) — \
                 codesign cannot use this key unattended, so a cut would stop here",
                ids[0]
            ),
            fix: format!(
                "run `security set-key-partition-list -S apple-tool:,apple:,codesign: -t \
                 private -s -l '{}' {}` (it asks for your login keychain password), then \
                 re-run this command — it re-tests the signature every time",
                identity_label(&ids[0]).unwrap_or_else(|| "<the certificate's name>".into()),
                login_keychain()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "~/Library/Keychains/login.keychain-db".into()),
            ),
        },
    }
}

/// Whether the on-disk request key is the private half of the certificate this identity
/// names. Decides only whether the spent request may be removed, never the key.
fn paired_with_installed(key: &Option<PathBuf>, sha1: &str) -> bool {
    let Some(key) = key else { return false };
    installed_cert_spki(sha1)
        .is_some_and(|installed| key_spki_sha256(key).is_some_and(|k| k == installed))
}

/// The SPKI digest of the certificate the identity `sha1` names. Selected by digest:
/// `openssl x509` parses only the first block of a `find-certificate -a` stream, and a
/// renewal leaves two overlapping Developer ID certificates.
fn installed_cert_spki(sha1: &str) -> Option<String> {
    let pem = installed_leaf_pem(sha1)?;
    let der = capture(
        "/usr/bin/openssl",
        &["x509", "-pubkey", "-noout"],
        Some(pem.as_bytes()),
    )?;
    let der = capture(
        "/usr/bin/openssl",
        &["pkey", "-pubin", "-outform", "DER"],
        Some(&der),
    )?;
    digest(&der)
}

/// The PEM of the installed `Developer ID Application` certificate whose SHA-1 (the digest
/// `find-identity` names an identity by) is `sha1`.
///
/// `security find-certificate -a -p -Z` prints, per certificate, a `SHA-256 hash:` line, a
/// `SHA-1 hash:` line, then the PEM block; nothing else separates one from the next.
fn installed_leaf_pem(sha1: &str) -> Option<String> {
    let out = capture(
        "/usr/bin/security",
        &[
            "find-certificate",
            "-a",
            "-c",
            "Developer ID Application",
            "-p",
            "-Z",
        ],
        None,
    )?;
    leaf_pem_by_sha1(&String::from_utf8_lossy(&out), sha1)
}

/// The selection, pure over the captured stream so it is testable without a keychain.
fn leaf_pem_by_sha1(text: &str, sha1: &str) -> Option<String> {
    let want = sha1.to_ascii_lowercase();
    let mut current: Option<String> = None;
    let mut block = String::new();
    let mut in_block = false;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("SHA-1 hash:") {
            current = Some(rest.trim().to_ascii_lowercase());
            continue;
        }
        if line.starts_with("-----BEGIN CERTIFICATE-----") {
            in_block = true;
            block.clear();
        }
        if in_block {
            block.push_str(line);
            block.push('\n');
        }
        if line.starts_with("-----END CERTIFICATE-----") {
            in_block = false;
            if current.as_deref() == Some(want.as_str()) {
                return Some(std::mem::take(&mut block));
            }
        }
    }
    None
}

/// How close to expiry the green line starts naming the renewal errand: ninety days, the
/// window Apple's renewal flow assumes.
const RENEWAL_WINDOW_SECS: u64 = 90 * 24 * 60 * 60;

/// The expiry clause on a green Apple line: `valid to 2031-08-18`, or the renewal errand
/// once the certificate is inside its last [`RENEWAL_WINDOW_SECS`]. `find-identity -v` is
/// a boolean, so without a date a certificate lapsing next week audits green until the
/// day it reads as "no identity".
fn expiry_note(sha1: &str) -> Option<String> {
    let leaf = installed_leaf_pem(sha1)?;
    let mut soonest = (cert_enddate_iso(&leaf)?, leaf.clone());
    // A chain lives as long as its shortest link, and that is not always the leaf: the
    // original Developer ID intermediate expires 2027-02-01, and when it passes every
    // certificate under it stops being an identity.
    if let Some(ca) = issuer_in_keychain(&leaf) {
        // ISO-8601 sorts chronologically.
        match cert_enddate_iso(&ca) {
            Some(iso) if iso < soonest.0 => soonest = (iso, ca),
            _ => {}
        }
    }
    let (iso, pem) = soonest;
    // `-checkend` answers "does this outlive N seconds?" (exit 0 beyond the window, 1
    // inside it on LibreSSL 3.3.6), so there is no date arithmetic and no second "now".
    match outlives(&pem, RENEWAL_WINDOW_SECS) {
        Some(true) => Some(format!("valid to {iso}")),
        Some(false) => Some(format!(
            "EXPIRES {iso} — request a renewal certificate at developer.apple.com"
        )),
        None => None,
    }
}

/// `notAfter` of a PEM certificate, as `YYYY-MM-DD`.
fn cert_enddate_iso(pem: &str) -> Option<String> {
    let out = capture(
        "/usr/bin/openssl",
        &["x509", "-noout", "-enddate"],
        Some(pem.as_bytes()),
    )?;
    enddate_iso(&String::from_utf8_lossy(&out))
}

/// The certificate in this keychain that issued `leaf`, matched by subject DN.
fn issuer_in_keychain(leaf: &str) -> Option<String> {
    let out = capture(
        "/usr/bin/openssl",
        &["x509", "-noout", "-issuer"],
        Some(leaf.as_bytes()),
    )?;
    let issuer = String::from_utf8_lossy(&out)
        .trim()
        .trim_start_matches("issuer=")
        .trim()
        .to_string();
    keychain_cert_with_subject(&issuer)
}

/// `notAfter=Aug 18 16:36:18 2031 GMT` → `2031-08-18`.
fn enddate_iso(line: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let tail = line.trim().strip_prefix("notAfter=")?;
    let mut parts = tail.split_whitespace();
    let month = parts.next()?;
    let day: u32 = parts.next()?.parse().ok()?;
    let _time = parts.next()?;
    let year: u32 = parts.next()?.parse().ok()?;
    let month = MONTHS.iter().position(|m| *m == month)? + 1;
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// Whether `openssl x509 -checkend` says this certificate outlives `secs`; `None` when it
/// could not be asked, which reports no clause rather than a date.
fn outlives(pem: &str, secs: u64) -> Option<bool> {
    let mut child = Command::new("/usr/bin/openssl")
        .args(["x509", "-noout", "-checkend", &secs.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(pem.as_bytes()).ok()?;
    Some(child.wait().ok()?.success())
}

/// The label macOS gives the paired private key (the certificate's common name), for the
/// partition-list remediation.
fn identity_label(sha1: &str) -> Option<String> {
    let out = capture(
        "/usr/bin/security",
        &["find-identity", "-v", "-p", "codesigning"],
        None,
    )?;
    let text = String::from_utf8_lossy(&out).to_string();
    let line = text.lines().find(|l| l.contains(sha1))?;
    let start = line.find('"')? + 1;
    let end = line.rfind('"')?;
    (end > start).then(|| line[start..end].to_string())
}

/// The plural `s`, so a count of 1 never reads `1 item(s)`.
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The verdict when no certificate has arrived.
///
/// `csr` is `Some` once a request has been surfaced to a path the operator can act on, and
/// selects the strong half of [`rejected_note`] ("not a download problem: upload <path>").
/// `None` selects the weak half ("move the .cer into a watched folder"); the
/// non-interactive path keeps `None` because nothing was surfaced there.
fn waiting_text(
    id: &str,
    invalid: usize,
    rejected: &[PathBuf],
    csr: Option<&Path>,
    watch: &Watch,
) -> String {
    let mut s = format!(
        "a certificate request for '{id}' is out for signature and no matching certificate has arrived yet"
    );
    if let Some(note) = rejected_note(rejected, csr, watch) {
        s.push_str("; ");
        s.push_str(&note);
    }
    if invalid > 0 {
        s.push_str(&format!(
            " (note: this keychain also holds {invalid} Developer ID certificate{} for \
             this team that {} not valid — expired, or missing their private key)",
            plural(invalid),
            if invalid == 1 { "is" } else { "are" },
        ));
    }
    s
}

/// Ask a question on the controlling terminal and read one whole line back.
///
/// A whole line, so no leftover of "yes" answers the next prompt. `/dev/tty` must be a
/// real terminal (the master phrase's rule), so a piped run can never answer a question
/// about spending a permanent slot.
fn tty_line(prompt: &str, max: usize) -> Result<String, String> {
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|e| format!("cannot open /dev/tty: {e}"))?;
    if !std::io::IsTerminal::is_terminal(&tty) {
        return Err("/dev/tty is not a terminal".into());
    }
    write!(tty, "{prompt}").map_err(|e| format!("cannot write to /dev/tty: {e}"))?;
    tty.flush().ok();
    let mut line = String::new();
    let mut byte = [0u8; 1];
    // Always drain to the newline: `max` bounds what is kept, never where reading stops.
    // An early break leaves the answer's tail queued for the next `/dev/tty` read, which
    // here is the echo-off master phrase.
    loop {
        match tty.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if line.len() < max {
                    line.push(byte[0] as char);
                }
            }
            Err(e) => return Err(format!("cannot read from /dev/tty: {e}")),
        }
    }
    Ok(line.trim().to_string())
}

/// The one irreversible question, printed through [`crate::publish::grid_block`] under
/// [`APPLE_LABEL`] like every line above it.
///
/// Both answers state their cost: `y` spends a slot forever, and `N` costs nothing and
/// comes back next run. An operator unsure whether declining aborts the run would take the
/// irreversible path.
fn confirm_slot(id: &str, invalid: usize) -> Result<bool, String> {
    let mut msg = format!(
        "generating a CSR for '{id}' spends one of team {}'s five Developer ID Application \
         slots, permanently. Revoking one later stops every app it already signed from \
         launching.",
        pins::APPLE_TEAM_ID
    );
    if invalid > 0 {
        msg.push_str(&format!(
            "\nNOTE: this keychain already holds {invalid} Developer ID certificate{} for \
             this team that {} not valid (expired, or missing their private key). Those \
             slots are already spent.",
            plural(invalid),
            if invalid == 1 { "is" } else { "are" },
        ));
    }
    msg.push_str(
        "\nN is safe: the rest of the audit still runs, nothing else here spends anything, \
         and this question comes back on the next run.\nContinue? [y/N] ",
    );
    let prompt = format!("\n{}", crate::publish::grid_block(APPLE_LABEL, &msg));
    let answer = tty_line(&prompt, 64)?.to_ascii_lowercase();
    let yes = answer == "y" || answer == "yes";
    // Say when an answer that is neither yes nor no was taken as no.
    if !yes && !answer.is_empty() && answer != "n" && answer != "no" {
        step(
            APPLE_LABEL,
            &format!("'{answer}' is not 'y' — taking that as no; no slot spent"),
        );
    }
    Ok(yes)
}

/// The keychain profile name the notarytool credential is stored under: a local keychain
/// label, the same on every machine. `ensure_notary` stores it, `write_credentials_profile`
/// writes it, `provision::notary_check` live-tests it and the cut reads it back.
/// Per-machine revocability lives in the app-specific password behind it, minted per
/// machine at https://account.apple.com.
pub(crate) const NOTARY_PROFILE: &str = "notary";

/// Store a notarytool credential in this machine's keychain.
///
/// `notarytool store-credentials` owns the password prompt on this terminal's inherited
/// stdio, so an app-specific password never passes through this process or its argv. Its
/// default `--validate` round-trips to Apple, so a stored credential has authenticated.
pub(crate) fn ensure_notary(may_change: bool) -> Outcome {
    if !cfg!(target_os = "macos") {
        return Outcome::Skipped("cuts run on macOS (Tier APPLE); no keychain on this host".into());
    }
    if !may_change {
        return Outcome::Todo {
            what: "no notarytool credential in the keychain".into(),
            next: "re-run without --check to store one".into(),
        };
    }
    // No "already stored?" probe: the caller (`provision::notary_acquire`) live-checks with
    // `notarytool history` first, the only test that means anything.
    let profile = NOTARY_PROFILE;
    // Said before the prompts: the password notarytool asks for exists nowhere until the
    // operator mints it, and an Apple ID password pasted there fails only after a round
    // trip to Apple. Numbered, with the shown-once fact read before the tab is closed.
    step(
        "notary",
        "the password this step asks for is an APP-SPECIFIC password, NOT your Apple ID \
         password. Mint one first — Apple shows it once:\n\
         1. https://account.apple.com → Sign-In and Security\n\
         2. App-Specific Passwords → +   (2FA required)\n\
         3. copy the xxxx-xxxx-xxxx-xxxx string\n\
         then paste it at the password prompt below.",
    );
    // Through the grid, aligned with the `notary` line above. "Blank" names its
    // consequence: a blank answer is `Waiting`, which defers the mint, so on a first run no
    // roster id is minted.
    let apple_id = match tty_line(
        &format!(
            "\n{}",
            crate::publish::grid_block(
                "notary",
                "Apple ID for notarization\n(blank skips it — but a cut is refused without \
                 notarization, and on a first run no roster id is minted either): "
            )
        ),
        128,
    ) {
        Ok(v) => v,
        Err(e) => {
            return Outcome::Waiting {
                what: format!("no notarytool credential, and no terminal to ask on ({e})"),
                next: format!(
                    "xcrun notarytool store-credentials {profile} --apple-id <your-apple-id> \
                     --team-id {}",
                    pins::APPLE_TEAM_ID
                ),
            };
        }
    };
    if apple_id.is_empty() {
        return Outcome::Waiting {
            what: "no notarytool credential in the keychain".into(),
            next: format!(
                "xcrun notarytool store-credentials {profile} --apple-id <your-apple-id> \
                 --team-id {} (password: an app-specific password — mint at https://account.apple.com → Sign-In and Security → App-Specific Passwords)",
                pins::APPLE_TEAM_ID
            ),
        };
    }
    let status = Command::new("/usr/bin/xcrun")
        .args([
            "notarytool",
            "store-credentials",
            profile,
            "--apple-id",
            &apple_id,
            "--team-id",
            pins::APPLE_TEAM_ID,
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    // notarytool's exit status is the verdict. `security find-generic-password` cannot
    // confirm it: notarytool stores the credential in the data-protection keychain, which
    // `security` never sees. The caller's live `notarytool history` check confirms it
    // authenticates and prints the line the operator reads.
    match status {
        Ok(s) if s.success() => Outcome::Ready {
            ids: Vec::new(),
            note: None,
        },
        Ok(_) => Outcome::Waiting {
            what: "notarytool did not store a credential".into(),
            next: format!(
                "check the Apple ID and app-specific password, then re-run; or run \
                 `xcrun notarytool store-credentials {profile} --apple-id {apple_id} \
                 --team-id {}` by hand",
                pins::APPLE_TEAM_ID
            ),
        },
        Err(e) => Outcome::Blocked {
            what: format!("could not run xcrun notarytool: {e}"),
            fix: "install the Xcode command-line tools (`xcode-select --install`)".into(),
        },
    }
}

/// Run 1: an RSA-2048 keypair and a CSR, both born here. `/usr/bin/openssl` explicitly: a
/// Homebrew OpenSSL 3 ahead of it on PATH changes defaults and output formats.
fn generate(dir: &Path, id: &str) -> Result<PathBuf, Outcome> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Err(Outcome::Blocked {
            what: format!("cannot create {}: {e}", dir.display()),
            fix: "check ownership of ~/.aterm".into(),
        });
    }
    if let Err(e) = chmod(dir, 0o700) {
        return Err(Outcome::Blocked {
            what: format!("cannot secure {}: {e}", dir.display()),
            fix: "check ownership of ~/.aterm/apple".into(),
        });
    }
    let key = key_path(dir, id);
    let csr = csr_path(dir, id);

    // Belt and braces over the state machine: `openssl genrsa -out` silently truncates an
    // existing file, and that key may be the private half of a spent slot.
    if key.exists() || csr.exists() {
        return Err(Outcome::Blocked {
            what: format!(
                "refusing to overwrite existing key material for '{id}' in {}",
                dir.display()
            ),
            fix: "re-run: the state machine recovers a key without a request by rebuilding \
                  the request, and never by replacing the key"
                .into(),
        });
    }
    if let Err(e) = run(
        "/usr/bin/openssl",
        &["genrsa", "-out", &key.to_string_lossy(), "2048"],
    ) {
        return Err(Outcome::Blocked {
            what: format!("could not generate a private key: {e}"),
            fix: "install the Xcode command-line tools (`xcode-select --install`)".into(),
        });
    }
    // The key is the half that never travels.
    if let Err(e) = chmod(&key, 0o600) {
        let _ = std::fs::remove_file(&key);
        return Err(Outcome::Blocked {
            what: format!("could not restrict the new private key to 0600: {e}"),
            fix: "check ownership of ~/.aterm/apple".into(),
        });
    }
    write_csr(&key, &csr).map_err(|e| Outcome::Blocked {
        what: format!("could not build the certificate signing request: {e}"),
        fix: "re-run: the key is kept and only the request is rebuilt".into(),
    })
}

/// Derive the request from the key: separate from [`generate`] because a lost request can
/// be redone and a lost key cannot.
fn write_csr(key: &Path, csr: &Path) -> Result<PathBuf, String> {
    // The subject Keychain Access produces for a Developer ID request; Apple replaces the
    // common name on issuance, so it only has to parse.
    let subject = format!(
        "/CN=Developer ID Application/O={}/C=US",
        pins::APPLE_TEAM_ID
    );
    run(
        "/usr/bin/openssl",
        &[
            "req",
            "-new",
            "-key",
            &key.to_string_lossy(),
            "-out",
            &csr.to_string_lossy(),
            "-subj",
            &subject,
        ],
    )?;
    Ok(csr.to_path_buf())
}

/// The one unavoidable human step, one instruction per line, and its only spelling: both
/// paths print these lines. The upload target has a line of its own and the trap comes
/// last, where the eye lands. Only the Account Holder can create a Developer ID
/// certificate (a delegate can see the list); the download is matched by public key.
///
/// `waiting` is set only by the interactive path, the only one that then waits.
pub(crate) fn errand_lines(csr: &Path, waiting: bool, watch: &Watch) -> Vec<String> {
    let mut out = vec![
        // The file first, on its own line, and named again at the step that uploads it.
        "you will upload THIS file (and only this one):".to_string(),
        format!("     {}", csr.display()),
        String::new(),
        "at https://developer.apple.com/account/resources/certificates".to_string(),
        "     signed in as the Account Holder — Apple lets nobody else create a Developer \
         ID certificate"
            .to_string(),
        String::new(),
        // The portal's own click order: the request is uploaded after the type and
        // profile are chosen.
        "1. + → Software → 'Developer ID Application'".to_string(),
        "2. Profile Type 'G2 Sub-CA'".to_string(),
        "     NOT 'Previous Sub-CA' — its intermediate expires 2027-02-01".to_string(),
        "3. upload the request named above".to_string(),
        format!("4. Download the result into {}", watch.spoken()),
        "     the filename does not matter — it is matched against this request by public key"
            .to_string(),
    ];
    if watch.named.is_none() {
        // No visible folder was named, so the download comes back to hidden ~/.aterm: say
        // how a file dialog reaches it, and how to name a folder next time.
        out.push(
            "     (hidden folders: press ⌘⇧G in the file dialog and paste the path; or \
             re-run with --cert-dir <folder> to have the request copied to, and the \
             certificate looked for in, a folder you name)"
                .to_string(),
        );
    }
    if waiting {
        // The wait's facts go above the trap: printed last, "waiting for the certificate"
        // reads as permission to collect whatever the portal lists.
        out.push(String::new());
        out.push(format!(
            "watching {}: this step finishes the instant the matching .cer lands, and \
             gives up after {} min. Ctrl-C is safe — nothing is lost and this step \
             resumes exactly here.",
            watch.spoken(),
            WAIT_FOR_CERT.as_secs() / 60
        ));
    }
    out.push(String::new());
    // The trap states both halves: a certificate issued from this request is the right one
    // and costs nothing, and creating another spends one of five permanent slots.
    out.push(
        "⚠ THE TRAP: the portal's list may ALREADY show a Developer ID certificate.".to_string(),
    );
    out.push(
        "  Download it ONLY if YOU created it from the request named above — then it \
         matches, and costs nothing."
            .to_string(),
    );
    out.push(
        "  Any other one holds a different key, will be refused here, and its private \
         half is on whichever machine made it."
            .to_string(),
    );
    out.push(
        "  Creating a new one instead spends one of this team's five Developer ID slots, \
         permanently — the right move ONLY when the list has nothing issued from the \
         request above."
            .to_string(),
    );
    // The common case: a wrong .cer is already in the watched folder. The act is to look
    // again, not "delete it and start over": a non-matching .cer is ignored, and starting
    // over mints a second certificate.
    out.push(
        "  Already downloaded one? Nothing to undo — a .cer that does not match is \
         ignored, never installed."
            .to_string(),
    );
    out.push(
        "  Check the list once more for one issued from the request above; create a new \
         certificate only if there is none."
            .to_string(),
    );
    out
}

/// Where the certificate is looked for and the request surfaced: `~/.aterm/apple`, plus
/// the one folder the operator named with `--cert-dir`. Naming it is the consent, so it may
/// be a folder macOS guards; nothing else is read, because from an aterm shell aterm owns
/// a guarded folder's consent dialog (`atpkg::protected`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Watch {
    /// `~/.aterm/apple`, or `None` without a `$HOME` (then only `named` is read).
    own: Option<PathBuf>,
    /// The folder the operator named, as given (a relative path is theirs to resolve).
    named: Option<PathBuf>,
}

impl Watch {
    pub(crate) fn new(named: Option<&Path>) -> Self {
        Self {
            own: apple_dir(),
            named: named.map(Path::to_path_buf),
        }
    }

    /// The folders `find_matching_cert` reads: the named one first (where the download
    /// lands), then aterm's own.
    fn dirs(&self) -> Vec<PathBuf> {
        self.named.iter().chain(self.own.iter()).cloned().collect()
    }

    /// The folders, spoken for the errand (`~/Downloads or ~/.aterm/apple`), `$HOME` folded
    /// to `~`.
    pub(crate) fn spoken(&self) -> String {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let tilde = |p: &Path| -> String {
            match &home {
                Some(h) if p.starts_with(h) => match p.strip_prefix(h) {
                    Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
                    Ok(rest) => format!("~/{}", rest.display()),
                    Err(_) => p.display().to_string(),
                },
                _ => p.display().to_string(),
            }
        };
        let mut parts: Vec<String> = self.dirs().iter().map(|p| tilde(p)).collect();
        if parts.is_empty() {
            parts.push("~/.aterm/apple".to_string());
        }
        parts.join(" or ")
    }

    /// A line, printed before the errand, saying the named folder is one macOS guards, so
    /// a consent dialog is explained before it appears; `None` when nothing named is
    /// guarded. Judged on the canonical path, which is what gets opened (`canonicalize` is
    /// not itself gated); a path that cannot be resolved is judged as given.
    fn consent_note(&self) -> Option<String> {
        let named = self.named.as_deref()?;
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        let resolved = std::fs::canonicalize(named).unwrap_or_else(|_| named.to_path_buf());
        let home_resolved = std::fs::canonicalize(&home).unwrap_or(home);
        if atpkg::protected::under_app_data(&home_resolved, &resolved) {
            return Some(format!(
                "{} is inside another app's container that macOS guards: reading it raises a \
                 \"would like to access data from other apps\" dialog for this terminal on \
                 every run (unless Full Disk Access is granted) — you named the folder, so \
                 that dialog is yours to answer",
                named.display()
            ));
        }
        atpkg::protected::under_protected_root(&home_resolved, &resolved).then(|| {
            format!(
                "{} is a folder macOS guards: reading it can raise a one-time \
                 \"would like to access files\" dialog for this terminal — you named the \
                 folder, so that dialog is yours to answer",
                named.display()
            )
        })
    }
}

/// Copy the request into the folder the operator named, where an upload dialog can see it
/// and where the certificate comes back, and reveal it in Finder. The CSR is public
/// material, so the copy leaks nothing; best-effort, since the hidden path still works.
/// Never called with an unnamed folder: writing into a guarded folder raises the
/// terminal's consent dialog.
#[cfg(unix)]
fn surface_csr(csr: &Path, id: &str, named: &Path) -> Option<PathBuf> {
    let visible = named.join(format!("devid-{id}.certSigningRequest"));
    std::fs::copy(csr, &visible).ok()?;
    let _ = Command::new("/usr/bin/open")
        .arg("-R")
        .arg(&visible)
        .status();
    Some(visible)
}

/// Find a downloaded certificate that belongs to this key, by comparing public keys: the
/// machine holds the only thing that identifies the right file, so there is no `--cer`
/// flag and no trusted filename.
///
/// Looks only in [`Watch::dirs`]: this polls every three seconds for up to thirty
/// minutes, and reading an unnamed guarded folder from an aterm shell raises a consent
/// dialog nobody asked for.
///
/// `Err` means "could not look"; `Ok((None, …))` means "looked, not there". The second
/// half of the pair is every `.cer` examined and refused, so a wrong file in the folder
/// does not read as "Apple has not answered". Paths only, no `openssl` reads: the reasons
/// are read once, in [`rejected_note`].
fn find_matching_cert(
    key: &Path,
    watch: &Watch,
) -> Result<(Option<MatchedCert>, Vec<PathBuf>), String> {
    let want = key_spki_sha256(key)
        .ok_or_else(|| format!("could not read the public half of {}", key.display()))?;
    let mut unreadable = Vec::new();
    let mut rejected = Vec::new();
    for dir in watch.dirs() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            // A missing folder is normal; one that exists but cannot be read is not.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                unreadable.push(format!("{}: {e}", dir.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_cer = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("cer"));
            if !is_cer {
                continue;
            }
            match ["DER", "PEM"]
                .into_iter()
                .find(|form| cert_spki_sha256(&path, form).as_deref() == Some(want.as_str()))
            {
                Some(form) => return Ok((Some(MatchedCert { path, form }), rejected)),
                None => rejected.push(path),
            }
        }
    }
    if !unreadable.is_empty() {
        return Err(unreadable.join("; "));
    }
    Ok((None, rejected))
}

/// The `.cer` files examined and refused, as one line: which file, why, and what fixes it.
/// Named, because "no matching certificate" about a folder the operator can see a
/// certificate in sends them to spend another slot.
fn rejected_note(rejected: &[PathBuf], csr: Option<&Path>, watch: &Watch) -> Option<String> {
    // Enough to recognise the one just downloaded.
    const SHOWN: usize = 3;
    if rejected.is_empty() {
        return None;
    }
    let named: Vec<String> = rejected
        .iter()
        .take(SHOWN)
        .map(|p| {
            let why = match ["DER", "PEM"].into_iter().find_map(|f| cert_subject(p, f)) {
                Some(_) => "a different public key",
                None => "not a certificate",
            };
            format!("{} ({why})", p.display())
        })
        .collect();
    let more = match rejected.len().saturating_sub(SHOWN) {
        0 => String::new(),
        n => format!(" +{n} more"),
    };
    // Name the act that fixes it, or "a different public key" reads as a bad download
    // and the next move is a second certificate.
    let fix = match csr {
        Some(csr) => format!(
            " — this is not a download problem: upload {} at the portal and download the \
             certificate THAT produces",
            csr.display()
        ),
        None => format!(
            " — if the browser saved the .cer elsewhere, move it into {}",
            watch.spoken()
        ),
    };
    Some(format!(
        "examined, not this request's: {}{more}{fix}",
        named.join(", ")
    ))
}

/// SHA-256 of the DER SubjectPublicKeyInfo of a private key.
fn key_spki_sha256(key: &Path) -> Option<String> {
    let der = capture(
        "/usr/bin/openssl",
        &[
            "rsa",
            "-in",
            &key.to_string_lossy(),
            "-pubout",
            "-outform",
            "DER",
        ],
        None,
    )?;
    digest(&der)
}

/// The same digest taken from a certificate in a known encoding, comparable with
/// [`key_spki_sha256`].
fn cert_spki_sha256(cer: &Path, form: &str) -> Option<String> {
    let pem_pub = capture(
        "/usr/bin/openssl",
        &[
            "x509",
            "-inform",
            form,
            "-in",
            &cer.to_string_lossy(),
            "-pubkey",
            "-noout",
        ],
        None,
    )?;
    let der = capture(
        "/usr/bin/openssl",
        &["pkey", "-pubin", "-outform", "DER"],
        Some(&pem_pub),
    )?;
    digest(&der)
}

/// SHA-256 of `der`, as hex. `/usr/bin/openssl dgst -sha256` on stdin prints the bare hex
/// (no `(stdin)= ` prefix); given a file it prints `SHA2-256(path)= <hex>`. Both parse.
fn digest(der: &[u8]) -> Option<String> {
    let out = capture("/usr/bin/openssl", &["dgst", "-sha256"], Some(der))?;
    let text = String::from_utf8_lossy(&out);
    let hex = text
        .trim()
        .rsplit(['=', ' '])
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    aterm_codec::hex::is_hex(&hex, 64).then_some(hex)
}

/// Run 2: check the certificate, import it and the key so macOS binds them into an
/// identity, widen the key's ACL, and prove the result signs. Retry-safe: an item already
/// in the keychain is success.
fn install(cer: &MatchedCert, key: &Path, id: &str) -> Outcome {
    let subject = match cert_subject(&cer.path, cer.form) {
        Some(s) => s,
        // Not "wrong team": that would send the operator to re-issue a probably fine
        // certificate.
        None => {
            return Outcome::Blocked {
                what: format!("could not read the subject of {}", cer.path.display()),
                fix: "check the file is the certificate Apple issued, not an HTML error page"
                    .into(),
            };
        }
    };
    if !subject.contains(pins::APPLE_TEAM_ID) {
        return Outcome::Blocked {
            what: format!(
                "{} is not a certificate for team {} ({})",
                cer.path.display(),
                pins::APPLE_TEAM_ID,
                subject.trim()
            ),
            fix: "download the certificate issued for THIS machine's request".into(),
        };
    }
    let Some(keychain) = login_keychain() else {
        return Outcome::Blocked {
            what: "no login keychain found".into(),
            fix: "run provision as the user that will cut releases".into(),
        };
    };
    let kc = keychain.to_string_lossy().to_string();

    if let Err(e) = import_idempotent(&[
        "import",
        &cer.path.to_string_lossy(),
        "-k",
        &kc,
        "-T",
        "/usr/bin/codesign",
    ]) {
        return Outcome::Blocked {
            what: format!("could not import {}: {e}", cer.path.display()),
            // The failing argv, so its own error can be read; a second publishing machine is
            // usually provisioned over SSH, where Finder is out of reach.
            fix: format!(
                "run it yourself and read the error: security import {} -k {kc} -T \
                 /usr/bin/codesign\n\
                 or import it by double-clicking in Finder, then re-run",
                cer.path.display()
            ),
        };
    }
    if let Err(e) = import_idempotent(&[
        "import",
        &key.to_string_lossy(),
        "-k",
        &kc,
        "-t",
        "priv",
        "-f",
        "openssl",
        "-T",
        "/usr/bin/codesign",
        "-T",
        "/usr/bin/security",
    ]) {
        return Outcome::Blocked {
            what: format!("could not import the private key: {e}"),
            // The exact command, never "import it by hand": a key imported without the -T
            // flags looks installed and then cannot sign unattended (securityd raises a
            // modal dialog the first time codesign touches it).
            fix: format!(
                "run it yourself and read the error: security import {} -k {kc} -t priv -f \
                 openssl -T /usr/bin/codesign -T /usr/bin/security\n\
                 the two -T flags are load-bearing — a key imported without them yields an \
                 identity that looks installed and then cannot sign unattended.",
                key.display()
            ),
        };
    }

    // A paired certificate and key still report no valid identity when the issuing
    // intermediate is absent (normal on a Mac that has never signed): supply it.
    let mut found = valid_identities().0;
    if found.is_empty() {
        match install_issuer_intermediate(cer) {
            Ok(Some(note)) => {
                step(APPLE_LABEL, &note);
                found = valid_identities().0;
            }
            Ok(None) => {}
            Err(e) => {
                return Outcome::Blocked {
                    what: format!(
                        "{} and its key imported and paired, but no valid identity \
                         appeared — the issuing intermediate is missing and could not be \
                         installed: {e}",
                        cer.path.display()
                    ),
                    // The form is known: `find_matching_cert` matched in that encoding.
                    fix: format!(
                        "name it: openssl x509 -inform {} -in {} -noout -issuer\n\
                         then install that intermediate from \
                         https://www.apple.com/certificateauthority/ and re-run",
                        cer.form,
                        cer.path.display()
                    ),
                };
            }
        }
    }
    if found.is_empty() {
        return Outcome::Blocked {
            what: format!(
                "{} and its key imported, but no valid identity appeared even with the \
                 issuing intermediate present",
                cer.path.display()
            ),
            // Both halves get a command; the key not being in this keychain is the likelier
            // cause.
            fix: format!(
                "check the dates:  openssl x509 -inform {} -in {} -noout -dates\n\
                 list what this keychain can actually sign with:  security find-identity \
                 -v -p codesigning {kc}\n\
                 an identity needs BOTH halves in ONE keychain — the certificate above is \
                 imported, so an empty list here means its private key is not in {kc}",
                cer.form,
                cer.path.display()
            ),
        };
    }

    // -T alone is not enough: the partition list is a second ACL dimension, and without it
    // securityd raises a modal dialog the first time `codesign` uses the key. Match by label
    // (`-l`, the common name the key takes once paired) on private keys: `-D` matches a
    // description, which private keys lack, so it would silently match nothing. Stdio is
    // inherited so `security` can ask for the login keychain password here; no `-k`,
    // because a password on argv shows in `ps`.
    let label =
        cert_common_name(&subject).unwrap_or_else(|| format!("Developer ID Application: {id}"));
    let _ = Command::new("/usr/bin/security")
        .args([
            "set-key-partition-list",
            "-S",
            "apple-tool:,apple:,codesign:",
            "-t",
            "private",
            "-s",
            "-l",
            &label,
            &kc,
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status();

    // Not asserted: measured by the same proof every other path uses.
    verdict(found)
}

/// `security import` fails on an item already present; a duplicate is success, so a re-run
/// after a half-finished install does not dead-end.
fn import_idempotent(args: &[&str]) -> Result<(), String> {
    match run("/usr/bin/security", args) {
        Ok(()) => Ok(()),
        Err(e) => {
            let lower = e.to_ascii_lowercase();
            if lower.contains("already exists") || lower.contains("duplicate") {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// Install the intermediate that issued this certificate, if the keychain lacks it.
///
/// Without the "Developer ID Certification Authority (G2)" intermediate, macOS cannot chain
/// the leaf to Apple Root CA: `find-identity -p codesigning` lists the identity and
/// `find-identity -v` refuses it, while `security verify-cert` passes (it fetches
/// intermediates over the network).
///
/// Trust is not widened: an intermediate is a chain link signed by Apple Root CA, and it is
/// imported only when provably the issuer of the operator's own leaf, by subject DN and by
/// issuing key. The key decides: Apple's two Developer ID generations share a common name,
/// so the DN only picks which published file to try first, and the leaf's Authority Key
/// Identifier (RFC 5280) must equal the candidate's Subject Key Identifier.
///
/// macOS's LibreSSL 3.3.6 has no `verify -partial_chain` and no `x509 -ext`, so both
/// identifiers are read out of `-text`.
fn install_issuer_intermediate(cer: &MatchedCert) -> Result<Option<String>, String> {
    /// Apple publishes both generations at fixed paths, and the leaf says which is which.
    const CA_URLS: [&str; 2] = [
        "https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer",
        "https://www.apple.com/certificateauthority/DeveloperIDCA.cer",
    ];
    let issuer = cert_field(&cer.path, cer.form, "-issuer")
        .ok_or("could not read the certificate's issuer")?;
    let issuer = issuer.trim_start_matches("issuer=").trim().to_string();
    if !issuer.contains("Developer ID Certification Authority") {
        return Ok(None);
    }
    // Already present? Then a missing intermediate is not what is wrong here.
    if keychain_has_subject(&issuer) {
        return Ok(None);
    }
    let want_key = cert_key_id(&cer.path, cer.form, AUTHORITY_KEY_ID).ok_or(
        "this certificate names no issuing key (no Authority Key Identifier), so which CA \
         issued it cannot be proved",
    )?;
    let keychain = login_keychain().ok_or("no login keychain")?;
    let dir = temp_run_dir("issuer")?;
    let outcome = (|| -> Result<Option<String>, String> {
        let mut urls = CA_URLS;
        if !issuer.contains("G2") {
            urls.swap(0, 1);
        }
        // Why each candidate was refused, so "could not reach apple.com" is not reported
        // as "Apple does not publish your issuer".
        let mut refused: Vec<String> = Vec::new();
        for url in urls {
            let name = url.rsplit('/').next().unwrap_or(url);
            let tmp = dir.join("issuer.cer");
            let _ = std::fs::remove_file(&tmp);
            let out = Command::new("/usr/bin/curl")
                .args([
                    "-fsSL",
                    "--proto",
                    "=https",
                    "--proto-redir",
                    "=https",
                    "--max-time",
                    "60",
                    "-o",
                    &tmp.to_string_lossy(),
                    url,
                ])
                .output()
                .map_err(|e| format!("curl: {e}"))?;
            if !out.status.success() {
                refused.push(format!("{name}: not downloadable"));
                continue;
            }
            // The encoding is proved, not assumed (as in [`find_matching_cert`]): a DER
            // guess about a PEM file would read as "a different issuer".
            let Some(form) = ["DER", "PEM"]
                .into_iter()
                .find(|f| cert_subject(&tmp, f).is_some())
            else {
                refused.push(format!("{name}: not a certificate"));
                continue;
            };
            let got = cert_field(&tmp, form, "-subject")
                .map(|s| s.trim_start_matches("subject=").trim().to_string());
            if got.as_deref() != Some(issuer.as_str()) {
                refused.push(format!("{name}: a different issuer name"));
                continue;
            }
            if cert_key_id(&tmp, form, SUBJECT_KEY_ID).as_deref() != Some(want_key.as_str()) {
                refused.push(format!("{name}: a different issuing key"));
                continue;
            }
            import_idempotent(&[
                "import",
                &tmp.to_string_lossy(),
                "-k",
                &keychain.to_string_lossy(),
            ])?;
            return Ok(Some(format!(
                "installed the missing issuing intermediate ({issuer}) — without it macOS \
                 cannot chain your certificate to Apple Root CA and reports the identity \
                 as invalid"
            )));
        }
        Err(format!(
            "no published Apple CA proved to be the issuer of this certificate ('{issuer}') \
             — {}",
            refused.join("; ")
        ))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

const AUTHORITY_KEY_ID: &str = "X509v3 Authority Key Identifier:";
const SUBJECT_KEY_ID: &str = "X509v3 Subject Key Identifier:";

/// One key identifier out of `openssl x509 -text`, normalised for comparison.
fn cert_key_id(cer: &Path, form: &str, header: &str) -> Option<String> {
    let out = capture(
        "/usr/bin/openssl",
        &[
            "x509",
            "-inform",
            form,
            "-in",
            &cer.to_string_lossy(),
            "-noout",
            "-text",
        ],
        None,
    )?;
    key_id_from_text(&String::from_utf8_lossy(&out), header)
}

/// The hex of the key identifier under `header`, uppercase and colon-free.
///
/// On LibreSSL 3.3.6 the value is on the line after the header, and the Authority form
/// prefixes it with `keyid:` (OpenSSL 3 drops the prefix on both). An AKI may be followed
/// by `DirName:`/`serial:` lines, so only the first value line counts, and it must be hex.
fn key_id_from_text(text: &str, header: &str) -> Option<String> {
    let mut lines = text.lines().skip_while(|l| !l.trim().starts_with(header));
    let header_line = lines.next()?;
    // OpenSSL has printed the value on the header line itself in some releases.
    let inline = header_line.trim().strip_prefix(header).unwrap_or("").trim();
    let raw = if inline.is_empty() {
        lines.next()?.trim()
    } else {
        inline
    };
    let hex: String = raw
        .trim_start_matches("keyid:")
        .trim()
        .chars()
        .filter(|c| *c != ':')
        .collect();
    (!hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_uppercase())
}

/// The same repair for a machine that already holds the certificate and lost the
/// intermediate (a login-keychain rebuild, a Keychain Access cleanup), which an import
/// never sees: each of our team's leaves is exported and run through
/// [`install_issuer_intermediate`]'s gate.
///
/// Errors are dropped: this is opportunistic, an unrelated certificate must not become a
/// Blocked verdict, and the branch table after it reports the real state either way.
fn repair_installed_chain() -> Option<String> {
    let pems = capture(
        "/usr/bin/security",
        &[
            "find-certificate",
            "-a",
            "-c",
            "Developer ID Application",
            "-p",
        ],
        None,
    )?;
    // `-c` matches on the common name alone, so other teams' blocks can be in this stream;
    // the team check below filters them.
    for block in String::from_utf8_lossy(&pems).split_inclusive("-----END CERTIFICATE-----") {
        if !block.contains("BEGIN CERTIFICATE") {
            continue;
        }
        let Ok(leaf) = temp_cert(block.as_bytes()) else {
            continue;
        };
        let cer = MatchedCert {
            path: leaf,
            form: "PEM",
        };
        let ours =
            cert_subject(&cer.path, cer.form).is_some_and(|s| s.contains(pins::APPLE_TEAM_ID));
        let note = if ours {
            install_issuer_intermediate(&cer).ok().flatten()
        } else {
            None
        };
        let _ = std::fs::remove_file(&cer.path);
        if note.is_some() {
            return note;
        }
    }
    None
}

/// A temp file nobody else can hold: `create_new` fails on an existing path or planted
/// symlink rather than overwriting it, and the pid in the name separates concurrent runs.
fn temp_cert(bytes: &[u8]) -> Result<PathBuf, String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = std::env::temp_dir();
    for n in 0..16u32 {
        let path = dir.join(format!(
            "aterm-provision-leaf-{}-{n}.pem",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(bytes)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Err("no free temp name".into())
}

/// Whether any certificate in the keychain has exactly this subject.
fn keychain_has_subject(subject: &str) -> bool {
    keychain_cert_with_subject(subject).is_some()
}

/// The Developer ID CA certificate in this keychain whose subject is exactly `subject`.
fn keychain_cert_with_subject(subject: &str) -> Option<String> {
    let pems = capture(
        "/usr/bin/security",
        &[
            "find-certificate",
            "-a",
            "-c",
            "Developer ID Certification Authority",
            "-p",
        ],
        None,
    )?;
    // One PEM block per certificate; compare each subject rather than trusting the
    // common-name filter, because both generations share a common name.
    String::from_utf8_lossy(&pems)
        .split_inclusive("-----END CERTIFICATE-----")
        .find(|block| {
            capture(
                "/usr/bin/openssl",
                &["x509", "-noout", "-subject"],
                Some(block.as_bytes()),
            )
            .map(|o| {
                String::from_utf8_lossy(&o)
                    .trim()
                    .trim_start_matches("subject=")
                    .trim()
                    .to_string()
            })
            .is_some_and(|s| s == subject)
        })
        .map(str::to_string)
}

/// One `openssl x509 -noout <field>` read of a certificate on disk.
fn cert_field(cer: &Path, form: &str, field: &str) -> Option<String> {
    let out = capture(
        "/usr/bin/openssl",
        &[
            "x509",
            "-inform",
            form,
            "-in",
            &cer.to_string_lossy(),
            "-noout",
            field,
        ],
        None,
    )?;
    Some(String::from_utf8_lossy(&out).to_string())
}

fn cert_subject(cer: &Path, form: &str) -> Option<String> {
    let out = capture(
        "/usr/bin/openssl",
        &[
            "x509",
            "-inform",
            form,
            "-in",
            &cer.to_string_lossy(),
            "-noout",
            "-subject",
        ],
        None,
    )?;
    Some(String::from_utf8_lossy(&out).to_string())
}

/// The common name out of an `openssl x509 -subject` line, in both dialects: LibreSSL
/// (macOS) prints `subject= /CN=Developer ID Application: Name (TEAM)/OU=TEAM/O=Name/C=US`,
/// OpenSSL 3 `subject=CN = Name, OU = ...`. A wrong label makes the partition-list call
/// match nothing.
fn cert_common_name(subject: &str) -> Option<String> {
    let after = subject
        .split_once("CN=")
        .or_else(|| subject.split_once("CN = "))
        .map(|(_, rest)| rest)?;
    let name = after
        .split(['/', ','])
        .next()
        .unwrap_or(after)
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// Sign a throwaway copy of a system binary with the identity, proving the ACL as well as
/// the pairing. A per-run directory and `create_new` on the file: a predictable path in a
/// world-writable TMPDIR lets a concurrent audit remove the copy before `codesign`, and
/// `fs::copy` follows a pre-created symlink.
fn prove_can_sign(sha1: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = temp_run_dir("probe")?;
    let probe = dir.join("probe");
    let signed = std::fs::read("/bin/echo")
        .map_err(|e| format!("could not read the probe source: {e}"))
        .and_then(|bytes| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(&probe)
                .and_then(|mut f| f.write_all(&bytes))
                .map_err(|e| format!("could not stage a probe: {e}"))
        })
        .and_then(|()| {
            run(
                "/usr/bin/codesign",
                &[
                    "--force",
                    "--timestamp=none",
                    "--sign",
                    sha1,
                    &probe.to_string_lossy(),
                ],
            )
        });
    let _ = std::fs::remove_dir_all(&dir);
    signed
}

/// A directory this run alone can hold: `create_dir` fails on an existing entry or planted
/// symlink, and the 0700 mode is set at creation, so a shared /tmp never lets anyone else
/// write inside. The pid separates concurrent runs; the counter steps over a directory a
/// killed run left behind.
fn temp_run_dir(what: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt as _;
    let base = std::env::temp_dir();
    for n in 0..16u32 {
        let path = base.join(format!("aterm-provision-{what}-{}-{n}", std::process::id()));
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Err(format!("no free temp name for {what}"))
}

/// The `--release-credentials` profile a Tier APPLE cut is given, written only when every
/// part of it is already true, so its existence means "this machine can cut".
///
/// `create_new`: an existing profile is never clobbered; it may hold a headless
/// `notary_password`, a hand-chosen `signing_identity_sha1` or a different key. The Ed25519
/// key is copied in because `sign.rs` reads `signing_key` only from here, so the file is
/// created 0600 with the mode (never readable for an instant).
pub(crate) fn write_credentials_profile(
    id: &str,
    roster: &Path,
    identity_sha1: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let Some(home) = std::env::var_os("HOME") else {
        return Err("HOME is not set".into());
    };
    let aterm = PathBuf::from(home).join(".aterm");
    let path = aterm.join("release-credentials.toml");
    if path.exists() {
        return Ok(None);
    }
    let key_bytes = std::fs::read(aterm.join("machine.key"))
        .map_err(|e| format!("cannot read this machine's key: {e}"))?;
    let signing_key = aterm_codec::base64::encode(&key_bytes)
        .map_err(|_| "this machine's key is too large to encode".to_string())?;

    let mut body = String::new();
    body.push_str(&format!(
        "# Written by `{} provision --id {id}`. 0600, owner-only: it carries this\n\
         # machine's release signing key. Name it with `{} --release-credentials`.\n\n",
        crate::publish::SHIP_COMMAND,
        crate::publish::CUT_COMMAND
    ));
    body.push_str(&format!("signing_key = \"{signing_key}\"\n"));
    body.push_str(&format!("machine_id = \"{id}\"\n"));
    body.push_str(&format!("machine_roster = \"{}\"\n", roster.display()));
    body.push_str(&format!("notary_profile = \"{}\"\n", NOTARY_PROFILE));
    if let Some(sha1) = identity_sha1 {
        body.push_str(&format!("signing_identity_sha1 = \"{sha1}\"\n"));
    }

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    f.write_all(body.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(Some(path))
}

fn login_keychain() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home)
        .join("Library")
        .join("Keychains")
        .join("login.keychain-db");
    p.is_file().then_some(p)
}

fn chmod(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Run a command for its exit status, capturing stderr for the error message.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(err
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("failed")
        .to_string())
}

/// Run a command with optional stdin, returning stdout on success.
fn capture(program: &str, args: &[&str], input: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(bytes) = input {
        child.stdin.take()?.write_all(bytes).ok()?;
    }
    let out = child.wait_with_output().ok()?;
    out.status.success().then_some(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LibreSSL (macOS) prints slash-separated RDNs; OpenSSL 3 prints `CN = …, OU = …`.
    #[test]
    fn the_common_name_parses_in_both_openssl_dialects() {
        let libressl = "subject= /CN=Developer ID Application: A Person (A66A9P66Z7)/OU=A66A9P66Z7/O=A Person/C=US";
        let openssl3 = "subject=CN = Developer ID Application: A Person (A66A9P66Z7), OU = A66A9P66Z7, O = A Person, C = US";
        let want = "Developer ID Application: A Person (A66A9P66Z7)";
        assert_eq!(cert_common_name(libressl).as_deref(), Some(want));
        assert_eq!(cert_common_name(openssl3).as_deref(), Some(want));
        assert!(cert_common_name("subject= /O=A Person/C=US").is_none());
    }

    #[test]
    fn nothing_outside_aterms_own_folder_is_read_unless_the_operator_named_it() {
        let own = Watch::new(None);
        let dirs = own.dirs();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        assert!(dirs[0].ends_with(".aterm/apple"), "{dirs:?}");
        assert!(
            !dirs
                .iter()
                .any(|d| d.to_string_lossy().contains("Downloads")),
            "{dirs:?}"
        );
        let named = Watch::new(Some(Path::new("/Users//x/Downloads")));
        let dirs = named.dirs();
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert_eq!(
            dirs[0],
            Path::new("/Users//x/Downloads"),
            "the named folder is looked in first"
        );
        assert!(dirs[1].ends_with(".aterm/apple"), "{dirs:?}");
    }

    /// Judged on the canonical path; nothing is said when nothing was named.
    #[test]
    fn the_consent_note_names_only_a_guarded_folder_the_operator_named() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME in tests"));
        assert!(Watch::new(None).consent_note().is_none());
        let guarded = Watch::new(Some(&home.join("Downloads")));
        let downloads_note = guarded
            .consent_note()
            .expect("Downloads is a guarded folder");
        assert!(
            downloads_note.contains("would like to access files"),
            "{downloads_note}"
        );
        // App Data (`~/Library/Containers/...`) gets its own wording: the dialog
        // says "would like to access data from other apps" and is asked on every
        // run (not one-time).
        let app_data = Watch::new(Some(
            &home.join("Library/Containers/com.aterm.nonexistent-test"),
        ));
        let app_data_note = app_data
            .consent_note()
            .expect("Library/Containers is a guarded App Data root");
        assert!(
            app_data_note.contains("would like to access data from other apps")
                && !app_data_note.contains("one-time"),
            "{app_data_note}"
        );
        let plain = Watch::new(Some(&home.join(".aterm").join("apple-scratch")));
        assert!(plain.consent_note().is_none());
        // Per component, never a string prefix: `Downloadsx` is not `Downloads`.
        let lookalike = Watch::new(Some(&home.join("Downloadsx")));
        assert!(lookalike.consent_note().is_none());
    }

    /// A renewal leaves two overlapping certificates, so selection is by digest, not order.
    #[test]
    fn the_certificate_is_selected_by_digest_not_by_position() {
        let stream = "SHA-256 hash: AAAA\nSHA-1 hash: 1111111111111111111111111111111111111111\n\
                      -----BEGIN CERTIFICATE-----\nolder\n-----END CERTIFICATE-----\n\
                      SHA-256 hash: BBBB\nSHA-1 hash: 2222222222222222222222222222222222222222\n\
                      -----BEGIN CERTIFICATE-----\nnewer\n-----END CERTIFICATE-----\n";
        let second = leaf_pem_by_sha1(stream, "2222222222222222222222222222222222222222")
            .expect("the second certificate");
        assert!(second.contains("newer"), "{second}");
        assert!(!second.contains("older"), "{second}");
        // `find-identity` prints the digest uppercase; the comparison may not care.
        assert!(
            leaf_pem_by_sha1(stream, "1111111111111111111111111111111111111111")
                .is_some_and(|p| p.contains("older"))
        );
        assert!(leaf_pem_by_sha1(stream, &"3".repeat(40)).is_none());
    }

    /// Verbatim LibreSSL 3.3.6 `x509 -text` over the Developer ID leaf and its G2 issuer:
    /// the authority form carries a `keyid:` prefix, the subject form does not.
    #[test]
    fn the_issuing_key_is_read_out_of_both_identifier_forms() {
        let leaf = "        X509v3 extensions:\n            \
                    X509v3 Authority Key Identifier: \n                \
                    keyid:F8:3A:0C:69:11:76:E0:ED:AC:D1:EB:A6:59:FA:37:D5:C4:55:B0:1E\n\n            \
                    X509v3 Subject Key Identifier: \n                \
                    7A:7C:DC:1E:A9:B4:E0:95:1A:80:1B:6C:69:46:8D:94:C2:6D:61:57\n            \
                    X509v3 Key Usage: critical\n";
        assert_eq!(
            key_id_from_text(leaf, AUTHORITY_KEY_ID).as_deref(),
            Some("F83A0C691176E0EDACD1EBA659FA37D5C455B01E")
        );
        assert_eq!(
            key_id_from_text(leaf, SUBJECT_KEY_ID).as_deref(),
            Some("7A7CDC1EA9B4E0951A801B6C69468D94C26D6157")
        );
        // A certificate carrying neither is not one this code may guess about.
        assert!(
            key_id_from_text("X509v3 Basic Constraints: critical\n", AUTHORITY_KEY_ID).is_none()
        );
    }

    /// `openssl x509 -noout -enddate` output, read as the date the operator would diarise.
    #[test]
    fn the_expiry_date_reads_as_iso() {
        assert_eq!(
            enddate_iso("notAfter=Aug 18 16:36:18 2031 GMT\n").as_deref(),
            Some("2031-08-18")
        );
        assert_eq!(
            enddate_iso("notAfter=Sep  1 00:00:00 2027 GMT").as_deref(),
            Some("2027-09-01")
        );
        assert!(enddate_iso("notBefore=Aug 17 16:36:19 2026 GMT").is_none());
    }
}
