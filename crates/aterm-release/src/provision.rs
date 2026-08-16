// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cargo ship provision --id <machine-id>` — a fresh checkout becomes a publishing
//! machine in ONE command, with the paper phrase as the only human input.
//!
//! The verb is the whole of RELEASING.md § "Promoting a new release machine", welded
//! together the same way `setup`/`join` welded the retired `master-new`/`machine-mint`/
//! `roster-verify` trio: each manual step was half a provisioning an operator could get
//! wrong, so none of them is manual any more.
//!
//!   1. **Seed the roster pair.** `dist/` is gitignored, so a fresh clone has no
//!      `aterm-machines.toml` — but the pair ships as assets on every channel release,
//!      and the channel is anonymously readable (a cut invariant, proved by
//!      `prove_channel_is_anonymously_readable`), so an unauthenticated `curl` of the
//!      latest release fetches it before any token exists on this machine. Every
//!      candidate — fetched or already in `dist/` — is verified under the committed
//!      `pins::PAPER_MASTER_PUBKEYS` before it is compared, and the NEWEST generation
//!      wins: a local pair ahead of the channel (an unpublished roster edit) is kept,
//!      never downgraded, which is the same generation-first rule the client runs.
//!   2. **Mint, in-process.** The `atpkg-keys` join ceremony runs as a library —
//!      `preflight → verify_master → plan → write_pins → write_rest` — so the master
//!      phrase is typed once, on `/dev/tty` with echo off, into the same code that
//!      enforces every leak rule (never argv, never env, never a file), and no second
//!      binary needs to exist. A machine that already holds `~/.aterm/machine.key`
//!      skips the mint and is audited instead: the verb is idempotent.
//!   3. **Audit what software cannot conjure.** The Apple Developer ID certificate's
//!      private half lives only in another Mac's keychain; the notary credential and
//!      the GitHub tokens are issued elsewhere. The verb proves what is present and
//!      names the exact remedy for what is not, ending in a READY TO CUT verdict.
//!
//! What this verb deliberately does NOT do: generate anything `--id` collides with (the
//! roster refuses id reuse), transfer any private key between machines (the design
//! forbids it — revocation is per-id), or touch `pins.rs` (a join never does).

#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
use aterm_update_core::pins;
#[cfg(unix)]
use aterm_update_core::roster::{self, Roster};

#[cfg(unix)]
use crate::ledger::{Error, Result};
#[cfg(unix)]
use crate::publish::step;
#[cfg(unix)]
use crate::{machines, mirror, publish};

/// POSIX-only, exactly like the engine it drives: `atpkg-keys` compiles empty on
/// Windows (`#![cfg(unix)]`), because the master phrase is read from `/dev/tty`.
#[cfg(not(unix))]
pub fn run_provision(_repo: &std::path::Path, _id: &str) -> crate::ledger::Result<()> {
    Err(crate::ledger::Error::new(
        "provision is POSIX-only: the provisioning engine reads the master phrase from /dev/tty",
    ))
}

