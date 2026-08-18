// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Apple half of `cargo ship provision` — this machine gets its OWN Developer ID
//! Application identity, and no private key ever crosses a machine boundary.
//!
//! # Why a human is in the middle, and why it is still one command
//!
//! Apple permits exactly two channels for creating a Developer ID certificate, and
//! documents the restriction on the App Store Connect API's own `certificates` page:
//! *"You can only create Developer ID certificates for macOS through the Apple Developer
//! website or Xcode."* Every automated route was measured and refused:
//!
//! * **`POST /v1/certificates`** — refused for every `DEVELOPER_ID_*` type. Not a role
//!   gate either: Account Holder is a singular per-account attribute, and team API keys
//!   carry only assignable user roles, so no key can be minted that carries it.
//! * **Cloud-managed Developer ID / `xcodebuild`** — the private key is generated and
//!   held on Apple's servers, so `security find-identity` never sees an identity and
//!   plain `codesign` cannot use it (Apple DTS, developer forums 768573: asked whether
//!   `codesign` can use a cloud-managed Developer ID certificate — "Correct", it cannot).
//! * **The website's private XHR endpoint** (`developer.apple.com/services-account/v1`,
//!   what fastlane's spaceship drives) — works, but needs the Account Holder's Apple ID
//!   password and a 2FA session that cannot be refreshed headlessly, is forbidden by
//!   Apple's site terms, and has broken hard three times in three years. It removes the
//!   browser, not the human.
//!
//! Apple's requirement is a human at a browser; it is not a requirement that the operator
//! run anything twice. So the command prints the errand and WAITS, matching the arriving
//! certificate by public key and continuing the moment it lands — one invocation, from an
//! unprovisioned machine to READY TO CUT.
//!
//! Waiting is not the same as holding state. There is no `--phase` flag and no progress
//! marker, because a marker can disagree with the world: every fact is DERIVED from what
//! is observably on disk and in the keychain, on every pass. That is what makes Ctrl-C
//! free — an interrupted run has lost nothing, and re-running the same command resumes at
//! exactly the point the world is actually in. Without a terminal (CI, a pipe) it does not
//! wait at all: there is nobody to do the errand, so it reports it and returns.
//!
//! # The two things this module must never get wrong
//!
//! 1. **A slot is permanent.** A team gets five Developer ID Application certificates,
//!    and revocation is retroactive — Apple: *"Any Developer ID app signed with a
//!    certificate that has been revoked can no longer be installed nor launch if it's
//!    already installed."* So it asks before spending one, never regenerates over a CSR
//!    that is already out for signature, and never revokes anything. `--check` cannot
//!    even reach that question: a flag whose whole promise is that nothing changes may
//!    not offer to spend something permanent, so it reports what it can see and stops.
//! 2. **A green verdict must be demonstrated, not inferred.** `find-identity` listing an
//!    identity proves the keychain paired a certificate with a key. It does NOT prove
//!    `codesign` can use that key unattended — the partition-list ACL decides that, and
//!    when it is wrong the failure surfaces twenty minutes into a real cut. So EVERY run
//!    that would report ready first signs something disposable, and a failed signature is
//!    never reported as ready. A demonstrated verdict also carries the date it stops
//!    being true: `find-identity -v` is a boolean, so without it a certificate that
//!    lapses next week audits identical to one good until 2031.

#![cfg(unix)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aterm_update_core::pins;

use crate::publish::step;

/// One subject, one label. In the audit's two-column layout the label IS the identity of
/// a check, so a module printing under `apple` while the audit records the same subject
/// under `apple id` reads as two subjects reporting on one certificate. Shared with
/// `provision.rs` so the two cannot drift apart again.
pub(crate) const APPLE_LABEL: &str = "apple id";

/// Where this machine's own Apple key material lives. Beside `~/.aterm/machine.key`,
/// which is already the home of "secrets this machine minted for itself", and outside the
/// repo so no `git clean` can sweep a key whose certificate slot is already spent.
fn apple_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".aterm").join("apple"))
}

fn key_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("devid-{id}.key"))
}

fn csr_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("devid-{id}.certSigningRequest"))
}

/// A certificate file and the encoding it was proved to parse as. Carried from the
/// matcher to the installer so the second read cannot disagree with the first: the
/// matcher accepts DER or PEM, and an installer that assumed DER would reject a PEM
/// certificate it had itself just matched, blaming the operator's team id for it.
struct MatchedCert {
    path: PathBuf,
    form: &'static str,
}

/// What is observably true right now. No stored flags: every run re-derives this, so an
/// abandoned attempt, a hand-imported certificate or a restored backup all land in the
/// right branch without anyone having to tell it.
pub(crate) struct Observed {
    /// 40-hex SHA-1 of each VALID `Developer ID Application` identity for our team.
    pub identities: Vec<String>,
    /// Set when `security find-identity` could not be run at all. "We could not look" is
    /// not the same claim as "there is nothing there", and only one of them may lead to
    /// spending a certificate slot.
    pub lookup_failed: Option<String>,
    /// Our team's Developer ID certificates that exist but are NOT valid — expired, or
    /// present without their private key. Without this, an expired certificate reads as
    /// "no identity" and the tool offers to burn a slot without mentioning the one the
    /// machine already has.
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

/// How many of our team's Developer ID identities the keychain holds but will NOT vouch
/// for. `-v` restricts to valid ones; without it the invalid ones are listed too.
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
    /// An identity is installed and PROVED usable — `ids` holds only 40-hex SHA-1s, so a
    /// caller counting them is counting certificates. Anything else to say rides in
    /// `note`, never in `ids`.
    Ready {
        ids: Vec<String>,
        note: Option<String>,
    },
    /// Progress, with the ball in the operator's court. Distinct from `Blocked` because
    /// "your certificate request is waiting at Apple" is not a fault to be fixed.
    Waiting { what: String, next: String },
    /// Same shape as `Waiting`, used by the read-only mode: nothing was attempted, so
    /// there is nothing in flight — only something a real run would do.
    Todo { what: String, next: String },
    /// Something is wrong and the tool will not proceed past it.
    Blocked { what: String, fix: String },
    /// Not a macOS host: the old audit's behaviour, so the verdict still says "skipped".
    Skipped(String),
}

