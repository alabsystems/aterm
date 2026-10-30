// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Apple Developer ID and notarization provisioning for `ship provision`. The Developer ID
//! pair goes into aterm's own release keychain ([`crate::release_keychain`]), never the login
//! keychain, so no step asks for the login keychain's password.

#![cfg(unix)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use aterm_update_core::pins;

use crate::dev_identity::Bounded;
use crate::publish::step;
use crate::release_keychain::{Keychains, Leave, OPENED_NOTE, Proof};

/// The label this check prints under, in the audit too: one subject, one label.
pub(crate) const APPLE_LABEL: &str = "apple id";

/// Where the Apple key material of the user whose home is `home` lives.
fn apple_dir_in(home: &Path) -> PathBuf {
    home.join(".aterm").join("apple")
}

fn key_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("devid-{id}.key"))
}

fn csr_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("devid-{id}.certSigningRequest"))
}

/// A certificate file and the encoding the matcher proved it parses as (DER or PEM).
struct MatchedCert {
    path: PathBuf,
    form: &'static str,
}

/// What is observably true right now, re-derived every run (no stored flags).
pub(crate) struct Observed {
    /// 40-hex SHA-1 of each valid `Developer ID Application` identity for our team.
    pub identities: Vec<String>,
    /// Set when `security find-identity` could not run.
    pub lookup_failed: Option<String>,
    /// Count of our team's Developer ID certificates that are present but not valid.
    pub invalid_present: usize,
    /// Their SHA-1s, as `security find-identity` prints them.
    pub invalid: Vec<String>,
    pub key: Option<PathBuf>,
    pub csr: Option<PathBuf>,
}

impl Observed {
    pub fn look(kc: &Keychains<'_>, id: &str) -> Observed {
        let dir = apple_dir_in(kc.home());
        let (key, csr) = {
            let k = key_path(&dir, id);
            let c = csr_path(&dir, id);
            (k.is_file().then_some(k), c.is_file().then_some(c))
        };
        let (identities, lookup_failed) = valid_identities(kc);
        let invalid = invalid_identities(kc, &identities);
        Observed {
            identities,
            lookup_failed,
            invalid_present: invalid.len(),
            invalid,
            key,
            csr,
        }
    }
}

/// `security find-identity -v` filtered to our team, in the release keychain by name and
/// then on the search list (where a Mac provisioned before it holds its identity, in the login
/// keychain), plus the reason we could not look.
fn valid_identities(kc: &Keychains<'_>) -> (Vec<String>, Option<String>) {
    match kc.identities(pins::APPLE_TEAM_ID, true) {
        Ok(ids) => (ids, None),
        Err(e) => (Vec::new(), Some(format!("could not run `security`: {e}"))),
    }
}

/// The SHA-1s of our team's Developer ID identities the keychains hold but will not vouch
/// for: all identities (no `-v`) but the `valid` ones.
fn invalid_identities(kc: &Keychains<'_>, valid: &[String]) -> Vec<String> {
    kc.identities(pins::APPLE_TEAM_ID, false)
        .unwrap_or_default()
        .into_iter()
        .filter(|id| !valid.iter().any(|v| v.eq_ignore_ascii_case(id)))
        .collect()
}

/// What the Apple step decided.
pub(crate) enum Outcome {
    /// An identity is installed and proved usable.
    Ready {
        ids: Vec<String>,
        note: Option<String>,
    },
    /// Progress, waiting on the operator: not a fault to fix, which is `Blocked`.
    Waiting { what: String, next: String },
    /// `Waiting`'s shape for the read-only mode.
    Todo { what: String, next: String },
    /// Something is wrong and the tool will not proceed past it.
    Blocked { what: String, fix: String },
    /// Not a macOS host; the verdict says "skipped".
    Skipped(String),
}

/// What a watched folder holds for the request key, judged against the keychain
/// ([`on_disk`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum OnDisk {
    /// No certificate for the key that is still good.
    Nothing,
    /// A current one the keychain holds as no identity: provision imports it.
    Unimported,
    /// One the keychain holds as an identity it does not trust, without the certificate that
    /// issued it: provision installs that.
    Unchained,
    /// Only ones the keychain holds with their issuer and still does not trust (revoked), the
    /// first of them at this path: only a new certificate from Apple changes that, and
    /// provision passes over these while it waits for one.
    Refused(PathBuf),
    /// The folders or the request key could not be read (why), which stops a real run too.
    Unlooked(String),
}

/// What a read-only run says of this machine's Apple state, given the valid identities, the
/// request key, the folder the operator named (`cert_dir`), and what the watched folders hold
/// for that key. A certificate for the key that provision can make an identity, by importing
/// it or its issuer, is a step for whoever runs provision; one the keychain holds with its
/// issuer and still refuses is the owner's errand at Apple. A folder or key it could not read
/// stops it as it stops a real run. `leave` says whether the test signature of a held
/// identity may re-assert the release keychain's partition list first.
fn read_only(
    kc: &Keychains<'_>,
    id: &str,
    leave: Leave,
    identities: Vec<String>,
    key: Option<&Path>,
    cert_dir: Option<&Path>,
    on_disk: &dyn Fn(&Path) -> OnDisk,
) -> Outcome {
    let provision = |which: &str| {
        format!(
            "run `{} provision --id {id}{}`, which {which}",
            crate::publish::SHIP_COMMAND,
            cert_dir.map_or(String::new(), |dir| format!(
                " --cert-dir {}",
                dir.display()
            ))
        )
    };
    match (identities.is_empty(), key) {
        (false, _) => verdict(kc, id, identities, leave),
        (true, Some(key)) => match on_disk(key) {
            OnDisk::Unimported => Outcome::Blocked {
                what: format!(
                    "'{id}''s Developer ID key and certificate are on disk but not in the keychain"
                ),
                fix: provision("imports them"),
            },
            OnDisk::Unchained => Outcome::Blocked {
                what: format!(
                    "'{id}''s Developer ID certificate is in the keychain without the certificate that issued it"
                ),
                fix: provision("installs it"),
            },
            OnDisk::Refused(cert) => Outcome::Todo {
                what: format!(
                    "'{id}''s Developer ID certificate {} is in the keychain with its issuer, and macOS still does not trust it (revoked?)",
                    cert.display()
                ),
                next: format!(
                    "a new certificate from Apple: {}; where the secrets vault holds the old \
                     one, replace that entry with the new certificate (aterm secrets set \
                     <its name> --file <the new .cer> --if-absent) so the Macs it fills get it",
                    provision(
                        "writes the request to upload and passes over the old certificate while it waits for the new one"
                    )
                ),
            },
            OnDisk::Unlooked(why) => unlooked(why, key),
            OnDisk::Nothing => Outcome::Todo {
                what: format!("'{id}' holds a request key and no installed certificate"),
                next:
                    "re-run without --check to import the certificate once you have downloaded it"
                        .into(),
            },
        },
        (true, None) => Outcome::Todo {
            what: format!(
                "no Developer ID Application identity for team {}",
                pins::APPLE_TEAM_ID
            ),
            next: format!(
                "re-run without --check: it will ask before spending one of five permanent \
                 certificate slots, then generate '{id}'s request"
            ),
        },
    }
}