#[cfg(unix)]
pub fn run_provision(repo: &Path, id: &str) -> Result<()> {
    // The same id rules the roster enforces, checked before anything network-shaped.
    atpkg_keys::provision::vet_machine_id(id).map_err(Error::new)?;

    let manifest = std::fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("read {}/Cargo.toml: {e}", repo.display())))?;
    let slug = mirror::update_channel_slug(&manifest)?.ok_or_else(|| {
        Error::new(
            "no update channel is committed ([workspace.metadata.aterm] update_channel), so \
             there is no release to seed the roster from and no channel to provision for",
        )
    })?;
    println!("aterm-release · provision {id} (channel {slug})");

    // ---- 1. the roster pair: newest verified generation into dist/ ----------------
    let roster_path = repo.join("dist").join(roster::ROSTER_ASSET);
    let (local, local_warn) = read_local_candidate(&roster_path);
    if let Some(warn) = local_warn {
        step("roster", &warn);
    }
    let fetched = fetch_channel_candidate(&slug);
    let (chosen, install, how) = choose_candidate(&roster_path, &slug, local, fetched)?;
    if install {
        install_pair(&roster_path, &chosen)?;
    }
    step("roster", &how);

    // ---- 2. this machine's identity: mint once, audit forever ---------------------
    let home = std::env::var("HOME").map_err(|_| Error::new("HOME is not set"))?;
    let key_path = Path::new(&home).join(atpkg_keys::provision::MACHINE_KEY_REL);
    let identity_path = Path::new(&home).join(atpkg_keys::provision::MACHINE_PUB_REL);

    let minted = if key_path.exists() {
        let identity = machines::MachineIdentity::read(&identity_path)?.ok_or_else(|| {
            Error::new(format!(
                "{} exists but {} is missing — a half-provisioned machine. The pair is \
                 written together by the join ceremony; restore machine.toml from wherever \
                 this key came from, or move the key aside and re-run provision to mint a \
                 fresh identity under a NEW id",
                key_path.display(),
                identity_path.display(),
            ))
        })?;
        if identity.id != id {
            return Err(Error::new(format!(
                "this machine is already provisioned as '{}' — run `cargo ship provision \
                 --id {}` to audit it. A machine never re-mints under a second id: revoke \
                 the old id first if it must be replaced (`atpkg-keys machine-revoke`)",
                identity.id, identity.id,
            )));
        }
        step(
            "identity",
            &format!("already provisioned as '{id}' — auditing, not re-minting"),
        );
        false
    } else {
        mint(repo, id, &roster_path)?;
        true
    };

    // Bind identity to the (possibly just re-signed) roster on disk — the proof that a
    // cut from this machine would pass `authorize_cut`, and the catch for a `dist/`
    // that was swept after a join.
    let identity = machines::MachineIdentity::read(&identity_path)?.ok_or_else(|| {
        Error::new(format!(
            "the join reported success but {} is unreadable",
            identity_path.display()
        ))
    })?;
    let doc = machines::RosterDocument::read(&roster_path)?;
    let current = admit_candidate(pins::PAPER_MASTER_PUBKEYS, doc.bytes, doc.signature)
        .map_err(|e| Error::new(format!("{}: {e}", roster_path.display())))?;
    let authority = match membership(&current.roster, &identity.id, &identity.pubkey) {
        Membership::Listed => format!(
            "the roster (seq {}) names '{}' with this machine's key",
            current.roster.roster_seq, identity.id
        ),
        Membership::Revoked => {
            return Err(Error::new(format!(
                "'{}' is REVOKED on the roster (seq {}) — an id never returns. Move \
                 ~/.aterm/machine.key and machine.toml aside and provision a new id",
                identity.id, current.roster.roster_seq,
            )));
        }
        Membership::WrongKey { rostered } => {
            return Err(Error::new(format!(
                "the roster (seq {}) names '{}' with a DIFFERENT key ({}…) than this \
                 machine holds — either this dist/ pair is stale (copy the newest \
                 aterm-machines.toml + .sig from the machine that last edited the \
                 roster) or the id was re-minted elsewhere and this key must be retired",
                current.roster.roster_seq,
                identity.id,
                rostered.chars().take(12).collect::<String>(),
            )));
        }
        Membership::Absent => {
            return Err(Error::new(format!(
                "this machine holds a key but the newest roster available here (seq {}) \
                 does not name '{}' — if the join's re-signed pair was lost (dist/ is \
                 gitignored and can be swept), restore aterm-machines.toml AND .sig from \
                 the machine holding the newest generation, or from the release that \
                 shipped it",
                current.roster.roster_seq, identity.id,
            )));
        }
    };
    step("authority", &authority);

    // ---- 3. the rest of the publishing stack: prove or name the remedy ------------
    let checks = [
        ("apple", apple_identity_check()),
        ("notary", notary_check()),
        ("github", gh_check()),
        ("channel", channel_token_check(&slug)),
    ];
    for (label, check) in &checks {
        print_check(label, check);
    }
    // Not a pass/fail check because the profile has no conventional path — it is named
    // on the cut command line. But a keychain-only audit would bless a machine that
    // still cannot cut: `resolve_apple_tier` requires a profile naming the notarytool
    // credential, so say so here rather than at minute twenty of a cut.
    step(
        "profile",
        "a Tier APPLE cut names a credentials profile (`--release-credentials <path>`, \
         0600) carrying notary_profile and signing_identity_sha1 — write one now if \
         this machine has none",
    );

    if minted {
        println!();
        println!("=== PROPAGATE ===");
        println!(
            "  the roster is now at seq {} and lives only in THIS checkout's dist/ — it",
            current.roster.roster_seq
        );
        println!("  becomes authoritative when published: it ships with the next `cargo ship");
        println!("  cut` and the next `UPLOAD=1 tools/atpkg-index.sh`. Until then, copy");
        println!("  dist/aterm-machines.toml AND its .sig to every other publishing machine —");
        println!("  the roster_seq baseline refuses a publish from an older generation.");
    }

    let fails = checks
        .iter()
        .filter(|(_, c)| matches!(c, Check::Fail { .. }))
        .count();
    let skipped = checks
        .iter()
        .any(|(_, c)| matches!(c, Check::Skip(_)));
    println!();
    if fails == 0 {
        let host = if skipped {
            " (Apple checks skipped — cuts themselves run on macOS)"
        } else {
            ""
        };
        println!(
            "READY TO CUT: yes{host} — `cargo ship cut --dry-run` is the next proof (a \
             rostered-key cut currently also needs --strand-pre-roster-clients; the cut \
             names the flag itself when it applies)"
        );
    } else {
        println!(
            "READY TO CUT: not yet — {fails} item(s) above name their remedy; re-run \
             `cargo ship provision --id {id}` to re-audit"
        );
    }
    Ok(())
}