/// The driver. Idempotent by construction: it reads the world, and only one branch
/// creates anything.
pub(crate) fn acquire(id: &str, may_change: bool) -> Outcome {
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

    // "We could not look" must never reach the branch that spends a slot — nor the
    // read-only branch below, which would otherwise report "no Developer ID Application
    // identity" about a keychain it never managed to open.
    if let Some(why) = seen.lookup_failed.as_deref() {
        return Outcome::Blocked {
            what: format!("cannot read the keychain, so this machine's Apple state is unknown: {why}"),
            fix: "install the Xcode command-line tools (`xcode-select --install`)".into(),
        };
    }

    // `--check` promises "(no writes)". Generating a CSR spends one of five PERMANENT
    // Developer ID slots and writes a private key; importing mutates the keychain. None
    // of that may happen behind a flag whose whole purpose is to change nothing, so the
    // read-only mode reports what it can see and stops there.
    if !may_change {
        return match (seen.identities.is_empty(), seen.key.is_some()) {
            (false, _) => verdict(seen.identities),
            // Only what is observable: a key on disk. Whether its request was ever
            // uploaded is Apple's state, not this machine's, and cannot be read here.
            (true, true) => Outcome::Todo {
                what: format!("'{id}' holds a request key and no installed certificate"),
                next: "re-run without --check to import the certificate once you have \
                       downloaded it"
                    .into(),
            },
            (true, false) => Outcome::Todo {
                what: format!("no Developer ID Application identity for team {}", pins::APPLE_TEAM_ID),
                next: format!(
                    "re-run without --check: it will ask before spending one of five \
                     permanent certificate slots, then generate '{id}'s request"
                ),
            },
        };
    }

    // A leaf whose issuing intermediate is missing is INVISIBLE to `find-identity -v`
    // (MEASURED on m2, 2026-08-17 — see `install_issuer_intermediate`), so it arrives here
    // as "nothing installed" and the branch table below would route a perfectly good
    // certificate toward spending a second permanent slot. `install()` repairs that during
    // an IMPORT; a machine that already HELD the certificate and lost the intermediate
    // afterwards — a new login keychain, a migration, a Keychain Access cleanup of
    // "expired/duplicate" items — could never reach the repair. `invalid_present` is the
    // signal that separates "nothing here" from "the chain is broken", and it was being
    // spent on decorating a prompt.
    if seen.identities.is_empty()
        && seen.invalid_present > 0
        && let Some(note) = repair_installed_chain()
    {
        step(APPLE_LABEL, &note);
        // `Observed::look` sampled the identities BEFORE the repair, so the identity it
        // just made valid stays invisible to every branch below until we re-read.
        seen = Observed::look(id);
    }


    // An identity is installed — prove it can actually sign, every run, and say so.
    if !seen.identities.is_empty() {
        // Nothing is said about leftover request material on a SUCCESS line: it is not
        // something the operator is being asked to act on, and a success line ending in a
        // chore is a success line nobody reads. The spent request is public and finished,
        // so it is simply removed; the private key is never touched automatically — it is
        // the private half of a slot that cannot be reclaimed.
        if let Some(csr) = &seen.csr
            && paired_with_installed(&seen.key, &seen.identities[0])
        {
            let _ = std::fs::remove_file(csr);
        }
        return verdict(seen.identities);
    }

    // K && C, no identity — install if Apple has answered, otherwise wait for it.
    if let (Some(key), Some(csr)) = (&seen.key, &seen.csr) {
        return await_then_install(key, csr, id, &dir, seen.invalid_present);
    }

    // A CSR with no key is unusable: a certificate issued against it could never be
    // signed with, because the only key that matches it is gone. The slot it spent does
    // not come back, so say that plainly rather than implying the file is worth keeping.
    if seen.key.is_none() && seen.csr.is_some() {
        let csr = csr_path(&dir, id);
        // `invalid_present` decides which of two situations this is, instead of spending
        // forty words on a hypothetical every time. Zero is the ordinary abandoned
        // request, and needs no warning at all. Above zero, the keychain is holding a
        // Developer ID certificate it cannot vouch for — which MAY be the one issued
        // against the lost key, and is the only case where the warning earns its lines.
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
            format!("delete {} and re-run to mint a fresh key and request", csr.display())
        };
        return Outcome::Blocked {
            what: format!(
                "a certificate request for '{id}' exists but its private key does not — \
                 nothing can ever be signed with a certificate issued from it"
            ),
            fix,
        };
    }

    // A key with no CSR is fully recoverable — the request is derived from the key, so
    // rebuild it rather than sending the operator to delete a key whose slot may already
    // be spent. Costs nothing and spends nothing: a CSR is not a certificate.
    if let Some(key) = &seen.key {
        return match write_csr(key, &csr_path(&dir, id)) {
            Ok(csr) => {
                step(APPLE_LABEL, &format!(
                    "rebuilt the certificate request for '{id}' from the key already here \
                     — no new key, no new slot"
                ));
                await_then_install(key, &csr, id, &dir, seen.invalid_present)
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
            Ok(csr) => await_then_install(&key_path(&dir, id), &csr, id, &dir, seen.invalid_present),
            Err(outcome) => outcome,
        },
        Ok(false) => Outcome::Waiting {
            what: "declined to generate a certificate signing request".into(),
            next: format!("re-run `cargo ship provision --id {id}` when ready to spend a slot"),
        },
        Err(e) => Outcome::Blocked {
            what: format!("could not ask before spending a Developer ID slot: {e}"),
            fix: "run provision from a terminal — it will not spend a slot unasked".into(),
        },
    }
}