/// The driver: it re-reads the world each run, and only a confirmed [`generate`] makes a new
/// key.
pub(crate) fn acquire(
    id: &str,
    may_change: bool,
    leave: Leave,
    cert_dir: Option<&Path>,
) -> Outcome {
    if !cfg!(target_os = "macos") {
        return Outcome::Skipped("cuts run on macOS (Tier APPLE); no keychain on this host".into());
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Outcome::Blocked {
            what: "HOME is not set, so ~/.aterm/apple has no location".into(),
            fix: "run provision as the user that will cut releases".into(),
        };
    };
    acquire_in(
        &Keychains::new(&home, &Bounded),
        id,
        may_change,
        leave,
        cert_dir,
    )
}

/// [`acquire`] for the user whose home `kc` names, every `security` and `codesign` call
/// through it. With leave to change it first re-asserts an existing release keychain
/// ([`Keychains::converge`]) and test-signs; without, it only opens it, and its test
/// signature follows `leave` (a real release's audit may re-assert the partition list; a
/// check or a dry run writes nothing).
fn acquire_in(
    kc: &Keychains<'_>,
    id: &str,
    may_change: bool,
    leave: Leave,
    cert_dir: Option<&Path>,
) -> Outcome {
    let dir = apple_dir_in(kc.home());
    if may_change {
        if let Err(refused) = kc.converge(pins::APPLE_TEAM_ID) {
            return Outcome::Blocked {
                what: refused.what,
                fix: refused.fix,
            };
        }
    } else {
        // One that will not open is the keychain line's to report; nothing signs from it.
        let _ = kc.unlock();
    }
    let watch = Watch::new(Some(kc.home()), cert_dir);
    let mut seen = Observed::look(kc, id);

    if let Some(why) = seen.lookup_failed.as_deref() {
        return Outcome::Blocked {
            what: format!(
                "cannot read the keychain, so this machine's Apple state is unknown: {why}"
            ),
            fix: "install the Xcode command-line tools (`xcode-select --install`)".into(),
        };
    }

    if !may_change {
        let Observed {
            identities,
            invalid,
            key,
            ..
        } = seen;
        return read_only(
            kc,
            id,
            leave,
            identities,
            key.as_deref(),
            cert_dir,
            &|key| on_disk(kc, key, &watch, &invalid),
        );
    }

    if seen.identities.is_empty()
        && seen.invalid_present > 0
        && let Some(note) = repair_installed_chain(kc)
    {
        step(APPLE_LABEL, &note);
        seen = Observed::look(kc, id);
    }

    if !seen.identities.is_empty() {
        // The spent request is public and finished, so it is removed silently. The
        // private key is never touched automatically: its slot cannot be reclaimed.
        if let Some(csr) = &seen.csr
            && paired_with_installed(kc, &seen.key, &seen.identities[0])
        {
            let _ = std::fs::remove_file(csr);
        }
        return verdict(kc, id, seen.identities, Leave::Write);
    }

    if let (Some(key), Some(csr)) = (&seen.key, &seen.csr) {
        return await_then_install(kc, key, csr, id, &seen.invalid, &watch);
    }

    if seen.key.is_none() && seen.csr.is_some() {
        let csr = csr_path(&dir, id);
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
                await_then_install(kc, key, &csr, id, &seen.invalid, &watch)
            }
            Err(e) => Outcome::Blocked {
                what: format!("could not rebuild the certificate request: {e}"),
                fix: format!("check {} is a readable private key", key.display()),
            },
        };
    }

    match confirm_slot(id, seen.invalid_present) {
        Ok(true) => match generate(&dir, id) {
            Ok(csr) => await_then_install(kc, &key_path(&dir, id), &csr, id, &seen.invalid, &watch),
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

/// What a run says when it could not look for the certificate (`why`, from
/// [`find_matching_cert`]): the request key is unreadable, or a watched folder is.
fn unlooked(why: String, key: &Path) -> Outcome {
    if why.contains("could not read the public half") {
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
            fix: "make the folder it names readable (ls -ld <it>), then re-run".into(),
        }
    }
}

/// How long the command waits for the browser errand.
const WAIT_FOR_CERT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Prints the browser errand, waits for a public-key-matched certificate in `watch`, and
/// installs it.
fn await_then_install(
    kc: &Keychains<'_>,
    key: &Path,
    csr: &Path,
    id: &str,
    invalid: &[String],
    watch: &Watch,
) -> Outcome {
    let issuer = |leaf: &str| issuer_in_keychain(kc, leaf);
    let lookup = || look(key, watch, invalid, 0, &issuer);
    let rejected = match lookup() {
        Ok(Look {
            cert: Some(cer), ..
        }) => return install(kc, &cer, key, id),
        Err(why) => return unlooked(why, key),
        Ok(Look { rejected, .. }) => rejected,
    };
    if !has_terminal() {
        return Outcome::Waiting {
            what: waiting_text(id, invalid.len(), &rejected, None, watch),
            next: errand_lines(csr, false, watch).join("\n"),
        };
    }
    let visible = watch
        .named
        .as_deref()
        .and_then(|named| surface_csr(csr, id, named));
    let upload = visible.as_deref().unwrap_or(csr);
    let waiting = |rejected: &[PathBuf]| Outcome::Waiting {
        what: waiting_text(id, invalid.len(), rejected, Some(upload), watch),
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
    if let Some(note) = rejected_note(&rejected, Some(upload), watch) {
        step(label, &note);
        label = "";
    }
    for line in errand_lines(upload, true, watch) {
        step(label, &line);
        label = "";
    }
    let started = std::time::Instant::now();
    let mut announced = 0u64;
    let mut rejected = rejected;
    while started.elapsed() < WAIT_FOR_CERT {
        std::thread::sleep(std::time::Duration::from_secs(3));
        match lookup() {
            Ok(Look {
                cert: Some(cer), ..
            }) => {
                step(
                    APPLE_LABEL,
                    &format!("certificate found: {}", cer.path.display()),
                );
                return install(kc, &cer, key, id);
            }
            Err(why) => return unlooked(why, key),
            Ok(Look { rejected: seen, .. }) => {
                let fresh = take_arrivals(seen, &mut rejected);
                if let Some(note) = rejected_note(&fresh, Some(upload), watch) {
                    step(
                        APPLE_LABEL,
                        "⚠ THE .cer THAT JUST ARRIVED IS NOT THIS REQUEST'S",
                    );
                    step("", &note);
                }
            }
        }
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
    step(
        APPLE_LABEL,
        &format!(
            "{} min elapsed — stopping the wait. Nothing was lost and nothing was spent.",
            WAIT_FOR_CERT.as_secs() / 60
        ),
    );
    waiting(&rejected)
}

/// The `.cer` files in `seen` that are not in `rejected`, in `seen`'s order: the wrong ones
/// that arrived since the last look. `rejected` then becomes `seen`, so each is named once.
fn take_arrivals(seen: Vec<PathBuf>, rejected: &mut Vec<PathBuf>) -> Vec<PathBuf> {
    let fresh = seen
        .iter()
        .filter(|p| !rejected.contains(p))
        .cloned()
        .collect();
    *rejected = seen;
    fresh
}

/// Whether there is a human to ask.
fn has_terminal() -> bool {
    // A test never waits on a person: with no terminal the errand is returned at once, so a
    // certificate the test expected to match fails fast instead of polling for 30 minutes.
    if cfg!(test) {
        return false;
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .is_ok_and(|f| std::io::IsTerminal::is_terminal(&f))
}

/// Verifies that the pinned `signing_identity_sha1` is valid in `kc`'s keychains and can sign
/// unattended ([`prove_can_sign`]).
pub(crate) fn pinned_identity_proves(
    kc: &Keychains<'_>,
    sha1: &str,
    leave: Leave,
) -> Result<Proof, String> {
    let (ids, lookup_failed) = valid_identities(kc);
    if let Some(why) = lookup_failed {
        return Err(format!("cannot read the keychain: {why}"));
    }
    if !ids.iter().any(|id| id.eq_ignore_ascii_case(sha1)) {
        return Err(format!(
            "no valid Developer ID Application identity {sha1} for team {} in the keychain",
            pins::APPLE_TEAM_ID
        ));
    }
    prove_can_sign(kc, sha1, leave)
}

/// The single place a green Apple verdict is produced, so no path skips the proof. The fix
/// for a key codesign cannot use names the login keychain's password only for an identity
/// that keychain is known to hold (a Mac provisioned before the release keychain): one in the
/// release keychain is fixed by provision with that keychain's own password, and one whose
/// keychain cannot be read is re-run.
fn verdict(kc: &Keychains<'_>, id: &str, ids: Vec<String>, leave: Leave) -> Outcome {
    match prove_can_sign(kc, &ids[0], leave) {
        Ok(proof) => {
            let expiry = expiry_note(kc, &ids[0]);
            let note = match proof {
                Proof::Signed => expiry,
                Proof::Opened => Some(match expiry {
                    Some(expiry) => format!("{OPENED_NOTE}; {expiry}"),
                    None => OPENED_NOTE.to_string(),
                }),
            };
            Outcome::Ready { ids, note }
        }
        Err(why) => Outcome::Blocked {
            what: format!(
                "identity {} is installed but a test signature did not succeed ({why}) — \
                 codesign cannot use this key unattended, so a cut would stop here",
                ids[0]
            ),
            fix: match kc.holds(pins::APPLE_TEAM_ID, &ids[0]) {
                Ok(true) => format!(
                    "run `{} provision --id {id}`: it sets the partition list on {} with that \
                     keychain's own stored password (never your login password), then \
                     re-tests the signature",
                    crate::publish::SHIP_COMMAND,
                    kc.release().display()
                ),
                Err(why) => format!(
                    "{} could not be read ({why}); re-run once `security` answers",
                    kc.release().display()
                ),
                Ok(false) => format!(
                    "this identity is in your login keychain (provisioned before aterm's \
                     release keychain): run `security set-key-partition-list -S \
                     apple-tool:,apple:,codesign: -t private -s -l '{}' {}` (it asks for your \
                     login keychain password), then re-run this command — it re-tests the \
                     signature every time",
                    identity_label(kc, &ids[0])
                        .unwrap_or_else(|| "<the certificate's name>".into()),
                    crate::release_keychain::login_keychain_path(kc.home()).display(),
                ),
            },
        },
    }
}

/// Whether the on-disk request key is the private half of the certificate this identity names.
fn paired_with_installed(kc: &Keychains<'_>, key: &Option<PathBuf>, sha1: &str) -> bool {
    let Some(key) = key else { return false };
    installed_cert_spki(kc, sha1)
        .is_some_and(|installed| key_spki_sha256(key).is_some_and(|k| k == installed))
}

/// The SPKI digest of the certificate the identity `sha1` names.
fn installed_cert_spki(kc: &Keychains<'_>, sha1: &str) -> Option<String> {
    let pem = installed_leaf_pem(kc, sha1)?;
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

/// PEM of the installed `Developer ID Application` certificate with digest `sha1`.
fn installed_leaf_pem(kc: &Keychains<'_>, sha1: &str) -> Option<String> {
    let out = kc.find_certificates(&[
        "find-certificate",
        "-a",
        "-c",
        "Developer ID Application",
        "-p",
        "-Z",
    ])?;
    leaf_pem_by_sha1(&out, sha1)
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

/// How close to expiry the green line starts naming the renewal errand.
const RENEWAL_WINDOW_SECS: u64 = 90 * 24 * 60 * 60;

/// The expiry clause on a green Apple line.
fn expiry_note(kc: &Keychains<'_>, sha1: &str) -> Option<String> {
    let leaf = installed_leaf_pem(kc, sha1)?;
    let mut soonest = (cert_enddate_iso(&leaf)?, leaf.clone());
    if let Some(ca) = issuer_in_keychain(kc, &leaf) {
        match cert_enddate_iso(&ca) {
            Some(iso) if iso < soonest.0 => soonest = (iso, ca),
            _ => {}
        }
    }
    let (iso, pem) = soonest;
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

/// The certificate in `kc`'s keychains that issued `leaf`, matched by subject DN.
fn issuer_in_keychain(kc: &Keychains<'_>, leaf: &str) -> Option<String> {
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
    keychain_cert_with_subject(kc, &issuer)
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

/// Whether `openssl x509 -checkend` says this certificate outlives `secs`.
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

/// The label macOS gives the paired private key (the certificate's common name), as the
/// search list holds it.
fn identity_label(kc: &Keychains<'_>, sha1: &str) -> Option<String> {
    let text = kc.security_stdout(&[&"find-identity", &"-v", &"-p", &"codesigning"])?;
    let line = text.lines().find(|l| l.contains(sha1))?;
    let start = line.find('"')? + 1;
    let end = line.rfind('"')?;
    (end > start).then(|| line[start..end].to_string())
}

/// The plural `s`, so a count of 1 never reads `1 item(s)`.
const fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The verdict when no certificate has arrived.
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

/// Prints the confirmation prompt for generating a new certificate request.
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
    if !yes && !answer.is_empty() && answer != "n" && answer != "no" {
        step(
            APPLE_LABEL,
            &format!("'{answer}' is not 'y' — taking that as no; no slot spent"),
        );
    }
    Ok(yes)
}

/// The keychain profile name the notarytool credential is stored under.
pub(crate) const NOTARY_PROFILE: &str = "notary";

/// Store a notarytool credential in this machine's keychain.
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
    let profile = NOTARY_PROFILE;
    step(
        "notary",
        "the password this step asks for is an APP-SPECIFIC password, NOT your Apple ID \
         password. Mint one first — Apple shows it once:\n\
         1. https://account.apple.com → Sign-In and Security\n\
         2. App-Specific Passwords → +   (2FA required)\n\
         3. copy the xxxx-xxxx-xxxx-xxxx string\n\
         then paste it at the password prompt below.",
    );
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

/// A first run's RSA-2048 key and CSR, both made here; existing key material is never
/// overwritten.
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

/// Derive the request from the key.
fn write_csr(key: &Path, csr: &Path) -> Result<PathBuf, String> {
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

/// The one unavoidable human step, one instruction per line, and its only spelling.
pub(crate) fn errand_lines(csr: &Path, waiting: bool, watch: &Watch) -> Vec<String> {
    let mut out = vec![
        "you will upload THIS file (and only this one):".to_string(),
        format!("     {}", csr.display()),
        String::new(),
        "at https://developer.apple.com/account/resources/certificates".to_string(),
        "     signed in as the Account Holder — Apple lets nobody else create a Developer \
         ID certificate"
            .to_string(),
        String::new(),
        "1. + → Software → 'Developer ID Application'".to_string(),
        "2. Profile Type 'G2 Sub-CA'".to_string(),
        "     NOT 'Previous Sub-CA' — its intermediate expires 2027-02-01".to_string(),
        "3. upload the request named above".to_string(),
        format!("4. Download the result into {}", watch.spoken()),
        "     the filename does not matter — it is matched against this request by public key"
            .to_string(),
    ];
    if watch.named.is_none() {
        out.push(
            "     (hidden folders: press ⌘⇧G in the file dialog and paste the path; or \
             re-run with --cert-dir <folder> to have the request copied to, and the \
             certificate looked for in, a folder you name)"
                .to_string(),
        );
    }
    if waiting {
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

/// Where the certificate is looked for and the request surfaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Watch {
    /// `~/.aterm/apple`, or `None` without a `$HOME` (then only `named` is read).
    own: Option<PathBuf>,
    /// The folder the operator named, as given (a relative path is theirs to resolve).
    named: Option<PathBuf>,
}

impl Watch {
    /// A watch over `named`, then `~/.aterm/apple` of the user whose home is `home`.
    pub(crate) fn new(home: Option<&Path>, named: Option<&Path>) -> Self {
        Self {
            own: home.map(apple_dir_in),
            named: named.map(Path::to_path_buf),
        }
    }

    /// The folders `find_matching_cert` reads.
    fn dirs(&self) -> Vec<PathBuf> {
        self.named.iter().chain(self.own.iter()).cloned().collect()
    }

    /// The folders, spoken for the errand (`~/Downloads or ~/.aterm/apple`), `$HOME` folded to `~`.
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

    /// A line, printed before the errand, saying the named folder is one macOS guards.
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

/// Copy the request into the folder the operator named.
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

/// What the watched folders hold for `key` ([`OnDisk`]), given the SHA-1s of the team's
/// identities the keychain holds but does not trust (`invalid`): unlooked when a folder or the
/// key could not be read and no certificate there can be installed.
fn on_disk(kc: &Keychains<'_>, key: &Path, watch: &Watch, invalid: &[String]) -> OnDisk {
    look(key, watch, invalid, 0, &|leaf| issuer_in_keychain(kc, leaf))
        .map_or_else(OnDisk::Unlooked, |look| look.judged)
}

/// What the watched folders hold for the request key, as provision's wait and its read-only
/// judge both read them.
struct Look {
    /// The first certificate for the key that is not [`spent`].
    cert: Option<MatchedCert>,
    /// How the keychain takes it, or, with none, the first refused one, else nothing.
    judged: OnDisk,
    /// The `.cer` files that are not for the key.
    rejected: Vec<PathBuf>,
}

/// [`Look`] at the watched folders, given the SHA-1s of the identities the keychain does not
/// trust (`invalid`), how long a certificate must stay good (`secs`), and `issuer`, which
/// finds the keychain's copy of a PEM leaf's issuer.
fn look(
    key: &Path,
    watch: &Watch,
    invalid: &[String],
    secs: u64,
    issuer: &dyn Fn(&str) -> Option<String>,
) -> Result<Look, String> {
    let mut verdict = OnDisk::Nothing;
    let mut refused = None;
    let (cert, rejected) = find_matching_cert(key, watch, &mut |cert| {
        verdict = judged(cert, invalid, secs, issuer);
        if let OnDisk::Refused(path) = &verdict {
            refused.get_or_insert_with(|| path.clone());
        }
        spent(&verdict)
    })?;
    let judged = match cert {
        Some(_) => verdict,
        None => refused.map_or(OnDisk::Nothing, OnDisk::Refused),
    };
    Ok(Look {
        cert,
        judged,
        rejected,
    })
}

/// How the keychain takes `cert`, a certificate for the request key: nothing when it is not
/// good `secs` from now, and refused when the keychain holds it, with the certificate that
/// issued it (`issuer`), as one of the identities it does not trust (`invalid`).
fn judged(
    cert: &MatchedCert,
    invalid: &[String],
    secs: u64,
    issuer: &dyn Fn(&str) -> Option<String>,
) -> OnDisk {
    if !good_for(cert, secs) {
        return OnDisk::Nothing;
    }
    let held =
        cert_sha1(cert).is_some_and(|sha1| invalid.iter().any(|id| id.eq_ignore_ascii_case(&sha1)));
    if !held {
        return OnDisk::Unimported;
    }
    match cert_pem(cert).and_then(|pem| issuer(&pem)) {
        None => OnDisk::Unchained,
        Some(_) => OnDisk::Refused(cert.path.clone()),
    }
}

/// Whether installing a certificate judged so changes nothing.
fn spent(judged: &OnDisk) -> bool {
    matches!(judged, OnDisk::Nothing | OnDisk::Refused(_))
}

/// `cert`'s SHA-1 fingerprint as `security find-identity` prints an identity's: 40
/// upper-case hex digits.
fn cert_sha1(cert: &MatchedCert) -> Option<String> {
    let path = cert.path.to_string_lossy();
    let args = [
        "x509",
        "-inform",
        cert.form,
        "-in",
        &path,
        "-noout",
        "-fingerprint",
        "-sha1",
    ];
    let out = capture("/usr/bin/openssl", &args, None)?;
    let text = String::from_utf8_lossy(&out);
    let hex: String = text
        .trim()
        .split_once('=')?
        .1
        .chars()
        .filter(|c| *c != ':')
        .collect();
    aterm_codec::hex::is_hex(&hex, 40).then(|| hex.to_ascii_uppercase())
}

/// `cert` as PEM text.
fn cert_pem(cert: &MatchedCert) -> Option<String> {
    let path = cert.path.to_string_lossy();
    let out = capture(
        "/usr/bin/openssl",
        &["x509", "-inform", cert.form, "-in", &path],
        None,
    )?;
    String::from_utf8(out).ok()
}

/// Whether `cert` is still good `secs` from now (`openssl x509 -checkend`); one openssl
/// cannot judge is not.
fn good_for(cert: &MatchedCert, secs: u64) -> bool {
    let path = cert.path.to_string_lossy();
    let secs = secs.to_string();
    let args = [
        "x509",
        "-inform",
        cert.form,
        "-in",
        &path,
        "-noout",
        "-checkend",
        &secs,
    ];
    capture("/usr/bin/openssl", &args, None).is_some()
}

/// Find a downloaded certificate that belongs to this key, by comparing public keys, passing
/// over one that `passed` names.
fn find_matching_cert(
    key: &Path,
    watch: &Watch,
    passed: &mut dyn FnMut(&MatchedCert) -> bool,
) -> Result<(Option<MatchedCert>, Vec<PathBuf>), String> {
    let want = key_spki_sha256(key)
        .ok_or_else(|| format!("could not read the public half of {}", key.display()))?;
    let mut unreadable = Vec::new();
    let mut rejected = Vec::new();
    for dir in watch.dirs() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
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
                Some(form) => {
                    let cert = MatchedCert { path, form };
                    if !passed(&cert) {
                        return Ok((Some(cert), rejected));
                    }
                }
                None => rejected.push(path),
            }
        }
    }
    if !unreadable.is_empty() {
        return Err(unreadable.join("; "));
    }
    Ok((None, rejected))
}

/// The `.cer` files examined and refused, as one line.
fn rejected_note(rejected: &[PathBuf], csr: Option<&Path>, watch: &Watch) -> Option<String> {
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

/// SHA-256 of a certificate's SPKI, comparable with [`key_spki_sha256`].
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

/// SHA-256 of `der`, as hex.
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

/// Imports the certificate and key into the release keychain, lets codesign use the key
/// unattended with that keychain's own password, and verifies signing. No other keychain is
/// written ([`Keychains::install_pair`]).
fn install(kc: &Keychains<'_>, cer: &MatchedCert, key: &Path, id: &str) -> Outcome {
    let subject = match cert_subject(&cer.path, cer.form) {
        Some(s) => s,
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
    let release = kc.release();
    if let Err(refused) = kc.install_pair(&cer.path, key) {
        return Outcome::Blocked {
            what: format!(
                "could not put {} and its key into {}: {}",
                cer.path.display(),
                release.display(),
                refused.what
            ),
            fix: refused.fix,
        };
    }

    let mut found = valid_identities(kc).0;
    if found.is_empty() {
        match install_issuer_intermediate(kc, cer) {
            Ok(Some(note)) => {
                step(APPLE_LABEL, &note);
                found = valid_identities(kc).0;
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
            fix: format!(
                "check the dates:  openssl x509 -inform {} -in {} -noout -dates\n\
                 list what this keychain can actually sign with:  security find-identity \
                 -v -p codesigning {}\n\
                 an identity needs BOTH halves in ONE keychain — the certificate above is \
                 imported, so an empty list here means its private key is not in {}",
                cer.form,
                cer.path.display(),
                release.display(),
                release.display()
            ),
        };
    }
    verdict(kc, id, found, Leave::Write)
}

/// Install the intermediate that issued this certificate into the release keychain, if no
/// keychain holds it.
fn install_issuer_intermediate(
    kc: &Keychains<'_>,
    cer: &MatchedCert,
) -> Result<Option<String>, String> {
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
    if keychain_has_subject(kc, &issuer) {
        return Ok(None);
    }
    let want_key = cert_key_id(&cer.path, cer.form, AUTHORITY_KEY_ID).ok_or(
        "this certificate names no issuing key (no Authority Key Identifier), so which CA \
         issued it cannot be proved",
    )?;
    let dir = temp_run_dir("issuer")?;
    let outcome = (|| -> Result<Option<String>, String> {
        let mut urls = CA_URLS;
        if !issuer.contains("G2") {
            urls.swap(0, 1);
        }
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
            kc.install_certificate(&tmp)?;
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
fn key_id_from_text(text: &str, header: &str) -> Option<String> {
    let mut lines = text.lines().skip_while(|l| !l.trim().starts_with(header));
    let header_line = lines.next()?;
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

/// Repairs missing issuer intermediates for already-installed team Developer ID leaves.
fn repair_installed_chain(kc: &Keychains<'_>) -> Option<String> {
    let pems = kc.find_certificates(&[
        "find-certificate",
        "-a",
        "-c",
        "Developer ID Application",
        "-p",
    ])?;
    for block in pems.split_inclusive("-----END CERTIFICATE-----") {
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
            install_issuer_intermediate(kc, &cer).ok().flatten()
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

/// A temp file nobody else can hold.
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

/// Whether any certificate in `kc`'s keychains has exactly this subject.
fn keychain_has_subject(kc: &Keychains<'_>, subject: &str) -> bool {
    keychain_cert_with_subject(kc, subject).is_some()
}

/// The Developer ID CA certificate in `kc`'s keychains whose subject is exactly `subject`.
fn keychain_cert_with_subject(kc: &Keychains<'_>, subject: &str) -> Option<String> {
    let pems = kc.find_certificates(&[
        "find-certificate",
        "-a",
        "-c",
        "Developer ID Certification Authority",
        "-p",
    ])?;
    pems.split_inclusive("-----END CERTIFICATE-----")
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

/// Signs a throwaway copy of a system binary with `sha1` to verify pairing and ACL, naming
/// the release keychain (`--keychain`) when it holds the identity
/// ([`Keychains::signing_keychain`]). Under [`Leave::Look`] an identity the release keychain
/// holds is only opened ([`Proof::Opened`]): a test signature from it is safe only once its
/// partition list is re-asserted, which is a write.
pub(crate) fn prove_can_sign(
    kc: &Keychains<'_>,
    sha1: &str,
    leave: Leave,
) -> Result<Proof, String> {
    aterm_secrets::release_keychain::prove_can_sign(kc, sha1, leave)
}

/// A directory this run alone can hold.
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

/// Writes the `--release-credentials` profile once signing is verified.
pub(crate) fn write_credentials_profile(
    id: &str,
    roster: &Path,
    identity_sha1: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let Some(home) = std::env::var_os("HOME") else {
        return Err("HOME is not set".into());
    };
    let ring = PathBuf::from(home).join(atpkg_keys::provision::RELEASE_RING_REL);
    let path = ring.join("release-credentials.toml");
    if path.exists() {
        return Ok(None);
    }
    let key_bytes = std::fs::read(ring.join("machine.key"))
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

fn chmod(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// A local helper may wait for keychain access; unattended provisioning must still finish.
const COMMAND_WAIT: Duration = Duration::from_secs(60);

/// Run a command for its exit status, capturing stderr for the error message.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    run_command(
        aterm_uds::spawnfd::command(program).args(args),
        COMMAND_WAIT,
    )
}

fn run_command(command: &mut aterm_uds::spawnfd::Command, within: Duration) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let out = aterm_uds::deadline::output_within(command, within).map_err(|error| match error {
        aterm_uds::deadline::RunError::TimedOut => format!(
            "{program}: did not finish within {} s; requested termination and waited for the owned command",
            within.as_secs()
        ),
        error => format!("{program}: {error}"),
    })?;
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
    use crate::release_keychain::fake::{Fake, Kc, World};

    /// A watch over `named` and this test process's own `~/.aterm/apple`.
    fn env_watch(named: Option<&Path>) -> Watch {
        Watch::new(
            std::env::var_os("HOME").map(PathBuf::from).as_deref(),
            named,
        )
    }

    const COMMAND_CHILD: &str = "ATERM_APPLE_COMMAND_TEST_CHILD";

    fn command_child(mode: &str) -> aterm_uds::spawnfd::Command {
        let mut command =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("this test executable"));
        command
            .args([
                "--exact",
                "apple::tests::signing_command_child",
                "--nocapture",
            ])
            .env(COMMAND_CHILD, mode);
        command
    }

    #[test]
    fn signing_command_child() {
        let Some(mode) = std::env::var_os(COMMAND_CHILD) else {
            return;
        };
        let mut input = Vec::new();
        std::io::stdin()
            .read_to_end(&mut input)
            .expect("closed stdin");
        assert!(
            input.is_empty(),
            "provisioning must not consume terminal input"
        );
        match mode.to_str().expect("test mode") {
            "ok" => {
                println!("ordinary command output");
                eprintln!("ordinary command diagnostic");
            }
            "fail" => {
                eprintln!("fixture signing failure\nmore detail");
                std::process::exit(7);
            }
            "block" => loop {
                std::thread::park();
            },
            _ => panic!("unknown fixture mode"),
        }
    }

    #[test]
    fn local_command_success_and_failure_keep_their_exit_meaning() {
        assert!(run_command(&mut command_child("ok"), COMMAND_WAIT).is_ok());
        assert_eq!(
            run_command(&mut command_child("fail"), COMMAND_WAIT),
            Err("fixture signing failure".to_string())
        );
        let absent = aterm_tempfile::tempdir().expect("tempdir");
        let error = run_command(
            &mut aterm_uds::spawnfd::command(absent.path().join("absent")),
            COMMAND_WAIT,
        )
        .expect_err("spawn failure");
        assert!(error.contains("could not start"), "{error}");
        assert!(!error.contains("requested termination"), "{error}");
    }

    /// A signing command that never answers is stopped at the deadline it was given, and the
    /// error names that deadline. Stopping and reaping it is `output_within`'s, which aterm-uds
    /// proves (`a_child_past_its_deadline_is_killed_and_reaped`); a child that never finishes
    /// meets a short deadline however slowly it starts, so this waits out no minute.
    #[test]
    fn a_signing_command_that_never_answers_is_stopped_at_its_deadline() {
        let error = run_command(&mut command_child("block"), Duration::from_secs(1))
            .expect_err("the blocked signing command must not pass");
        assert!(error.contains("did not finish within 1 s"), "{error}");
    }

    /// A read-only look names provision, with the operator's folder, as the step when it can
    /// make the on-disk certificate an identity; a refused certificate (named), no current one,
    /// or no key is the owner's errand at Apple, and a folder or key it could not read blocks.
    #[test]
    fn a_key_whose_certificate_is_on_disk_is_provisions_step_not_an_errand() {
        let key = Path::new("/nonexistent/devid-m9.key");
        let fake = Fake::new(World::new(&"0".repeat(40)));
        let kc = Keychains::new(Path::new("/nonexistent"), &fake);
        let blocked = |found: OnDisk, cert_dir: Option<&Path>| match read_only(
            &kc,
            "m9",
            Leave::Look,
            Vec::new(),
            Some(key),
            cert_dir,
            &|_| found.clone(),
        ) {
            Outcome::Blocked { what, fix } => {
                assert!(what.contains("'m9'") && what.contains("keychain"), "{what}");
                fix
            }
            _ => panic!("{found:?} is provision's step, not an errand"),
        };
        let fix = blocked(OnDisk::Unimported, None);
        assert!(
            fix.contains("provision --id m9`, which imports") && !fix.contains("--cert-dir"),
            "{fix}"
        );
        let fix = blocked(OnDisk::Unchained, Some(Path::new("/x/Downloads")));
        assert!(
            fix.contains("provision --id m9 --cert-dir /x/Downloads`, which installs"),
            "{fix}"
        );
        for (key, found) in [
            (
                Some(key),
                OnDisk::Refused(PathBuf::from("/x/Downloads/old.cer")),
            ),
            (Some(key), OnDisk::Nothing),
            (None, OnDisk::Unimported),
        ] {
            assert!(
                matches!(
                    read_only(&kc, "m9", Leave::Look, Vec::new(), key, None, &|_| found
                        .clone()),
                    Outcome::Todo { .. }
                ),
                "{key:?} {found:?}"
            );
        }
        for (why, fix) in [
            ("/x/locked: Permission denied (os error 13)", "readable"),
            ("could not read the public half of /k", "openssl rsa -in"),
        ] {
            match read_only(&kc, "m9", Leave::Look, Vec::new(), Some(key), None, &|_| {
                OnDisk::Unlooked(why.into())
            }) {
                Outcome::Blocked { what, fix: said } => {
                    assert!(what.contains(why) && said.contains(fix), "{what} / {said}");
                }
                _ => panic!("what a real run cannot read stops the check too: {why}"),
            }
        }
        let refused = OnDisk::Refused(PathBuf::from("/x/Downloads/old.cer"));
        match read_only(
            &kc,
            "m9",
            Leave::Look,
            Vec::new(),
            Some(key),
            Some(Path::new("/x/Downloads")),
            &|_| refused.clone(),
        ) {
            Outcome::Todo { what, next } => {
                assert!(what.contains("/x/Downloads/old.cer"), "{what}");
                assert!(
                    next.contains("provision --id m9 --cert-dir /x/Downloads`"),
                    "{next}"
                );
            }
            _ => panic!("a refused certificate is an errand at Apple"),
        }
    }

    /// A certificate on disk is judged by its key, its expiry and the keychain; a refused or
    /// expired one is passed over for another for the key, and the first refused is named when
    /// every one is refused.
    #[test]
    fn a_certificate_on_disk_is_judged_by_its_key_its_end_and_the_keychain() {
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        let dir = scratch.path();
        let fake = Fake::new(World::new(&"0".repeat(40)));
        let kc = Keychains::new(dir, &fake);
        let on_disk =
            |key: &Path, watch: &Watch, invalid: &[String]| on_disk(&kc, key, watch, invalid);
        let issuer_in_keychain = |leaf: &str| issuer_in_keychain(&kc, leaf);
        let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
        let openssl = |args: &[&str]| {
            assert!(
                capture("/usr/bin/openssl", args, None).is_some(),
                "openssl {args:?}"
            );
        };
        for key in ["k.pem", "other.pem"] {
            openssl(&["genrsa", "-out", &path(key), "2048"]);
        }
        let pem = path("c.pem");
        openssl(&[
            "req",
            "-new",
            "-x509",
            "-key",
            &path("k.pem"),
            "-subj",
            "/CN=probe",
            "-days",
            "30",
            "-out",
            &pem,
        ]);
        openssl(&[
            "x509",
            "-in",
            &pem,
            "-outform",
            "DER",
            "-out",
            &path("c.cer"),
        ]);
        let watch = Watch::new(None, Some(dir));
        let key = dir.join("k.pem");
        let cert = MatchedCert {
            path: dir.join("c.cer"),
            form: "DER",
        };
        let sha1 = cert_sha1(&cert).expect("a fingerprint");
        assert_eq!(
            [&[][..], &["0".repeat(40)][..]].map(|invalid| on_disk(&key, &watch, invalid)),
            [OnDisk::Unimported, OnDisk::Unimported]
        );
        assert_eq!(
            on_disk(&key, &watch, &[sha1.to_ascii_lowercase()]),
            OnDisk::Unchained
        );
        let seen =
            |watch: &Watch, invalid: &[String], secs, issuer: &dyn Fn(&str) -> Option<String>| {
                let look = look(&key, watch, invalid, secs, issuer).expect("look");
                (look.cert.map(|cert| cert.path), look.judged)
            };
        assert_eq!(
            seen(&watch, &[], 10 * 365 * 24 * 60 * 60, &issuer_in_keychain),
            (None, OnDisk::Nothing)
        );
        assert_eq!(
            on_disk(&dir.join("other.pem"), &watch, std::slice::from_ref(&sha1)),
            OnDisk::Nothing
        );
        let found = |_: &str| Some("issuer".to_owned());
        let refused = OnDisk::Refused(cert.path.clone());
        assert_eq!(
            seen(&watch, std::slice::from_ref(&sha1), 0, &found),
            (None, refused.clone())
        );
        assert_eq!(
            seen(&watch, std::slice::from_ref(&sha1), 0, &|_| None),
            (Some(cert.path.clone()), OnDisk::Unchained)
        );

        // The renewal lives in a folder read after the first, so the order is the test's.
        let later = dir.join("later");
        std::fs::create_dir(&later).expect("dir");
        let renewed = path("d.pem");
        openssl(&[
            "req",
            "-new",
            "-x509",
            "-key",
            &path("k.pem"),
            "-subj",
            "/CN=renewed",
            "-days",
            "400",
            "-out",
            &renewed,
        ]);
        let renewal = later.join("d.cer");
        openssl(&[
            "x509",
            "-in",
            &renewed,
            "-outform",
            "DER",
            "-out",
            &renewal.to_string_lossy(),
        ]);
        let renewed_sha1 = cert_sha1(&MatchedCert {
            path: renewal.clone(),
            form: "DER",
        })
        .expect("a fingerprint");
        let watch = Watch {
            named: Some(dir.to_path_buf()),
            own: Some(later),
        };
        let both = [sha1.clone(), renewed_sha1];
        let sixty_days = 60 * 24 * 60 * 60;
        assert_eq!(
            [
                seen(&watch, std::slice::from_ref(&sha1), 0, &found),
                seen(&watch, &[], sixty_days, &found),
                seen(&watch, &both, 0, &found),
            ],
            [
                (Some(renewal.clone()), OnDisk::Unimported),
                (Some(renewal), OnDisk::Unimported),
                (None, refused),
            ],
            "passed over: refused, ended; every one refused names the first"
        );

        // A folder that cannot be read, with nothing installable elsewhere, is no answer.
        let locked = dir.join("locked");
        let empty = dir.join("empty");
        std::fs::create_dir(&locked).expect("dir");
        std::fs::create_dir(&empty).expect("dir");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let blind = Watch {
            named: Some(locked.clone()),
            own: Some(empty),
        };
        let blind_look = on_disk(&key, &blind, &[]);
        let missing = dir.join("missing.pem");
        let unreadable_key = on_disk(&missing, &blind, &[]);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        assert!(
            matches!(&blind_look, OnDisk::Unlooked(why) if why.contains("locked")),
            "{blind_look:?}"
        );
        // The key's own error takes the slot-sparing fix, wherever it is reported.
        let OnDisk::Unlooked(why) = unreadable_key else {
            panic!("{unreadable_key:?}");
        };
        assert!(
            matches!(unlooked(why, &missing), Outcome::Blocked { fix, .. } if fix.contains("openssl rsa -in")),
            "a key that cannot be read is proven before a slot is spent"
        );
    }

    /// A scratch HOME holding a Developer ID pair in `~/.aterm/apple` as the vault fills it
    /// (`devid-<id>.key` and `.cer`, no request), and its certificate's SHA-1.
    fn vault_filled_home(id: &str) -> (aterm_tempfile::TempDir, PathBuf, String) {
        let scratch = aterm_tempfile::tempdir().expect("scratch HOME");
        let home = std::fs::canonicalize(scratch.path()).expect("canonical scratch");
        let apple = apple_dir_in(&home);
        std::fs::create_dir_all(&apple).expect(".aterm/apple");
        std::fs::create_dir_all(home.join("Library/Keychains")).expect("Keychains");
        let key = key_path(&apple, id).to_string_lossy().into_owned();
        let pem = home.join("cert.pem").to_string_lossy().into_owned();
        let cer = apple.join(format!("devid-{id}.cer"));
        let team = pins::APPLE_TEAM_ID;
        let subject =
            format!("/CN=Developer ID Application: Placeholder Org ({team})/OU={team}/C=US");
        for args in [
            &["genrsa", "-out", &key, "2048"][..],
            &[
                "req", "-new", "-x509", "-key", &key, "-subj", &subject, "-days", "30", "-out",
                &pem,
            ],
            &[
                "x509",
                "-in",
                &pem,
                "-outform",
                "DER",
                "-out",
                &cer.to_string_lossy(),
            ],
        ] {
            assert!(
                capture("/usr/bin/openssl", args, None).is_some(),
                "openssl {args:?}"
            );
        }
        let sha1 = cert_sha1(&MatchedCert {
            path: cer,
            form: "DER",
        })
        .expect("a fingerprint");
        (scratch, home, sha1)
    }

    /// Whether a logged call names the login keychain other than to keep it on the search
    /// list where it was.
    fn names_login(call: &[String], login: &Path) -> bool {
        let keeps_it = call[1] == "list-keychains"
            && call
                .iter()
                .position(|a| a == "-s")
                .is_some_and(|at| call.get(at + 1).map(String::as_str) == login.to_str());
        !keeps_it && call.iter().any(|a| a.contains("login.keychain"))
    }

    /// `ship provision` on a fresh Mac the vault filled: the pair goes into the release
    /// keychain, made, opened and given its partition list with its own password; the search
    /// list keeps login first; the test signature names the release keychain; and no call
    /// names the login keychain. A second run makes, imports and rewrites nothing.
    #[test]
    fn provision_on_a_fresh_home_fills_the_release_keychain_and_never_the_login_one() {
        let (_scratch, home, sha1) = vault_filled_home("m9");
        let login = crate::release_keychain::login_keychain_path(&home);
        let mut world = World::new(&sha1);
        world.keychains.insert(login.clone(), Kc::default());
        world.search_list = vec![login.clone()];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        let release = kc.release();

        match acquire_in(&kc, "m9", true, Leave::Write, None) {
            Outcome::Ready { ids, .. } => assert_eq!(ids, std::slice::from_ref(&sha1)),
            Outcome::Blocked { what, fix } => panic!("blocked: {what} / {fix}"),
            _ => panic!("a vault-filled pair provisions in one run"),
        }
        {
            let world = fake.0.borrow();
            for call in &world.log {
                assert!(!names_login(call, &login), "names login: {call:?}");
            }
            let filled = &world.keychains[&release];
            assert!(filled.partition && !filled.locked, "{filled:?}");
            assert_eq!((filled.certs.clone(), filled.keys), (vec![sha1.clone()], 1));
            assert_eq!(world.keychains[&login], Kc::default(), "login untouched");
            assert_eq!(world.search_list, [login.clone(), release.clone()]);
        }
        let probes = fake.calls("codesign", "--force");
        assert_eq!(probes.len(), 1, "{probes:?}");
        assert!(
            probes[0]
                .windows(2)
                .any(|w| w[0] == "--keychain" && Path::new(&w[1]) == release),
            "{probes:?}"
        );

        let before = fake.0.borrow().keychains.clone();
        fake.0.borrow_mut().log.clear();
        assert!(matches!(
            acquire_in(&kc, "m9", true, Leave::Write, None),
            Outcome::Ready { .. }
        ));
        assert_eq!(fake.0.borrow().keychains, before);
        for sub in ["create-keychain", "import"] {
            assert!(
                fake.calls("security", sub).is_empty(),
                "a second run ran {sub}"
            );
        }
        assert!(
            fake.writes().iter().all(|c| c[1] != "list-keychains"),
            "a second run rewrote the search list"
        );

        // A key codesign cannot use there, its partition list not settable now, is provision's
        // to fix, with no login password.
        {
            let mut world = fake.0.borrow_mut();
            world.keychains.get_mut(&release).unwrap().partition = false;
            world.fail = Some("set-key-partition-list");
        }
        let Outcome::Blocked { fix, .. } = verdict(&kc, "m9", vec![sha1.clone()], Leave::Write)
        else {
            panic!("a key codesign cannot use is blocked");
        };
        assert!(fix.contains("provision --id m9"), "{fix}");
        assert!(!fix.contains("asks for your login"), "{fix}");

        // A release keychain that cannot be listed is re-run, never sent to the login keychain.
        fake.0.borrow_mut().fail = Some("find-identity");
        let Outcome::Blocked { fix, .. } = verdict(&kc, "m9", vec![sha1], Leave::Write) else {
            panic!("an unreadable keychain is blocked");
        };
        assert!(fix.contains("could not be read"), "{fix}");
        assert!(!fix.contains("login"), "{fix}");
    }

    /// A run without leave to change follows `leave` on a release keychain that holds the
    /// identity, its key's partition list gone: `ship provision --check` and a dry release
    /// only open it (no write but the unlock, no codesign) and say so; a real release's audit
    /// re-asserts the list and test-signs before the publish.
    #[test]
    fn a_check_only_looks_and_a_real_release_audit_test_signs() {
        let sha1 = "C".repeat(40);
        let scratch = aterm_tempfile::tempdir().expect("scratch HOME");
        let home = std::fs::canonicalize(scratch.path()).expect("canonical scratch");
        let mut world = World::new(&sha1);
        world.search_list = vec![crate::release_keychain::login_keychain_path(&home)];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect("install");
        let release = kc.release();
        let stop = || {
            let mut world = fake.0.borrow_mut();
            let kept = world.keychains.get_mut(&release).unwrap();
            kept.partition = false;
            kept.locked = true;
            world.log.clear();
        };

        stop();
        match acquire_in(&kc, "m9", false, Leave::Look, None) {
            Outcome::Ready { ids, note } => {
                assert_eq!(ids, std::slice::from_ref(&sha1));
                assert!(note.is_some_and(|n| n.contains(OPENED_NOTE)));
            }
            Outcome::Blocked { what, fix } => panic!("a look is ready: {what} / {fix}"),
            _ => panic!("a look at a held identity is ready"),
        }
        let changed: Vec<_> = fake
            .writes()
            .into_iter()
            .filter(|c| c[1] != "unlock-keychain")
            .collect();
        assert!(changed.is_empty(), "a look only unlocks: {changed:?}");
        assert!(fake.calls("codesign", "--force").is_empty());

        stop();
        match acquire_in(&kc, "m9", false, Leave::Write, None) {
            Outcome::Ready { note, .. } => {
                assert!(!note.is_some_and(|n| n.contains(OPENED_NOTE)));
            }
            Outcome::Blocked { what, fix } => panic!("the audit signs: {what} / {fix}"),
            _ => panic!("a real release's audit test-signs"),
        }
        assert!(fake.0.borrow().keychains[&release].partition);
        assert!(!fake.calls("codesign", "--force").is_empty());
        let added: Vec<_> = fake
            .writes()
            .into_iter()
            .filter(|c| !matches!(c[1].as_str(), "unlock-keychain" | "set-key-partition-list"))
            .collect();
        assert!(added.is_empty(), "nothing is imported or listed: {added:?}");
    }

    /// A Mac provisioned before the release keychain keeps signing from its login keychain:
    /// provision and its check accept the identity there, write nothing, and only the fix for
    /// a key codesign cannot use there names the login password.
    #[test]
    fn a_login_keychain_provisioned_mac_is_accepted_with_nothing_written() {
        let sha1 = "B".repeat(40);
        let scratch = aterm_tempfile::tempdir().expect("scratch HOME");
        let home = std::fs::canonicalize(scratch.path()).expect("canonical scratch");
        let login = crate::release_keychain::login_keychain_path(&home);
        let mut world = World::new(&sha1);
        world.keychains.insert(login.clone(), Kc::holding(&sha1));
        world.search_list = vec![login.clone()];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        for may_change in [true, false] {
            match acquire_in(&kc, "m9", may_change, Leave::Look, None) {
                Outcome::Ready { ids, .. } => assert_eq!(ids, std::slice::from_ref(&sha1)),
                Outcome::Blocked { what, fix } => panic!("blocked: {what} / {fix}"),
                _ => panic!("the login keychain's identity is accepted"),
            }
        }
        assert!(fake.writes().is_empty(), "{:?}", fake.writes());
        let probes = fake.calls("codesign", "--force");
        assert!(
            !probes.is_empty()
                && probes
                    .iter()
                    .all(|p| !p.contains(&"--keychain".to_string())),
            "codesign finds it on the search list as it always has: {probes:?}"
        );
        for leave in [Leave::Write, Leave::Look] {
            assert_eq!(pinned_identity_proves(&kc, &sha1, leave), Ok(Proof::Signed));
        }

        fake.0
            .borrow_mut()
            .keychains
            .get_mut(&login)
            .unwrap()
            .partition = false;
        let Outcome::Blocked { fix, .. } = verdict(&kc, "m9", vec![sha1.clone()], Leave::Look)
        else {
            panic!("a key codesign cannot use is blocked");
        };
        assert!(
            fix.contains("asks for your login keychain password"),
            "{fix}"
        );
        assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    }

    #[test]
    fn nothing_outside_aterms_own_folder_is_read_unless_the_operator_named_it() {
        let own = env_watch(None);
        let dirs = own.dirs();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        assert!(dirs[0].ends_with(".aterm/apple"), "{dirs:?}");
        assert!(
            !dirs
                .iter()
                .any(|d| d.to_string_lossy().contains("Downloads")),
            "{dirs:?}"
        );
        let named = env_watch(Some(Path::new("/Users//x/Downloads")));
        let dirs = named.dirs();
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert_eq!(
            dirs[0],
            Path::new("/Users//x/Downloads"),
            "the named folder is looked in first"
        );
        assert!(dirs[1].ends_with(".aterm/apple"), "{dirs:?}");
    }

    #[test]
    fn the_consent_note_names_only_a_guarded_folder_the_operator_named() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME in tests"));
        assert!(env_watch(None).consent_note().is_none());
        let guarded = env_watch(Some(&home.join("Downloads")));
        let downloads_note = guarded
            .consent_note()
            .expect("Downloads is a guarded folder");
        assert!(
            downloads_note.contains("would like to access files"),
            "{downloads_note}"
        );
        let app_data = env_watch(Some(
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
        let plain = env_watch(Some(&home.join(".aterm").join("apple-scratch")));
        assert!(plain.consent_note().is_none());
        let lookalike = env_watch(Some(&home.join("Downloadsx")));
        assert!(lookalike.consent_note().is_none());
    }

    #[test]
    fn the_certificate_is_selected_by_digest_not_by_position() {
        let stream = "SHA-256 hash: AAAA\nSHA-1 hash: 1111111111111111111111111111111111abcdef\n\
                      -----BEGIN CERTIFICATE-----\nolder\n-----END CERTIFICATE-----\n\
                      SHA-256 hash: BBBB\nSHA-1 hash: 2222222222222222222222222222222222FEDCBA\n\
                      -----BEGIN CERTIFICATE-----\nnewer\n-----END CERTIFICATE-----\n";
        let second = leaf_pem_by_sha1(stream, "2222222222222222222222222222222222fedcba")
            .expect("the second certificate");
        assert!(second.contains("newer"), "{second}");
        assert!(!second.contains("older"), "{second}");
        assert!(
            leaf_pem_by_sha1(stream, "1111111111111111111111111111111111ABCDEF")
                .is_some_and(|p| p.contains("older"))
        );
        assert!(leaf_pem_by_sha1(stream, &"3".repeat(40)).is_none());
    }

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
        assert!(
            key_id_from_text("X509v3 Basic Constraints: critical\n", AUTHORITY_KEY_ID).is_none()
        );
    }

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

    /// While the errand waits, a wrong `.cer` is named on the look it arrives on and not while it
    /// stays; one that left the folder leaves what the wait reports and is named again if it
    /// comes back.
    #[test]
    fn a_wrong_certificate_is_named_once_when_it_arrives() {
        let cer = |name: &str| PathBuf::from(format!("/w/{name}.cer"));
        let mut first = Vec::new();
        assert_eq!(take_arrivals(vec![cer("a")], &mut first), [cer("a")]);
        let mut rejected = vec![cer("a")];
        assert_eq!(
            take_arrivals(vec![cer("a"), cer("b")], &mut rejected),
            [cer("b")]
        );
        assert_eq!(rejected, [cer("a"), cer("b")]);
        assert!(take_arrivals(vec![cer("a"), cer("b")], &mut rejected).is_empty());
        assert_eq!(rejected, [cer("a"), cer("b")]);
        assert!(take_arrivals(vec![cer("b")], &mut rejected).is_empty());
        assert_eq!(rejected, [cer("b")], "a .cer that left is not reported");
        assert_eq!(
            take_arrivals(vec![cer("a"), cer("b")], &mut rejected),
            [cer("a")],
            "a .cer that came back is named again"
        );
    }
}