/// The in-process join ceremony: the exact `preflight → verify_master → plan →
/// write_pins → write_rest` sequence the `atpkg-keys` CLI runs, so every rule that
/// tool enforces — the phrase from `/dev/tty` only, the mistype caught against the
/// committed anchor before any write, `create_new` on the key file — holds here
/// unchanged. RELEASE-KEYS.md's "signing is in-process" rule extends to minting: no
/// second binary, no key path on any command line.
#[cfg(unix)]
fn mint(repo: &Path, id: &str, roster_path: &Path) -> Result<()> {
    use atpkg_keys::{master, provision as prov};

    // Before anything sensitive exists in this process.
    master::forbid_core_dumps();

    let paths = prov::Paths {
        pins: utf8_path(&repo.join(prov::PINS_REL))?,
        roster: utf8_path(roster_path)?,
        key: prov::home_path(prov::MACHINE_KEY_REL).map_err(Error::new)?,
        machine_pub: prov::home_path(prov::MACHINE_PUB_REL).map_err(Error::new)?,
        // This is the checkout's own discovered anchor, not another tree's.
        pins_explicit: false,
    };
    let pre = prov::preflight(prov::Verb::Join, id, prov::DEFAULT_HEAD_ID, &paths)
        .map_err(Error::new)?;
    let phrase = master::prompt_for_master(
        "master phrase (52 characters, echo off; spaces, case, and o/i/l are forgiven): ",
    )
    .map_err(Error::new)?;
    let seed = phrase.seed();
    println!(
        "master fingerprint: {}  (compare with the paper)",
        seed.fingerprint().map_err(Error::new)?
    );
    prov::verify_master(&pre, &seed).map_err(Error::new)?;
    let planned = prov::plan(pre, &seed, now_unix()?).map_err(Error::new)?;
    prov::write_pins(&planned).map_err(Error::new)?;
    let report = prov::write_rest(planned).map_err(Error::new)?;
    for line in prov::render_report(&report) {
        println!("{line}");
    }
    Ok(())
}