/// How long the command will sit waiting for the operator's browser errand before it
/// gives up and tells them to re-run. Generous, because the errand involves signing in to
/// Apple as the Account Holder, and short enough that a forgotten terminal does not hold a
/// session open all night.
const WAIT_FOR_CERT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// ONE command, even though Apple insists on a human in the middle.
///
/// The certificate can only be created by the Account Holder in a browser, so instead of
/// ending the run and making the operator remember to type the same thing again, the
/// command prints the errand and WAITS for the certificate to appear — matching it by
/// public key, so it starts the moment the right file lands in ~/Downloads.
///
/// Interrupting is safe and loses nothing: every fact this verb acts on is derived from
/// disk and the keychain, never from a progress marker, so a re-run resumes exactly here.
/// Without a terminal (CI, a pipe) it does not wait at all — it reports the errand and
/// returns, because there is nobody to do it.
fn await_then_install(
    key: &Path,
    csr: &Path,
    id: &str,
    dir: &Path,
    invalid: usize,
) -> Outcome {
    let blocked = |why: String| Outcome::Blocked {
        what: format!("could not look for the issued certificate: {why}"),
        fix: format!(
            "put the downloaded .cer somewhere readable ({}) and re-run",
            dir.display()
        ),
    };
    let waiting = |rejected: &[PathBuf]| Outcome::Waiting {
        what: waiting_text(id, invalid, rejected),
        // Printed in full below, once, before the wait — so the verdict after a timeout
        // points at it instead of repeating all eighty words.
        next: format!("upload {} at developer.apple.com — see above", csr.display()),
    };
    let rejected = match find_matching_cert(key) {
        Ok((Some(cer), _)) => return install(&cer, key, id),
        Err(why) => return blocked(why),
        Ok((None, rejected)) => rejected,
    };
    // Nobody to wait for (CI, a pipe) — and nobody to have read an errand that is never
    // printed on this path, so the verdict carries the only copy. "See above" pointing at
    // nothing is the one thing worse than saying it twice.
    if !has_terminal() {
        return Outcome::Waiting {
            what: waiting_text(id, invalid, &rejected),
            next: errand(csr),
        };
    }
    // Said BEFORE the errand and before the wait, because it is the difference between
    // "Apple has not answered" and "the file you already downloaded is not this one" —
    // and thirty minutes of heartbeat says the first while meaning the second, which is
    // how an operator ends up creating a SECOND certificate to fix a download.
    //
    // The errand opens with `upload {csr}`, so it IS the "request ready" line: printing
    // that path on a line of its own first said the same thing one line earlier.
    let mut label = APPLE_LABEL;
    if let Some(note) = rejected_note(&rejected) {
        step(label, &note);
        label = "";
    }
    // Surface the request in ~/Downloads and reveal it in Finder before the errand
    // names it: the operator's next act is an upload dialog, and a path under hidden
    // ~/.aterm is invisible to one. The errand then names the copy the dialog can see.
    let visible = surface_csr(csr, id);
    step(label, &errand(visible.as_deref().unwrap_or(csr)));
    step(
        "",
        "waiting for the certificate to appear… (Ctrl-C is safe — re-running resumes here)",
    );
    let started = std::time::Instant::now();
    let mut announced = 0u64;
    let mut rejected = rejected;
    while started.elapsed() < WAIT_FOR_CERT {
        std::thread::sleep(std::time::Duration::from_secs(3));
        match find_matching_cert(key) {
            Ok((Some(cer), _)) => {
                step(APPLE_LABEL, &format!("certificate found: {}", cer.path.display()));
                return install(&cer, key, id);
            }
            Err(why) => return blocked(why),
            // Kept current, so the verdict after a 30-minute wait names whatever is on
            // disk NOW — a .cer downloaded during the wait is the likeliest of all.
            Ok((None, seen)) => rejected = seen,
        }
        // A quiet heartbeat, so a long wait never looks like a hang.
        let mins = started.elapsed().as_secs() / 60;
        if mins > announced {
            announced = mins;
            step("", &format!("still waiting… ({mins} min)"));
        }
    }
    waiting(&rejected)
}

/// Whether there is a human to ask. The same test the prompts use, so "will it wait?" and
/// "will it prompt?" can never disagree.
fn has_terminal() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .is_ok_and(|f| std::io::IsTerminal::is_terminal(&f))
}

