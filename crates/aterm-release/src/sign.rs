// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signing (release spec §6 `sign.rs`, absorbing notarize.sh): inside-out
//! codesign. Default is ad-hoc (`--sign -`) — the shipped tier, unchanged. If
//! a Developer-ID identity is supplied, the dormant Dev-ID hook runs
//! nested-first with hardened runtime + entitlements, signs the DMG,
//! and drives `notarytool submit --wait` + staple + spctl — keeping the old
//! refusal preflights (reject ad-hoc identity, require hardened runtime). The
//! hook adds zero steps to the default path.
//!
//! Ports three shell sources at once:
//!   * tools/release-conf.sh — the credentials file + its ownership/mode
//!     refusal. The file is now PARSED, never sourced, so a hostile line is
//!     inert text instead of arbitrary code — but the stat refusal is kept
//!     anyway: the file steers what gets signed with which identity.
//!   * apps/aterm-mac/build-app.sh step 7 — inside-out codesign (nested
//!     atpkg/aterm-ctl/aterm-cli BEFORE the outer bundle seals them) + make-dmg.sh's
//!     DMG signature.
//!   * apps/aterm-mac/notarize.sh — auth assembly, the ad-hoc/Dev-ID/hardened-
//!     runtime preflights (pure, fixture-tested in tests/signconf.rs), submit
//!     --wait, staple, validate, spctl assessment.

use base64::Engine as _;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Release credentials — ONE profile file, named by ONE explicit flag
// ---------------------------------------------------------------------------

/// The owner's signing material, loaded from the path given to
/// `cargo ship cut --release-credentials <path>`.
///
/// This replaces the per-machine `~/.aterm/release.conf`. That file was AMBIENT:
/// present or absent, invisible either way, and discovered only at the moment of
/// failure — a full cut could clear every gate and then die because a file nobody
/// mentioned was missing. A flag is present or absent in the command you ran, so
/// "what signed this?" is answered by reading it.
///
/// The profile carries ONLY the Ed25519 signing key. It deliberately does not carry
/// Apple credentials: `pins::APPLE_TEAM_ID` is empty, aterm ships ad-hoc signed, and
/// there is no configured Developer-ID path to preserve. Turning Tier APPLE on is a
/// reviewed change that adds the anchor AND the credentials together.
///
/// ```toml
/// signing_key = "<base64 PKCS#8 Ed25519 private key>"
/// ```
///
/// Base64 is required, not incidental: `atpkg-keys keygen` writes BINARY PKCS#8, so a
/// raw paste into TOML cannot round-trip. The loader says so rather than failing with
/// a parse error nobody can act on.
#[derive(Clone)]
pub struct ReleaseCredentials {
    /// Raw PKCS#8 bytes. Never logged, never journaled, never serialized.
    pkcs8: Vec<u8>,
    /// The derived public identity — the only part that is ever recorded.
    pubkey_b64: String,
}

impl std::fmt::Debug for ReleaseCredentials {
    /// Hand-written so the private key can never reach a log through a derive.
    /// Only the public identity is printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReleaseCredentials")
            .field("pubkey", &self.pubkey_b64)
            .field("pkcs8", &"<redacted>")
            .finish()
    }
}

impl ReleaseCredentials {
    /// Load and validate the profile at `path`.
    ///
    /// Enforces the ownership rule `release.conf` had — owner-only, no group/other
    /// write — because the file still holds a private key, and derives the public
    /// identity IN-PROCESS. The old path shelled out to `atpkg-keys pubkey`, which
    /// meant a release could not be cut without a second binary built and on disk.
    pub fn load(path: &Path) -> Result<Self, String> {
        let meta = std::fs::metadata(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            check_credentials_perms(meta.uid(), meta.mode(), current_uid()?, path)?;
        }
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let encoded = credentials_signing_key(&text)?;
        let pkcs8 = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| {
                format!(
                    "{}: signing_key is not valid base64. `atpkg-keys keygen` writes BINARY \
                     PKCS#8 — base64-encode those bytes rather than pasting them",
                    path.display()
                )
            })?;
        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(&pkcs8)
            .map_err(|_| format!("{}: signing_key is not a PKCS#8 Ed25519 key", path.display()))?;
        let pubkey_b64 = {
            use ring::signature::KeyPair as _;
            base64::engine::general_purpose::STANDARD.encode(keypair.public_key().as_ref())
        };
        Ok(Self { pkcs8, pubkey_b64 })
    }

    /// The public identity of the loaded key — what preflight matches against the
    /// committed anchor, and the only value the journal records.
    #[must_use]
    pub fn pubkey(&self) -> &str {
        &self.pubkey_b64
    }

    /// Detached Ed25519 signature over `msg`.
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(&self.pkcs8)
            .map_err(|_| "signing key became unusable after load".to_string())?;
        Ok(keypair.sign(msg).as_ref().to_vec())
    }
}