/// A roster pair that verified under the committed paper master and parsed.
/// Nothing here is secret — derived `Debug` is deliberate.
#[cfg(unix)]
#[derive(Debug)]
struct Candidate {
    bytes: Vec<u8>,
    sig: Vec<u8>,
    roster: Roster,
}

/// Verify-then-parse — the only admission path, same rule as every client: no
/// unverified roster bytes are ever compared, installed, or trusted for a seq.
#[cfg(unix)]
fn admit_candidate(
    master_pubkeys: &[&str],
    bytes: Vec<u8>,
    sig: Vec<u8>,
) -> std::result::Result<Candidate, String> {
    let verified = roster::verify_roster(master_pubkeys, bytes.clone(), &sig)
        .map_err(|e| format!("the roster did not verify under the committed paper master ({e:?})"))?;
    let parsed = Roster::parse(&verified)
        .map_err(|e| format!("the roster verified but did not parse ({e:?})"))?;
    Ok(Candidate {
        bytes,
        sig,
        roster: parsed,
    })
}

/// The pair already in `dist/`, admitted — or `None` with a warning when it exists but
/// cannot be admitted. Unverifiable local state is deliberately NOT fatal: garbage
/// authorizes nothing (a torn copy, a half-copied pair), and a verified channel fetch
/// replacing it is an upgrade from nothing — but it is said out loud, because the torn
/// copy might be the front half of somebody's newer-generation hand-copy.
#[cfg(unix)]
fn read_local_candidate(roster_path: &Path) -> (Option<Candidate>, Option<String>) {
    if !roster_path.exists() {
        return (None, None);
    }
    let doc = match machines::RosterDocument::read(roster_path) {
        Ok(doc) => doc,
        Err(e) => return (None, Some(format!("dist/ pair unusable ({e}) — reseeding"))),
    };
    match admit_candidate(pins::PAPER_MASTER_PUBKEYS, doc.bytes, doc.signature) {
        Ok(c) => (Some(c), None),
        Err(e) => (
            None,
            Some(format!(
                "dist/ pair unusable ({e}) — reseeding; if that pair was a fresh hand-copy, \
                 re-copy BOTH files from the source machine"
            )),
        ),
    }
}

/// The latest channel release's pair, fetched anonymously and admitted.
#[cfg(unix)]
fn fetch_channel_candidate(slug: &str) -> std::result::Result<Candidate, String> {
    let bytes = curl_fetch(&release_asset_url(slug, roster::ROSTER_ASSET), 65_536)?;
    let sig = curl_fetch(&release_asset_url(slug, roster::ROSTER_SIG_ASSET), 4_096)?;
    admit_candidate(pins::PAPER_MASTER_PUBKEYS, bytes, sig)
}

/// `https://github.com/<slug>/releases/latest/download/<asset>` — the anonymous asset
/// path, deliberately not `gh`: a machine being provisioned has no tokens yet, and the
/// roster's authority is its master signature, never its transport.
#[cfg(unix)]
fn release_asset_url(slug: &str, asset: &str) -> String {
    format!("https://github.com/{slug}/releases/latest/download/{asset}")
}

/// One bounded anonymous download. The caps mirror the update client's own roster
/// limits (64 KiB body, 4 KiB signature) — a seed must never accept more than the
/// fleet would.
#[cfg(unix)]
fn curl_fetch(url: &str, cap: usize) -> std::result::Result<Vec<u8>, String> {
    let cap_s = cap.to_string();
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "60",
            "--max-filesize",
            &cap_s,
            url,
        ])
        .output()
        .map_err(|e| format!("failed to run curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("curl {url}: {}", err.trim()));
    }
    // --max-filesize can miss chunked transfers; the cap is the contract either way.
    if out.stdout.len() > cap {
        return Err(format!("{url}: exceeded the {cap}-byte cap"));
    }
    if out.stdout.is_empty() {
        return Err(format!("{url}: zero bytes"));
    }
    Ok(out.stdout)
}