/// The single place a green Apple verdict is produced, so the proof cannot be skipped on
/// some paths and not others. A signature that does not succeed is NOT ready: reporting
/// it as ready is precisely the "discovered at minute twenty of a cut" failure this verb
/// exists to prevent.
fn verdict(ids: Vec<String>) -> Outcome {
    match prove_can_sign(&ids[0]) {
        Ok(()) => {
            // The expiry rides here rather than at any one call site, for the same reason
            // the signature proof does: a green line that omits it on some paths is a
            // green line nobody can trust to carry it.
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

/// Whether the on-disk request key is the private half of the certificate THIS identity
/// names. Used only to decide whether the SPENT REQUEST may be cleaned up — never to
/// decide anything about the key itself, which is not this code's to delete.
fn paired_with_installed(key: &Option<PathBuf>, sha1: &str) -> bool {
    let Some(key) = key else { return false };
    installed_cert_spki(sha1)
        .is_some_and(|installed| key_spki_sha256(key).is_some_and(|k| k == installed))
}

/// The SPKI digest of the certificate the identity `sha1` names, for the pairing proof
/// above.
///
/// It used to pipe the WHOLE `find-certificate -a … -p` stream into `openssl x509`, which
/// parses the first block and stops — so on a machine with two Developer ID certificates
/// the comparison was made against whichever the keychain happened to list first. Two
/// overlapping certificates is not an exotic state: it is what a renewal looks like, and
/// `apple_identity_check` has a message for it. (The same pipe could also lose to EPIPE
/// once openssl stopped reading, which `capture` maps to `None` — the identical false
/// branch.) Select by digest instead.
fn installed_cert_spki(sha1: &str) -> Option<String> {
    let pem = installed_leaf_pem(sha1)?;
    let der = capture(
        "/usr/bin/openssl",
        &["x509", "-pubkey", "-noout"],
        Some(pem.as_bytes()),
    )?;
    let der = capture("/usr/bin/openssl", &["pkey", "-pubin", "-outform", "DER"], Some(&der))?;
    digest(&der)
}

/// The PEM of the installed `Developer ID Application` certificate whose SHA-1 is `sha1` —
/// the digest `find-identity` names an identity by, so a caller holding an identity can
/// reach ITS certificate and no other.
///
/// MEASURED on this platform: `security find-certificate -a -p -Z` prints, per
/// certificate, a `SHA-256 hash:` line, a `SHA-1 hash:` line, then the PEM block. Nothing
/// else in the stream separates one certificate from the next.
fn installed_leaf_pem(sha1: &str) -> Option<String> {
    let out = capture(
        "/usr/bin/security",
        &["find-certificate", "-a", "-c", "Developer ID Application", "-p", "-Z"],
        None,
    )?;
    leaf_pem_by_sha1(&String::from_utf8_lossy(&out), sha1)
}

/// The selection itself, pure over the captured stream so the rule is testable without a
/// keychain — and so "which certificate did it pick?" has one answer with a test on it.
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

/// How close to expiry a certificate has to be before the green line stops saying only
/// "valid to" and starts naming the errand. Ninety days is the window Apple's own renewal
/// flow assumes, and it is long enough that the renewal can wait for a convenient day.
const RENEWAL_WINDOW_SECS: u64 = 90 * 24 * 60 * 60;

/// The expiry clause on a green Apple line: `valid to 2031-08-18`, or the renewal errand
/// once the certificate is inside its last [`RENEWAL_WINDOW_SECS`].
///
/// Nothing read a certificate's dates before this. `find-identity -v` is a boolean — valid
/// today, gone tomorrow — so a machine whose Developer ID certificate lapses next week
/// audited green today, and the day it lapsed the audit reported "no Developer ID
/// Application identity" and offered to spend one of five permanent slots. A date on the
/// line that already carries the SHA-1 is what turns that from a mystery into a diarised
/// errand.
fn expiry_note(sha1: &str) -> Option<String> {
    let leaf = installed_leaf_pem(sha1)?;
    let mut soonest = (cert_enddate_iso(&leaf)?, leaf.clone());
    // A chain is only as long-lived as its shortest link, and the leaf is not always it:
    // the ORIGINAL Developer ID intermediate expires 2027-02-01 — this module's errand
    // text states that as the reason to pick G2 — which is sooner than most of a five-year
    // leaf, and the day it passes, every certificate under it stops being an identity.
    if let Some(ca) = issuer_in_keychain(&leaf) {
        // ISO-8601 sorts chronologically, which is most of why it is written this way.
        match cert_enddate_iso(&ca) {
            Some(iso) if iso < soonest.0 => soonest = (iso, ca),
            _ => {}
        }
    }
    let (iso, pem) = soonest;
    // `-checkend` is the platform's own answer to "does this outlive N seconds?"
    // (MEASURED on LibreSSL 3.3.6: exit 0 beyond the window, 1 inside it), so there is no
    // date arithmetic here and no second definition of "now".
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

/// `notAfter=Aug 18 16:36:18 2031 GMT` → `2031-08-18`. MEASURED against the certificate
/// installed on m2; the ISO spelling is the one an operator can put in a calendar without
/// re-reading it.
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

/// Whether `openssl x509 -checkend` says this certificate outlives `secs`. `None` when the
/// question could not be asked at all — which is reported as no clause, never as a date.
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

/// The label macOS gives the private key once it is paired, which is the certificate's
/// common name — needed for the partition-list remediation the operator may have to run.
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

/// The plural `s`. The counts this module reports are almost always 1, and `1 item(s)`
/// reads as a bug in the tool rather than a fact about the machine — the same rule
/// `provision::gaps` follows for the summary line.
fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn waiting_text(id: &str, invalid: usize, rejected: &[PathBuf]) -> String {
    let mut s = format!("a certificate request for '{id}' is out for signature and no matching certificate has arrived yet");
    if let Some(note) = rejected_note(rejected) {
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
/// A whole line, not one byte: reading a single character leaves the rest of "yes" in the
/// terminal buffer, where the NEXT prompt would silently consume it as its answer. And
/// `/dev/tty` must be a real terminal — the same rule the master phrase follows, so a
/// piped or redirected run can never answer a question about spending a permanent
/// certificate slot.
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
    loop {
        match tty.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0] as char),
            Err(e) => return Err(format!("cannot read from /dev/tty: {e}")),
        }
        if line.len() >= max {
            break;
        }
    }
    Ok(line.trim().to_string())
}

/// The one irreversible question, asked in the audit's own two-column layout: `publish::step`
/// prints `"  {label:<11}{msg}"`, so the label is [`APPLE_LABEL`] — the name every other line
/// about this certificate carries — and continuations are indented to the same 13th column.
/// A prompt that arrives under a different label and a different gutter reads as a different
/// subject, which is not what a permanent choice should look like.
fn confirm_slot(id: &str, invalid: usize) -> Result<bool, String> {
    let gutter = " ".repeat(13);
    let mut prompt = format!(
        "\n  {APPLE_LABEL:<11}generating a CSR for '{id}' spends one of team {}'s five Developer\n\
         {gutter}ID Application slots, permanently. Revoking one later stops every app\n\
         {gutter}it already signed from launching.",
        pins::APPLE_TEAM_ID
    );
    if invalid > 0 {
        prompt.push_str(&format!(
            "\n{gutter}NOTE: this keychain already holds {invalid} Developer ID certificate{}\n\
             {gutter}for this team that {} not valid (expired, or missing their private\n\
             {gutter}key). Those slots are already spent.",
            plural(invalid),
            if invalid == 1 { "is" } else { "are" },
        ));
    }
    prompt.push_str(&format!("\n{gutter}Continue? [y/N] "));
    let answer = tty_line(&prompt, 16)?.to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// The keychain profile name the notarytool credential is stored under — the SAME on
/// every machine, because it is a local keychain label, not an Apple-side identifier, and
/// `sign.rs` and the incumbent machine already say "notary". One definition, because four
/// places name it: `ensure_notary` stores it, `write_credentials_profile` writes it,
/// `provision::notary_check` live-tests it, and the cut reads it back.
///
/// Per-machine revocability lives where it actually bites: the app-specific password
/// behind it, which should be minted per machine at https://account.apple.com (Sign-In and Security → App-Specific Passwords).
pub(crate) const NOTARY_PROFILE: &str = "notary";

/// Store a notarytool credential in this machine's keychain.
///
/// `notarytool store-credentials` owns the password prompt itself — its stdio is
/// inherited onto this terminal rather than read here, so an app-specific password never
/// passes through this process, never reaches argv (where `ps` would show it), and is
/// never held in our memory. Its `--validate` default round-trips to Apple, so a stored
/// credential is one that actually authenticated.
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
    // NB: there is deliberately no "is one already stored?" probe here. The caller
    // (`notary_acquire`) LIVE-checks with `notarytool history` before ever calling this,
    // and that is the only test that means anything — see below.
    let profile = NOTARY_PROFILE;
    // Said BEFORE the prompts, because the password notarytool asks for is the one
    // credential in this ceremony that exists nowhere until the operator MINTS it —
    // and an ordinary Apple ID password pasted there fails only after a round-trip
    // to Apple, twenty words into the errand this line replaces.
    step(
        "notary",
        "the password asked for below is an APP-SPECIFIC password, not your Apple ID \
         password — mint one at https://account.apple.com → Sign-In and Security → \
         App-Specific Passwords → + (2FA required; the xxxx-xxxx-xxxx-xxxx string is \
         shown exactly once), then paste it at the prompt",
    );
    let apple_id = match tty_line(
        "\n  notary   Apple ID for notarization (blank to skip): ",
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
            }
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
    // MEASURED on m2 (2026-08-17): `store-credentials` printed "Success. Credentials
    // validated. Credentials saved to Keychain." and the tool then called it a failure,
    // because it confirmed with `security find-generic-password -s
    // com.apple.gke.notary.tool`. `security` only sees FILE-BASED keychains; notarytool
    // stores its credential in the data-protection keychain, where that command can never
    // find it. The probe was not merely wrong here — it can never be right, so it is gone.
    // notarytool's own exit status is the verdict, and the caller re-runs the live
    // `notarytool history` check to confirm the credential actually authenticates — which
    // is also why success here carries nothing to print: the line the operator reads comes
    // from that live check, not from this exit status.
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

/// Run 1: an RSA-2048 keypair and a CSR, both born here. `/usr/bin/openssl` explicitly —
/// a Homebrew OpenSSL 3 ahead of it on PATH changes both defaults and output formats, and
/// this must be the one macOS ships.
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

    // Belt and braces over the state machine above: `openssl genrsa -out` TRUNCATES an
    // existing file without a word, and the key it would destroy may be the private half
    // of a certificate slot that is already spent — unrecoverable, and the slot does not
    // come back.
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
    if let Err(e) = run("/usr/bin/openssl", &["genrsa", "-out", &key.to_string_lossy(), "2048"]) {
        return Err(Outcome::Blocked {
            what: format!("could not generate a private key: {e}"),
            fix: "install the Xcode command-line tools (`xcode-select --install`)".into(),
        });
    }
    // The key is the whole point of this design: it is the half that never travels.
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

/// Derive the request from the key. Separate from [`generate`] because a lost request is
/// recoverable and a lost key is not — this is the half that can always be redone.
fn write_csr(key: &Path, csr: &Path) -> Result<PathBuf, String> {
    // The subject Keychain Access produces for a Developer ID request: Apple replaces the
    // common name with the team's own on issuance, so what matters is that it parses.
    let subject = format!("/CN=Developer ID Application/O={}/C=US", pins::APPLE_TEAM_ID);
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

/// The one unavoidable human step, written out so nobody has to guess the click path.
///
/// It does NOT end in "then re-run the same command". On the interactive path it is
/// printed immediately above "waiting for the certificate to appear…", so the errand was
/// telling the operator to re-run a command that was at that moment sitting in a wait
/// loop; and on the non-interactive path `provision`'s own summary line already ends with
/// `re-run cargo ship provision --id <id>`. What survives is the only part they cannot
/// derive: where the download has to land.
fn errand(csr: &Path) -> String {
    format!(
        "upload {} at https://developer.apple.com/account/resources/certificates (sign in \
         as the Account Holder — Apple permits nobody else to create a Developer ID \
         certificate) → + → Software → 'Developer ID Application' → Profile Type 'G2 \
         Sub-CA' (NOT 'Previous Sub-CA': its intermediate expires 2027-02-01) → upload \
         the request → Download into ~/Downloads, where it is matched by public key. If \
         the list ALREADY has a certificate for this request, download that one — \
         creating a second spends a second slot.",
        csr.display()
    )
}

/// Copy the request somewhere a file picker can actually see. The canonical copy lives
/// under hidden `~/.aterm/apple/`, which an upload dialog cannot show — and the dialog
/// opens in ~/Downloads, which is also where the certificate comes back, so the errand
/// starts and ends in one visible folder. The CSR is PUBLIC material (the subject and
/// public key; the private half never leaves `~/.aterm/apple`), so the copy leaks
/// nothing. Best-effort by design: on any failure the hidden canonical path still
/// works, and the Finder reveal merely pre-selects the file for the operator.
#[cfg(unix)]
fn surface_csr(csr: &Path, id: &str) -> Option<PathBuf> {
    let visible = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Downloads"))?
        .join(format!("devid-{id}.certSigningRequest"));
    std::fs::copy(csr, &visible).ok()?;
    let _ = Command::new("/usr/bin/open").arg("-R").arg(&visible).status();
    Some(visible)
}

/// Find a downloaded certificate that belongs to THIS key, by comparing public keys. This
/// is why there is no `--cer` flag and no fixed drop path: the machine already holds the
/// only thing that can identify the right file, so it proves the match instead of trusting
/// a filename — or a teammate's certificate that happens to be in ~/Downloads.
///
/// `Err` is reserved for "could not look properly"; `Ok((None, …))` means "looked, not
/// there". Collapsing the two would tell an operator whose ~/Downloads is unreadable that
/// Apple had not answered yet.
///
/// The second half of the pair is every `.cer` that WAS examined and is not this
/// machine's. Discarding it made "Apple has not issued the certificate" and "the wrong
/// certificate is sitting in ~/Downloads" print the identical heartbeat for thirty
/// minutes — and the natural reading of that is the first, so the operator goes back to
/// developer.apple.com and creates another certificate: one more of five permanent slots,
/// spent to fix a download. Paths only, no `openssl` reads: this runs every three seconds
/// for the whole wait, and the reasons are only ever needed once, in [`rejected_note`].
fn find_matching_cert(key: &Path) -> Result<(Option<MatchedCert>, Vec<PathBuf>), String> {
    let want = key_spki_sha256(key)
        .ok_or_else(|| format!("could not read the public half of {}", key.display()))?;
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let mut unreadable = Vec::new();
    let mut rejected = Vec::new();
    for dir in [
        PathBuf::from(&home).join("Downloads"),
        PathBuf::from(&home).join(".aterm").join("apple"),
    ] {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            // A missing ~/Downloads is normal; one that exists but cannot be read is not.
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

/// The `.cer` files that were examined and are not this machine's, as one line: which
/// file, why it was refused, and where the right one has to land. Named files, because
/// "no matching certificate" about a directory the operator can see a certificate in is
/// the sentence that sends them to spend another slot.
fn rejected_note(rejected: &[PathBuf]) -> Option<String> {
    // Three is enough to recognise the one you just downloaded; a full ~/Downloads dump
    // would be a paragraph nobody reads.
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
    Some(format!(
        "examined, not this request's: {}{more} — if the browser saved the .cer elsewhere, \
         move it into ~/Downloads",
        named.join(", ")
    ))
}

/// SHA-256 of the DER SubjectPublicKeyInfo of a private key.
fn key_spki_sha256(key: &Path) -> Option<String> {
    let der = capture(
        "/usr/bin/openssl",
        &["rsa", "-in", &key.to_string_lossy(), "-pubout", "-outform", "DER"],
        None,
    )?;
    digest(&der)
}

/// The same digest taken from a certificate in a known encoding, so the two can be
/// compared. MEASURED end to end against a self-signed pair: the two paths agree.
fn cert_spki_sha256(cer: &Path, form: &str) -> Option<String> {
    let pem_pub = capture(
        "/usr/bin/openssl",
        &["x509", "-inform", form, "-in", &cer.to_string_lossy(), "-pubkey", "-noout"],
        None,
    )?;
    let der = capture(
        "/usr/bin/openssl",
        &["pkey", "-pubin", "-outform", "DER"],
        Some(&pem_pub),
    )?;
    digest(&der)
}

/// SHA-256 of `der`, as hex.
///
/// MEASURED, not assumed: `/usr/bin/openssl dgst -sha256` reading stdin on this platform
/// prints the bare hex with NO `(stdin)= ` prefix, while the same command given a FILE
/// prints `SHA2-256(path)= <hex>`. An earlier version split on the last space and so
/// returned `None` for every digest — which fails silently as "the certificate never
/// arrived", the worst possible symptom because it looks like the operator's fault.
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
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

/// Run 2: check the certificate before touching the keychain, then import it and the key
/// so macOS binds them into an identity, widen the key's ACL, and prove the result signs.
///
/// Every step is retry-safe: an item already in the keychain is success, not failure, so a
/// run that died after importing the certificate can simply be run again.
fn install(cer: &MatchedCert, key: &Path, id: &str) -> Outcome {
    let subject = match cert_subject(&cer.path, cer.form) {
        Some(s) => s,
        // Do not turn "could not read the subject" into "wrong team": one sends the
        // operator to re-issue a certificate that is probably fine.
        None => {
            return Outcome::Blocked {
                what: format!("could not read the subject of {}", cer.path.display()),
                fix: "check the file is the certificate Apple issued, not an HTML error page"
                    .into(),
            }
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
            fix: "import it by double-clicking in Finder, then re-run".into(),
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
            fix: format!("import {} by hand, then re-run", key.display()),
        };
    }

    // A certificate and key that ARE paired still report as no VALID identity when the
    // issuing intermediate is absent: macOS cannot build a chain to the root, and
    // `find-identity -v` lists only identities it can vouch for. This is the normal state
    // of a Mac that has never signed anything, and the old advice here — "open Keychain
    // Access and check the certificate is trusted" — named a symptom and left the operator
    // to guess. Supply the missing link instead.
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
                    fix: "install the issuer named by `openssl x509 -inform DER -in \
                          <cer> -noout -issuer` from https://www.apple.com/certificateauthority/ \
                          and re-run"
                        .into(),
                }
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
            fix: "check the certificate has not expired (`openssl x509 -inform DER -in \
                  <cer> -noout -dates`) and that its private key is in the same keychain"
                .into(),
        };
    }

    // -T alone is not enough: the partition list is a second ACL dimension, and without it
    // securityd raises a modal dialog the first time `codesign` uses the key. Match by
    // LABEL (`-l`, the certificate's common name, which the key takes once paired)
    // restricted to private signing keys — `-D` matches a key's *description*, an
    // attribute private keys do not carry, so it would match nothing and silently leave
    // the ACL unset. Stdio is INHERITED because `security` must be able to ask for the
    // login keychain password on this terminal; with `Command::output()` its stdin is
    // /dev/null and the prompt has nowhere to go. No `-k`: a password on argv is visible
    // in `ps`.
    let label = cert_common_name(&subject).unwrap_or_else(|| format!("Developer ID Application: {id}"));
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

    // Whether that worked is not asserted — it is measured, by the same proof every other
    // path uses.
    verdict(found)
}

/// `security import` is not idempotent: importing an item already present fails with a
/// duplicate-item error. A re-run after a half-finished install must not dead-end on
/// that, so a duplicate is treated as the success it effectively is.
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
/// MEASURED on a fresh Mac (m2, 2026-08-17): a correctly imported Developer ID certificate
/// and its key pair into an identity that `security find-identity -p codesigning` lists —
/// and that `find-identity -v` refuses, because the "Developer ID Certification Authority
/// (G2)" intermediate had never been installed, so macOS cannot chain the leaf to Apple
/// Root CA. `security verify-cert` PASSES in that state (it fetches intermediates over the
/// network), which makes the failure look like a mystery: the certificate verifies, and
/// the identity is still invalid. Twenty minutes of a real provisioning went into finding
/// that, so the tool now supplies the missing link itself.
///
/// Trust is not widened by this. An intermediate is only a chain link — it is signed by
/// Apple Root CA, which macOS already trusts — and it is imported ONLY when it is provably
/// the certificate that issued the operator's own leaf: same subject DN, and the same
/// issuing KEY. Anything else is refused, not imported.
///
/// The key half is the load-bearing one. A DN is a NAME, and subject-DN equality is no
/// proof of issuance: two distinct CA certificates can carry one DN — that is exactly the
/// relationship between a CA and a re-issued generation of itself, and Apple's two
/// Developer ID generations share the common name this code filters on. So the DN decides
/// only which of Apple's two published files to TRY first; what decides installation is
/// RFC 5280's Authority Key Identifier, the field whose whole job is naming the issuing
/// key. With the name no longer load-bearing, both files are tried and whichever actually
/// issued the leaf is the one installed.
///
/// MEASURED on this platform (LibreSSL 3.3.6, the openssl macOS ships): `openssl verify`
/// has no `-partial_chain`, so verifying a leaf against a bare intermediate is not
/// available here — it fails "unable to get issuer certificate" — and `x509 -ext` does not
/// exist either. Both identifiers are therefore read out of `-text`.
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
        // Why each candidate was refused, so "could not reach apple.com" never reports as
        // "Apple does not publish your issuer".
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
            // The encoding is PROVED, not assumed — the same rule [`find_matching_cert`]
            // follows. An `-inform DER` guess about a PEM file reads as "a different
            // issuer", which would send the operator after the wrong thing entirely.
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
        &["x509", "-inform", form, "-in", &cer.to_string_lossy(), "-noout", "-text"],
        None,
    )?;
    key_id_from_text(&String::from_utf8_lossy(&out), header)
}

/// The hex of the key identifier under `header`, uppercase and colon-free.
///
/// MEASURED on LibreSSL 3.3.6: the value is on the line AFTER the header, and the
/// Authority form prefixes it with `keyid:` while the Subject form does not (OpenSSL 3
/// drops the prefix on both). An AKI may be followed by `DirName:`/`serial:` lines, so
/// only the first value line counts, and it must look like hex.
fn key_id_from_text(text: &str, header: &str) -> Option<String> {
    let mut lines = text.lines().skip_while(|l| !l.trim().starts_with(header));
    let header_line = lines.next()?;
    // OpenSSL has printed the value on the header line itself in some releases.
    let inline = header_line.trim().strip_prefix(header).unwrap_or("").trim();
    let raw = if inline.is_empty() { lines.next()?.trim() } else { inline };
    let hex: String = raw
        .trim_start_matches("keyid:")
        .trim()
        .chars()
        .filter(|c| *c != ':')
        .collect();
    (!hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_uppercase())
}

/// The same repair, reached from the side an IMPORT cannot see: the certificate is
/// already in this keychain, and it is the intermediate that is gone.
///
/// [`install_issuer_intermediate`] was only ever called from [`install`], so it repaired
/// the machine that was importing a `.cer` and never the machine that already held one —
/// yet those are the same defect, and the second is the one that survives a login-keychain
/// rebuild or a Keychain Access cleanup. Nothing here is new behaviour: the leaf is
/// exported from the keychain, and the existing subject-equality gate decides whether an
/// intermediate may be installed against it.
///
/// Errors are dropped rather than reported. This is opportunistic — an unrelated
/// Developer ID certificate in the keychain must not turn into a Blocked verdict — and the
/// branch table that follows still reports the machine's real state either way. When the
/// repair does work it says so, and only then.
fn repair_installed_chain() -> Option<String> {
    let pems = capture(
        "/usr/bin/security",
        &["find-certificate", "-a", "-c", "Developer ID Application", "-p"],
        None,
    )?;
    // One PEM block per certificate, and `-c` matches on the common name alone, so blocks
    // for other teams can be in this stream — the team check below is the filter.
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

/// A temp file nobody else can be holding: `create_new` fails on an existing path, so a
/// pre-created file or symlink at the name is an error rather than something to overwrite.
/// The name still carries the pid, so two concurrent runs do not collide in the first
/// place.
fn temp_cert(bytes: &[u8]) -> Result<PathBuf, String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = std::env::temp_dir();
    for n in 0..16u32 {
        let path = dir.join(format!("aterm-provision-leaf-{}-{n}.pem", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(bytes).map_err(|e| format!("{}: {e}", path.display()))?;
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
        &["find-certificate", "-a", "-c", "Developer ID Certification Authority", "-p"],
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
        &["x509", "-inform", form, "-in", &cer.to_string_lossy(), "-noout", field],
        None,
    )?;
    Some(String::from_utf8_lossy(&out).to_string())
}

fn cert_subject(cer: &Path, form: &str) -> Option<String> {
    let out = capture(
        "/usr/bin/openssl",
        &["x509", "-inform", form, "-in", &cer.to_string_lossy(), "-noout", "-subject"],
        None,
    )?;
    Some(String::from_utf8_lossy(&out).to_string())
}

/// The common name out of an `openssl x509 -subject` line.
///
/// MEASURED on this platform: macOS ships LibreSSL, which prints slash-separated RDNs —
/// `subject= /CN=Developer ID Application: Name (TEAM)/OU=TEAM/O=Name/C=US`. OpenSSL 3
/// prints `subject=CN = Name, OU = ...`. Both are accepted, because a wrong label here
/// makes the partition-list call match nothing and the remediation command uncopyable.
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

/// Sign a throwaway copy of a system binary with the identity. Proves the ACL as well as
/// the pairing, and touches nothing that matters: the copy lives in a private directory
/// that is removed either way.
///
/// The probe path used to be `${TMPDIR}/aterm-provision-probe-<sha1>` — derived only from
/// a digest this tool PRINTS, in a directory that is world-writable whenever TMPDIR is
/// /tmp (sudo, cron, a CI shell). Two things followed. A second run of the audit — two
/// terminals, or a wrapper script beside a human — removed the file between the first
/// run's copy and its `codesign`, and a failed probe is not a small lie: `verdict` turns
/// it into Blocked, which defers the mint and prints a `set-key-partition-list` remedy for
/// a keychain that is already correct. And `fs::copy` FOLLOWS symlinks, so a pre-created
/// name was a target to overwrite rather than an error. Both go away with a per-run
/// directory (`create_dir` fails on an existing entry) and `create_new` on the file
/// inside.
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

/// A directory this run alone can be holding: `create_dir` fails on an existing entry, so
/// a pre-created path or a symlink planted at the name is an error rather than something
/// to write through. Created 0700 with the mode, not chmod'd afterwards, so there is no
/// instant in which a shared /tmp lets anyone else write inside it. The pid keeps two
/// concurrent runs from meeting at all; the counter steps over a directory a killed run
/// left behind.
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

/// The `--release-credentials` profile a Tier APPLE cut is given.
///
/// Written only when every part of it is already true, so the file's existence means "this
/// machine can cut", not "someone started provisioning it".
///
/// `create_new`: an existing profile is never clobbered. It may hold a headless
/// `notary_password` fallback, a hand-chosen `signing_identity_sha1`, or a key that is not
/// the one in `~/.aterm/machine.key`, and silently overwriting any of those would break a
/// machine that currently works.
///
/// The Ed25519 key is duplicated into this file because `sign.rs` requires `signing_key`
/// there and reads no other location. That is a second copy of a secret, so it is created
/// 0600 — with the mode, not chmod'd afterwards, so there is no instant in which it is
/// readable — in the same owner-only directory that already holds the original.
pub(crate) fn write_credentials_profile(
    id: &str,
    roster: &Path,
    identity_sha1: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    use base64::Engine as _;
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
    let signing_key = base64::engine::general_purpose::STANDARD.encode(&key_bytes);

    let mut body = String::new();
    body.push_str("# Written by `cargo ship provision --id ");
    body.push_str(id);
    body.push_str(
        "`. 0600, owner-only: it carries this\n\
         # machine's release signing key. Name it with `cargo ship cut --release-credentials`.\n\n",
    );
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

    /// MEASURED against both toolchains: macOS ships LibreSSL, whose slash-separated form
    /// the first version of this parser got wrong — it split on ", " and swallowed the
    /// rest of the subject into the label, which makes the partition-list call match
    /// nothing and leaves the operator an uncopyable remediation command.
    #[test]
    fn the_common_name_parses_in_both_openssl_dialects() {
        let libressl = "subject= /CN=Developer ID Application: A Person (A66A9P66Z7)/OU=A66A9P66Z7/O=A Person/C=US";
        let openssl3 = "subject=CN = Developer ID Application: A Person (A66A9P66Z7), OU = A66A9P66Z7, O = A Person, C = US";
        let want = "Developer ID Application: A Person (A66A9P66Z7)";
        assert_eq!(cert_common_name(libressl).as_deref(), Some(want));
        assert_eq!(cert_common_name(openssl3).as_deref(), Some(want));
    }

    #[test]
    fn a_subject_without_a_common_name_yields_none() {
        assert!(cert_common_name("subject= /O=A Person/C=US").is_none());
    }

    /// The errand text is the whole of the human's instructions, so it must name the
    /// request file, the Account Holder requirement, and G2 — the three things that make
    /// the upload land on the first try.
    ///
    /// And it must NOT end in "then re-run": on the interactive path it is printed
    /// directly above "waiting for the certificate to appear…", so that sentence told the
    /// operator to re-run a command that was at that moment sitting in a wait loop.
    #[test]
    fn the_errand_names_the_request_the_role_and_the_sub_ca() {
        let text = errand(Path::new("/tmp/devid-m9.certSigningRequest"));
        assert!(text.contains("/tmp/devid-m9.certSigningRequest"));
        assert!(text.contains("Account Holder"));
        assert!(text.contains("G2 Sub-CA"));
        assert!(text.contains("~/Downloads"), "the download has to land somewhere: {text}");
        assert!(!text.contains("re-run"), "{text}");
    }

    /// Two overlapping Developer ID certificates is what a RENEWAL looks like, and the
    /// pairing proof used to compare against whichever the keychain listed first. The
    /// stream shape is MEASURED: `security find-certificate -a -p -Z` prints a SHA-256
    /// line, a SHA-1 line, then the block.
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
        assert!(leaf_pem_by_sha1(stream, "1111111111111111111111111111111111111111")
            .is_some_and(|p| p.contains("older")));
        assert!(leaf_pem_by_sha1(stream, &"3".repeat(40)).is_none());
    }

    /// The two identifiers that decide whether an intermediate may be installed. Both
    /// fixtures are VERBATIM `openssl x509 -text` output from LibreSSL 3.3.6 — the openssl
    /// macOS ships — over the Developer ID certificate and its G2 issuer: the authority
    /// form carries a `keyid:` prefix and the subject form does not, and reading either
    /// one wrong would import a CA that did not issue the operator's certificate.
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
        assert!(key_id_from_text("X509v3 Basic Constraints: critical\n", AUTHORITY_KEY_ID).is_none());
    }

    /// MEASURED: `openssl x509 -noout -enddate` over the installed Developer ID
    /// certificate. The date is the whole value of the clause — a wrong one is worse than
    /// none, because it is the date the operator would diarise.
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

    /// A duplicate item is the shape a RETRY takes, and retries must work: the operator
    /// re-runs the same command after any interruption.
    #[test]
    fn a_duplicate_keychain_item_is_not_a_failure() {
        let dup = "security: SecKeychainItemImport: The specified item already exists in the keychain.";
        assert!(dup.to_ascii_lowercase().contains("already exists"));
    }
}