/// Pull `signing_key` out of the profile without a TOML dependency: the file has one
/// key of record, and a hand-rolled reader keeps the parse surface as small as the
/// secret it is reading.
fn credentials_signing_key(text: &str) -> Result<&str, String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("not a `key = value` line: {line}"));
        };
        if key.trim() != "signing_key" {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .ok_or_else(|| "signing_key must be a quoted string".to_string())?;
        if value.is_empty() {
            return Err("signing_key is empty".to_string());
        }
        return Ok(value);
    }
    Err("no `signing_key` in the credentials profile".to_string())
}

/// The pure refusal rule (fixture-tested): refuse a credentials profile not owned
/// by the current uid, or writable by group/other. It holds a private key, so the
/// ownership guarantee `release.conf` carried is kept verbatim.
pub fn check_credentials_perms(
    owner_uid: u32,
    mode: u32,
    current_uid: u32,
    path: &Path,
) -> Result<(), String> {
    if owner_uid != current_uid {
        return Err(format!(
            "REFUSING {} — not owned by you (uid {owner_uid})",
            path.display()
        ));
    }
    // TIGHTENED from the old conf rule (`0o022`, group/other WRITE) to `0o077`, any
    // group/other access. release.conf held identities and paths; this file holds the
    // PRIVATE KEY, so a world-READABLE profile is a leak even though nobody else can
    // write it. Caught by its own test, which passed at 0644 under the old mask.
    if mode & 0o077 != 0 {
        return Err(format!(
            "REFUSING {} — group/other-accessible (mode {:03o}); it holds a private key, \
             chmod 600 it",
            path.display(),
            mode & 0o777
        ));
    }
    Ok(())
}

/// Current uid via `id -u` — the same probe release-conf.sh used; keeps the
/// crate free of a libc dependency for one syscall.
#[cfg(unix)]
fn current_uid() -> Result<u32, String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| format!("spawn id -u: {e}"))?;
    if !out.status.success() {
        return Err("id -u failed".into());
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|e| format!("id -u parse: {e}"))
}

// ---------------------------------------------------------------------------
// codesign — inside-out (build-app.sh step 7 + make-dmg.sh's DMG signature)
// ---------------------------------------------------------------------------

/// Sign the assembled bundle. Returns the `signed_by` provenance string for
/// `bundle::write_provenance` ("ad-hoc", or the Developer-ID identity).
///
/// Dev-ID path: the co-located atpkg / aterm-ctl / aterm-cli are FURTHER
/// Mach-Os in Contents/MacOS and must be signed BEFORE the outer bundle seals
/// them (inside-out). They need no extra entitlements (atpkg shells
/// curl/codesign/spctl like the updater does; aterm-ctl only dials the
/// per-user control socket; aterm-cli spawns the user's shell through the
/// same protected seam it uses standalone); the outer app gets the hardened
/// runtime + the minimal, no-exception entitlements perimeter — notarization
/// requires both.
///
/// Ad-hoc path (no conf / no ATERM_SIGN_ID): `--deep` signs the nested
/// binaries too. Runs locally; NOT distributable to other Macs.
pub fn sign_app(app: &Path, entitlements: &Path, sign_id: Option<&str>) -> Result<String, String> {
    let signed_by = match sign_id {
        Some(id) => {
            // ONE Mach-O: nothing nested to sign. The argv0 alias symlinks in
            // Contents/MacOS are resources, sealed by the outer signature.
            println!("==> codesign (Developer-ID, hardened runtime + entitlements): {id}");
            run_checked(
                Command::new("codesign")
                    .args([
                        "--force",
                        "--options",
                        "runtime",
                        "--timestamp",
                        "--entitlements",
                    ])
                    .arg(entitlements)
                    .args(["--sign", id])
                    .arg(app),
                "codesign app",
            )?;
            id.to_string()
        }
        None => {
            println!("==> codesign (ad-hoc): runs locally, not distributable to other Macs");
            run_checked(
                Command::new("codesign")
                    .args(["--force", "--deep", "--sign", "-"])
                    .arg(app),
                "codesign app (ad-hoc)",
            )?;
            "ad-hoc".to_string()
        }
    };
    // Post-sign verification print (best-effort, like the script's `|| true`);
    // the cut's hard `codesign --verify --deep --strict` gate lives in the
    // publish self-check (spec §7 step 4).
    if let Ok(out) = Command::new("codesign")
        .args(["--verify", "--verbose=2"])
        .arg(app)
        .output()
    {
        for line in String::from_utf8_lossy(&out.stderr).lines() {
            println!("    {line}");
        }
    }
    Ok(signed_by)
}