/// Newest-generation-wins between what `dist/` holds and what the channel serves —
/// pure, so the rule is testable without a network. Returns the chosen candidate,
/// whether it must be written into `dist/`, and the transcript line saying why.
#[cfg(unix)]
fn choose_candidate(
    roster_path: &Path,
    slug: &str,
    local: Option<Candidate>,
    fetched: std::result::Result<Candidate, String>,
) -> Result<(Candidate, bool, String)> {
    match (local, fetched) {
        (None, Err(e)) => Err(Error::new(format!(
            "no roster pair anywhere: {} is absent and the channel fetch failed ({e}). \
             Copy aterm-machines.toml AND aterm-machines.toml.sig into dist/ from a \
             provisioned machine or from a release's assets ({} and …sig), then re-run",
            roster_path.display(),
            release_asset_url(slug, roster::ROSTER_ASSET),
        ))),
        (None, Ok(f)) => {
            let how = format!(
                "seeded from the latest channel release (roster_seq {})",
                f.roster.roster_seq
            );
            Ok((f, true, how))
        }
        (Some(l), Err(e)) => {
            let how = format!(
                "using the dist/ pair (roster_seq {}); channel fetch failed ({e})",
                l.roster.roster_seq
            );
            Ok((l, false, how))
        }
        (Some(l), Ok(f)) => {
            let (ls, fs) = (l.roster.roster_seq, f.roster.roster_seq);
            if fs > ls {
                Ok((
                    f,
                    true,
                    format!("upgraded the dist/ pair: roster_seq {ls} → {fs} (channel release)"),
                ))
            } else if fs < ls {
                Ok((
                    l,
                    false,
                    format!(
                        "the dist/ pair (roster_seq {ls}) is AHEAD of the channel ({fs}) — an \
                         unpublished roster edit; keeping the newer generation"
                    ),
                ))
            } else {
                Ok((
                    l,
                    false,
                    format!("the dist/ pair already matches the channel (roster_seq {ls})"),
                ))
            }
        }
    }
}

/// Write the chosen pair into `dist/` — body then signature, each through a staged
/// sibling and an atomic rename, so no reader ever sees a half-written file. A crash
/// between the two renames leaves a mismatched pair, which the next run refuses to
/// verify and reseeds — torn state self-heals, it never authorizes.
#[cfg(unix)]
fn install_pair(roster_path: &Path, c: &Candidate) -> Result<()> {
    if let Some(dir) = roster_path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::new(format!("create {}: {e}", dir.display())))?;
    }
    let sig_path = machines::RosterDocument::signature_path(roster_path);
    stage_and_rename(roster_path, &c.bytes)?;
    stage_and_rename(&sig_path, &c.sig)?;
    Ok(())
}

#[cfg(unix)]
fn stage_and_rename(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut staged = path.as_os_str().to_owned();
    staged.push(".provision.tmp");
    let staged = PathBuf::from(staged);
    std::fs::write(&staged, bytes)
        .map_err(|e| Error::new(format!("write {}: {e}", staged.display())))?;
    std::fs::rename(&staged, path)
        .map_err(|e| Error::new(format!("rename {} into place: {e}", staged.display())))
}

/// Where this machine's identity stands in a roster. `Revoked` outranks a listing —
/// the same precedence the client applies.
#[cfg(unix)]
enum Membership {
    Listed,
    WrongKey { rostered: String },
    Revoked,
    Absent,
}

#[cfg(unix)]
fn membership(r: &Roster, id: &str, pubkey: &str) -> Membership {
    if r.revoked.iter().any(|x| x == id) {
        return Membership::Revoked;
    }
    match r.machines.iter().find(|m| m.id == id) {
        Some(m) if m.pubkey == pubkey => Membership::Listed,
        Some(m) => Membership::WrongKey {
            rostered: m.pubkey.clone(),
        },
        None => Membership::Absent,
    }
}

/// One audit line: proven, missing-with-remedy, or not applicable on this host.
#[cfg(unix)]
enum Check {
    Pass(String),
    Fail { what: String, fix: String },
    Skip(String),
}