/// Sign the DMG (make-dmg.sh's owner path). `--timestamp` embeds a secure
/// timestamp so the signature stays valid after the signing cert expires (the
/// .app inside is already timestamped by [`sign_app`]).
pub fn sign_dmg(dmg: &Path, sign_id: &str) -> Result<(), String> {
    println!("==> codesign dmg (Developer-ID): {sign_id}");
    run_checked(
        Command::new("codesign")
            .args(["--force", "--timestamp", "--sign", sign_id])
            .arg(dmg),
        "codesign dmg",
    )
}

// ---------------------------------------------------------------------------
// notarization — the absorbed notarize.sh
// ---------------------------------------------------------------------------

/// notarytool credentials (notarize.sh's two spellings; keychain profile is
/// tried first). DORMANT: Tier APPLE is off (`pins::APPLE_TEAM_ID` is empty), so
/// nothing constructs this today. Turning the tier on adds the credentials to the
/// release-credentials profile in the same reviewed change that commits the anchor.
pub enum NotaryAuth {
    KeychainProfile(String),
    AppleId {
        apple_id: String,
        team_id: String,
        password: String,
    },
}

impl NotaryAuth {
    fn args(&self) -> Vec<String> {
        match self {
            NotaryAuth::KeychainProfile(p) => {
                vec!["--keychain-profile".into(), p.clone()]
            }
            NotaryAuth::AppleId {
                apple_id,
                team_id,
                password,
            } => vec![
                "--apple-id".into(),
                apple_id.clone(),
                "--team-id".into(),
                team_id.clone(),
                "--password".into(),
                password.clone(),
            ],
        }
    }
}

/// The notarize.sh refusal preflights, as a PURE function over `codesign -dv
/// --verbose=2` output (fixture-tested in tests/signconf.rs). Rejects, in the
/// script's order:
///   1. an ad-hoc signature ("Signature=adhoc") — cannot be notarized; loudly,
///      rather than wasting a round-trip to Apple;
///   2. anything without a Developer ID Application Authority line;
///   3. a .app signed WITHOUT the hardened runtime (no "(runtime)" in the
///      CodeDirectory flags) — notarization requires `--options runtime`, so
///      reject early instead of burning an Apple round-trip on a build that
///      will come back rejected.
pub fn devid_preflight(codesign_info: &str, is_app: bool) -> Result<(), String> {
    if codesign_info.contains("Signature=adhoc") {
        return Err(
            "artifact is ad-hoc signed — sign with a Developer-ID identity \
                    (ATERM_SIGN_ID) before notarizing"
                .into(),
        );
    }
    if !codesign_info.contains("Authority=Developer ID Application") {
        return Err(
            "artifact is not Developer-ID signed (no matching Authority) — \
                    sign with ATERM_SIGN_ID=\"Developer ID Application: … (TEAMID)\" first"
                .into(),
        );
    }
    if is_app && !codesign_info.contains("(runtime)") {
        return Err(
            "artifact is not signed with the hardened runtime — re-sign with \
                    --options runtime before notarizing"
                .into(),
        );
    }
    Ok(())
}

/// Run the preflight against a real on-disk artifact (`codesign -dv` prints
/// to stderr; unsigned targets only produce an error — both flow into the
/// pure check, which then rejects for the missing Authority).
fn check_devid_signed(target: &Path) -> Result<(), String> {
    let out = Command::new("codesign")
        .args(["-dv", "--verbose=2"])
        .arg(target)
        .output()
        .map_err(|e| format!("spawn codesign -dv: {e}"))?;
    let mut info = String::from_utf8_lossy(&out.stderr).to_string();
    info.push_str(&String::from_utf8_lossy(&out.stdout));
    let is_app = target.extension().is_some_and(|e| e == "app");
    devid_preflight(&info, is_app).map_err(|e| format!("{}: {e}", target.display()))
}

/// Notarize + staple a .dmg (preferred) or .app: preflight → (zip a bare
/// .app — notarytool needs a zip/dmg/pkg) → `notarytool submit --wait` →
/// `stapler staple` + `validate` → spctl assessment print. The staple runs
/// against the ORIGINAL artifact, never the throwaway zip.
pub fn notarize(artifact: &Path, auth: &NotaryAuth) -> Result<(), String> {
    check_devid_signed(artifact)?;

    let ext = artifact.extension().and_then(|e| e.to_str()).unwrap_or("");
    let (submit, cleanup): (PathBuf, Option<PathBuf>) = match ext {
        "dmg" => (artifact.to_path_buf(), None),
        "app" => {
            let zip = artifact.with_extension("notarize.zip");
            println!(
                "==> zipping {} -> {} (notarytool needs a zip/dmg)",
                artifact.display(),
                zip.display()
            );
            run_checked(
                Command::new("/usr/bin/ditto")
                    .args(["-c", "-k", "--keepParent"])
                    .arg(artifact)
                    .arg(&zip),
                "ditto notarize zip",
            )?;
            (zip.clone(), Some(zip))
        }
        _ => {
            return Err(format!(
                "expected a .dmg or .app, got {}",
                artifact.display()
            ));
        }
    };

    println!("==> xcrun notarytool submit {} --wait", submit.display());
    let result = run_streamed(
        Command::new("xcrun")
            .args(["notarytool", "submit"])
            .arg(&submit)
            .args(auth.args())
            .arg("--wait"),
        "notarytool submit",
    );
    // The zip is a throwaway either way — remove it before propagating.
    if let Some(zip) = cleanup {
        let _ = std::fs::remove_file(zip);
    }
    result?;

    println!("==> xcrun stapler staple {}", artifact.display());
    run_streamed(
        Command::new("xcrun")
            .args(["stapler", "staple"])
            .arg(artifact),
        "stapler staple",
    )?;
    run_streamed(
        Command::new("xcrun")
            .args(["stapler", "validate"])
            .arg(artifact),
        "stapler validate",
    )?;

    // Gatekeeper's own verdict, printed for the record (best-effort — the
    // script `|| true`s this too; notarization already succeeded above).
    println!("==> spctl assessment");
    let mut spctl = Command::new("spctl");
    if ext == "dmg" {
        spctl.args([
            "-a",
            "-vvv",
            "-t",
            "open",
            "--context",
            "context:primary-signature",
        ]);
    } else {
        spctl.args(["-a", "-vvv", "-t", "install"]);
    }
    if let Ok(out) = spctl.arg(artifact).output() {
        for line in String::from_utf8_lossy(&out.stderr)
            .lines()
            .chain(String::from_utf8_lossy(&out.stdout).lines())
        {
            println!("    {line}");
        }
    }
    println!("==> notarized + stapled: {}", artifact.display());
    Ok(())
}

/// Sign + notarize the DMG when the conf enables the owner path; a no-op
/// (returning false) otherwise. The one call chunk C's pipeline makes after
/// `dmg::create` — keeps the Dev-ID hook to zero steps on the default path.
pub fn sign_and_notarize_dmg(dmg: &Path, sign_id: Option<&str>) -> Result<bool, String> {
    // Tier APPLE is OFF: `pins::APPLE_TEAM_ID` is empty and aterm ships ad-hoc, so
    // callers pass `None` and this is a no-op. The Developer-ID and notarization
    // machinery below the fold (`sign_dmg`, `notarize`) is kept intact and unused —
    // turning the tier on is a reviewed change that commits the anchor AND supplies
    // the credentials, not a matter of a file happening to exist on some machine.
    let Some(id) = sign_id else { return Ok(false) };
    sign_dmg(dmg, id)?;
    println!(
        "    NOTE: Developer-ID signed, not notarized — Tier APPLE credentials are \
         not part of the release-credentials profile yet"
    );
    Ok(false)
}

// ---------------------------------------------------------------------------

fn run_checked(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("spawn {what}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Streamed variant for the long-running / progress-printing tools
/// (notarytool submit --wait can take minutes; its status lines matter).
fn run_streamed(cmd: &mut Command, what: &str) -> Result<(), String> {
    let status = cmd
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("spawn {what}: {e}"))?;
    if !status.success() {
        return Err(format!("{what} failed ({status})"));
    }
    Ok(())
}