#[cfg(unix)]
fn print_check(label: &str, c: &Check) {
    match c {
        Check::Pass(msg) => step(label, msg),
        Check::Skip(msg) => step(label, &format!("skipped — {msg}")),
        Check::Fail { what, fix } => {
            step(label, &format!("MISSING — {what}"));
            step("", &format!("fix: {fix}"));
        }
    }
}

/// The Developer ID Application identities for `team_id` in a `security find-identity
/// -v -p codesigning` listing — pure over the captured output, so it is testable
/// without a keychain. Returns the 40-hex SHA-1 of each matching line.
#[cfg(unix)]
fn devid_identities(listing: &str, team_id: &str) -> Vec<String> {
    let team_tag = format!("({team_id})");
    listing
        .lines()
        .filter(|l| l.contains("Developer ID Application:") && l.contains(&team_tag))
        .filter_map(|l| {
            l.split_whitespace()
                .find(|t| t.len() == 40 && t.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .map(str::to_string)
        .collect()
}

/// The one secret `provision` cannot fetch or mint: the Developer ID certificate's
/// private half exists only in an already-provisioned Mac's keychain, so the remedy is
/// the one sanctioned transfer in the whole scheme — a passphrase-protected `.p12`,
/// moved by hand, deleted after import.
#[cfg(unix)]
fn apple_identity_check() -> Check {
    if !cfg!(target_os = "macos") {
        return Check::Skip("cuts run on macOS (Tier APPLE); no keychain on this host".into());
    }
    match Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
    {
        Err(e) => Check::Fail {
            what: format!("could not run `security find-identity`: {e}"),
            fix: "install the Xcode command-line tools (`xcode-select --install`)".into(),
        },
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let ids = devid_identities(&text, pins::APPLE_TEAM_ID);
            match ids.len() {
                0 => Check::Fail {
                    what: format!(
                        "no `Developer ID Application` certificate for team {} in the keychain",
                        pins::APPLE_TEAM_ID
                    ),
                    fix: "on an already-provisioned Mac: Keychain Access → export the \
                          Developer ID Application certificate WITH its private key as a \
                          passphrase-protected .p12 → move it here over scp/AirDrop → \
                          double-click to import → delete the .p12 from both machines"
                        .into(),
                },
                1 => Check::Pass(format!("Developer ID Application [{}]", ids[0])),
                n => Check::Pass(format!(
                    "{n} Developer ID Application certificates — the credentials profile's \
                     signing_identity_sha1 disambiguates at cut time"
                )),
            }
        }
    }
}

#[cfg(unix)]
fn notary_check() -> Check {
    if !cfg!(target_os = "macos") {
        return Check::Skip("cuts run on macOS (Tier APPLE); no keychain on this host".into());
    }
    // notarytool stores its credential as a keychain generic password under this
    // service name; presence is what a cut needs, the profile name is in the
    // credentials file.
    match Command::new("security")
        .args(["find-generic-password", "-s", "com.apple.gke.notary.tool"])
        .output()
    {
        Ok(out) if out.status.success() => {
            Check::Pass("a notarytool keychain credential exists".into())
        }
        _ => Check::Fail {
            what: "no notarytool credential in the keychain".into(),
            fix: format!(
                "xcrun notarytool store-credentials notary --apple-id <your-apple-id> \
                 --team-id {} (password: an app-specific password from appleid.apple.com)",
                pins::APPLE_TEAM_ID
            ),
        },
    }
}

#[cfg(unix)]
fn gh_check() -> Check {
    match Command::new("gh").args(["auth", "status"]).output() {
        Ok(out) if out.status.success() => Check::Pass("gh is authenticated".into()),
        Ok(_) => Check::Fail {
            what: "gh is not authenticated".into(),
            fix: "gh auth login".into(),
        },
        Err(e) => Check::Fail {
            what: format!("could not run gh: {e}"),
            fix: "install the GitHub CLI (`brew install gh`), then `gh auth login`".into(),
        },
    }
}

/// The channel repo is a different org than the dev remote, so `gh`'s default account
/// cannot publish there — the cut threads a dedicated token from a file.
#[cfg(unix)]
fn channel_token_check(slug: &str) -> Check {
    let where_it_lives = publish::channel_token_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.secrets/gh_access_token_alabsystems".into());
    match publish::channel_token() {
        Some(_) => Check::Pass("channel token present".into()),
        None => Check::Fail {
            what: format!("no channel token at {where_it_lives}"),
            fix: format!(
                "mint a fine-grained PAT (Contents: read/write on {slug}, short expiry) \
                 and write it to {where_it_lives}, mode 600 — per-machine tokens revoke \
                 per-machine, same logic as the keys"
            ),
        },
    }
}

#[cfg(unix)]
fn utf8_path(p: &Path) -> Result<String> {
    p.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::new(format!("{} is not valid UTF-8", p.display())))
}

/// Unix seconds, failing rather than guessing — the mint stamps `added_at` from this.
#[cfg(unix)]
fn now_unix() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| Error::new("the system clock is before the unix epoch"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use aterm_update_core::roster::{Machine, SUPPORTED_SCHEMA};
    use base64::Engine as _;

    /// A synthetic paper phrase, built at runtime so no phrase-shaped literal exists in
    /// this file (grep_guard B7 scans it).
    fn seed() -> atpkg_keys::master::MasterSeed {
        let phrase = "2".repeat(52);
        atpkg_keys::master::parse_master(&phrase)
            .expect("synthetic phrase")
            .seed()
    }

    fn pubkey_of(byte: u8) -> String {
        base64::engine::general_purpose::STANDARD.encode([byte; 32])
    }

    fn roster_with(seq: u64, machines: Vec<Machine>, revoked: Vec<String>) -> Roster {
        Roster {
            schema: SUPPORTED_SCHEMA,
            roster_seq: seq,
            valid_until: "9999-12-31T00:00:00Z".into(),
            machines,
            revoked,
        }
    }

    fn machine(id: &str, key_byte: u8) -> Machine {
        Machine {
            id: id.into(),
            pubkey: pubkey_of(key_byte),
            added_at: "2026-01-01T00:00:00Z".into(),
            not_after: None,
        }
    }

    fn signed_candidate(seq: u64) -> (String, Vec<u8>, Vec<u8>) {
        let s = seed();
        let r = roster_with(seq, vec![machine("m3", 0x42)], vec![]);
        let bytes = r.to_toml().expect("valid roster").into_bytes();
        let sig = s.sign(&bytes).expect("sign");
        (s.pubkey_b64().expect("pubkey"), bytes, sig)
    }

    #[test]
    fn admission_requires_the_master_signature() {
        let (master, bytes, sig) = signed_candidate(7);
        let ok = admit_candidate(&[&master], bytes.clone(), sig.clone()).expect("admits");
        assert_eq!(ok.roster.roster_seq, 7);

        let mut tampered = bytes.clone();
        tampered[0] ^= 1;
        assert!(admit_candidate(&[&master], tampered, sig.clone()).is_err());

        let other = pubkey_of(0x99);
        assert!(admit_candidate(&[&other], bytes, sig).is_err());
    }

    #[test]
    fn the_newest_generation_wins_and_never_downgrades() {
        let (master, b, s) = signed_candidate(3);
        let older = admit_candidate(&[&master], b, s).unwrap();
        let (_, b, s) = signed_candidate(5);
        let newer = admit_candidate(&[&master], b, s).unwrap();
        let path = Path::new("dist/aterm-machines.toml");

        // channel newer → install
        let (_, b, s) = signed_candidate(3);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (chosen, install, how) = choose_candidate(path, "o/r", Some(local), Ok(newer)).unwrap();
        assert_eq!(chosen.roster.roster_seq, 5);
        assert!(install);
        assert!(how.contains("3 → 5"), "{how}");

        // local ahead → keep, never downgrade
        let (_, b, s) = signed_candidate(9);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (chosen, install, how) =
            choose_candidate(path, "o/r", Some(local), Ok(older)).unwrap();
        assert_eq!(chosen.roster.roster_seq, 9);
        assert!(!install);
        assert!(how.contains("AHEAD"), "{how}");

        // equal → keep local
        let (_, b, s) = signed_candidate(4);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (_, b, s) = signed_candidate(4);
        let fetched = admit_candidate(&[&master], b, s).unwrap();
        let (_, install, how) =
            choose_candidate(path, "o/r", Some(local), Ok(fetched)).unwrap();
        assert!(!install);
        assert!(how.contains("matches"), "{how}");

        // nothing local, fetch ok → install
        let (_, b, s) = signed_candidate(2);
        let fetched = admit_candidate(&[&master], b, s).unwrap();
        let (_, install, _) = choose_candidate(path, "o/r", None, Ok(fetched)).unwrap();
        assert!(install);

        // fetch failed, local present → keep with the failure named
        let (_, b, s) = signed_candidate(2);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (_, install, how) =
            choose_candidate(path, "o/r", Some(local), Err("offline".into())).unwrap();
        assert!(!install);
        assert!(how.contains("offline"), "{how}");

        // nothing anywhere → the error names the manual remedy and the URL
        let err = choose_candidate(path, "owner/repo", None, Err("offline".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("aterm-machines.toml.sig"), "{err}");
        assert!(
            err.contains("github.com/owner/repo/releases/latest/download"),
            "{err}"
        );
    }

    #[test]
    fn membership_is_id_and_key_and_revocation_outranks_a_listing() {
        let r = roster_with(
            4,
            vec![machine("m3", 0x42), machine("dead", 0x01)],
            vec!["dead".into()],
        );
        assert!(matches!(membership(&r, "m3", &pubkey_of(0x42)), Membership::Listed));
        assert!(matches!(
            membership(&r, "m3", &pubkey_of(0x43)),
            Membership::WrongKey { .. }
        ));
        assert!(matches!(membership(&r, "dead", &pubkey_of(0x01)), Membership::Revoked));
        assert!(matches!(membership(&r, "m9", &pubkey_of(0x42)), Membership::Absent));
    }

    #[test]
    fn devid_parsing_filters_by_kind_and_team() {
        let listing = concat!(
            "  1) 1111111111111111111111111111111111111111 \"Developer ID Application: A (TEAMIDXXXX)\"\n",
            "  2) 2222222222222222222222222222222222222222 \"Apple Development: A (TEAMIDXXXX)\"\n",
            "  3) 3333333333333333333333333333333333333333 \"Developer ID Application: A (OTHERTEAMX)\"\n",
            "  4) 4444444444444444444444444444444444444444 \"Developer ID Application: B (TEAMIDXXXX)\"\n",
            "     4 valid identities found\n",
        );
        assert_eq!(
            devid_identities(listing, "TEAMIDXXXX"),
            vec![
                "1111111111111111111111111111111111111111".to_string(),
                "4444444444444444444444444444444444444444".to_string(),
            ]
        );
        assert!(devid_identities("", "TEAMIDXXXX").is_empty());
    }

    #[test]
    fn the_asset_url_is_the_anonymous_latest_release_path() {
        assert_eq!(
            release_asset_url("alabsystems/aterm", "aterm-machines.toml"),
            "https://github.com/alabsystems/aterm/releases/latest/download/aterm-machines.toml"
        );
    }

    #[test]
    fn install_pair_writes_the_pair_and_a_rerun_admits_it() {
        let dir = std::env::temp_dir().join(format!("provision-pair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("aterm-machines.toml");
        let (master, bytes, sig) = signed_candidate(6);
        let c = admit_candidate(&[&master], bytes, sig).unwrap();
        install_pair(&path, &c).expect("install");
        let doc = machines::RosterDocument::read(&path).expect("pair on disk");
        let again = admit_candidate(&[&master], doc.bytes, doc.signature).expect("re-admits");
        assert_eq!(again.roster.roster_seq, 6);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
