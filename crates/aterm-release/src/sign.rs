// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signing (release spec §6 `sign.rs`, absorbing notarize.sh): inside-out
//! codesign. Default is ad-hoc (`--sign -`) — the shipped tier, unchanged. If
//! `~/.aterm/release.conf` exists (KEY=value parsed in-process, NEVER
//! shell-sourced; refused unless owner-only-writable + owned — the ported
//! release-conf.sh stat checks) and sets `ATERM_SIGN_ID`, the dormant Dev-ID
//! hook runs nested-first with hardened runtime + entitlements, signs the DMG,
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

use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// ~/.aterm/release.conf — in-process KEY=value parsing (spec decision 10)
// ---------------------------------------------------------------------------

/// The owner's per-machine release credentials, parsed from
/// `~/.aterm/release.conf`. Keys of record (docs/RELEASING.md):
/// ATERM_UPDATE_PUBKEY, ATERM_UPDATE_SIGN_KEY, ATERM_PKG_ROOTKEY,
/// ATERM_SIGN_ID, ATERM_TEAM_ID / ATERM_EXPECTED_TEAM_ID,
/// ATERM_NOTARY_PROFILE, ATERM_APPLE_ID, ATERM_APP_PASSWORD. Unknown keys are
/// carried (they may feed future tiers) but only ever exported by name — the
/// file can no longer execute anything.
#[derive(Debug)]
pub struct ReleaseConf {
    /// Ordered (later assignment to the same key wins, like the shell source).
    vars: Vec<(String, String)>,
}

impl ReleaseConf {
    /// Last assignment wins — matches sourcing the file into a shell.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The Developer-ID signing identity, when the owner configured one.
    pub fn sign_id(&self) -> Option<&str> {
        self.get("ATERM_SIGN_ID").filter(|s| !s.is_empty())
    }

    /// The compile-time pins the conf must deliver into child cargo env
    /// (spec §6): updater Tier SIG pubkey, atpkg trust-root pubkey, and the
    /// Apple Team ID pin — read by `option_env!` in aterm-update / atpkg, so
    /// they must be present in the BUILD's environment, not just at signing.
    pub fn env_pins(&self) -> Vec<(String, String)> {
        [
            "ATERM_PKG_ROOTKEY",
            "ATERM_UPDATE_PUBKEY",
            "ATERM_EXPECTED_TEAM_ID",
        ]
        .iter()
        // EMPTY assignments are dropped, not exported: a stale `KEY=` line
        // would otherwise pin the empty string into the cargo child env and
        // OVERRIDE a non-empty value the operator has exported — baking an
        // inert client while the cut's own gates pass under the env key
        // (adversarial review 2026-07-30). An absent pin and an empty pin mean
        // the same thing (inert); only a real value is worth exporting.
        .filter_map(|k| {
            self.get(k)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| (k.to_string(), v.to_string()))
        })
        .collect()
    }

    /// notarytool credentials, keychain profile tried first (notarize.sh's
    /// precedence). None ⇒ signing may still happen, notarization is skipped
    /// by the caller with a clear message.
    pub fn notary_auth(&self) -> Option<NotaryAuth> {
        if let Some(profile) = self.get("ATERM_NOTARY_PROFILE").filter(|s| !s.is_empty()) {
            return Some(NotaryAuth::KeychainProfile(profile.to_string()));
        }
        match (
            self.get("ATERM_APPLE_ID").filter(|s| !s.is_empty()),
            self.get("ATERM_TEAM_ID").filter(|s| !s.is_empty()),
            self.get("ATERM_APP_PASSWORD").filter(|s| !s.is_empty()),
        ) {
            (Some(id), Some(team), Some(pw)) => Some(NotaryAuth::AppleId {
                apple_id: id.to_string(),
                team_id: team.to_string(),
                password: pw.to_string(),
            }),
            _ => None,
        }
    }
}

/// Load `~/.aterm/release.conf`. `Ok(None)` when absent — NOT an error:
/// builds without credentials keep the defaults (ad-hoc sign, Tier REPO
/// updater, inert atpkg) — strictly fail-closed, like release-conf.sh.
pub fn load_default() -> Result<Option<ReleaseConf>, String> {
    #[cfg(unix)]
    {
        let home = std::env::var_os("HOME").ok_or("HOME not set")?;
        load_conf(&PathBuf::from(home).join(".aterm/release.conf"))
    }
    // Windows: `~/.aterm/release.conf` is a Unix location and the refusal
    // rules below are uid/mode-based, so credentials never load here —
    // builds keep the fail-closed defaults (ad-hoc sign, Tier REPO, inert
    // atpkg), exactly like a Unix host with no conf present.
    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

/// Load a conf file with the ported release-conf.sh refusals: must be owned
/// by the current user and not group/other-writable (`chmod 600` is the
/// documented remediation). We no longer source it, but it still selects the
/// signing identity and the compile-time trust pins — a file someone else can
/// edit must never steer a release.
pub fn load_conf(path: &Path) -> Result<Option<ReleaseConf>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("stat {}: {e}", path.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        check_conf_perms(meta.uid(), meta.mode(), current_uid()?, path)?;
    }
    // Windows has no uid/mode semantics to enforce the refusal rules with, so
    // an existing conf is REFUSED outright rather than loaded unverified —
    // same fail-closed posture, stricter arm.
    #[cfg(not(unix))]
    {
        let _ = meta;
        return Err(format!(
            "REFUSING {} — the ownership/mode refusals guarding release \
             credentials are Unix-only; run release signing from macOS",
            path.display()
        ));
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let vars = parse_conf(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("==> release credentials loaded from {}", path.display());
    Ok(Some(ReleaseConf { vars }))
}

/// The pure refusal rule (fixture-tested): refuse a conf not owned by the
/// current uid, or writable by group/other — release-conf.sh's stat checks
/// (`%u` vs `id -u`; mode regex `[2367]$|[2367][0-7]$` ⇒ any g/o write bit).
pub fn check_conf_perms(
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
    if mode & 0o022 != 0 {
        return Err(format!(
            "REFUSING {} — group/other-writable (mode {:03o}); chmod 600 it",
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

/// Parse the KEY=value body IN-PROCESS. Accepted per line: blank, `#` comment,
/// `KEY=value` (optionally `export KEY=value`, the common env-file spelling),
/// with the value optionally wrapped in matching single or double quotes.
/// REFUSED with the line number: non-assignment lines, invalid key names, and
/// values still containing `$`/backtick after quote-stripping — the file is
/// data now, and a value written expecting shell expansion would otherwise be
/// exported verbatim and silently diverge from the sourcing era.
pub fn parse_conf(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut vars = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let lineno = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export ")
            .map(str::trim_start)
            .unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "line {lineno}: not a KEY=value assignment: {raw:?}"
            ));
        };
        let key = key.trim_end();
        let valid_key = !key.is_empty()
            && !key.starts_with(|c: char| c.is_ascii_digit())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid_key {
            return Err(format!("line {lineno}: invalid key name {key:?}"));
        }
        let value = strip_quotes(value.trim());
        if value.contains('$') || value.contains('`') {
            return Err(format!(
                "line {lineno}: value of {key} contains shell expansion ($ or `) — \
                 release.conf is parsed, not sourced; use a literal value"
            ));
        }
        vars.push((key.to_string(), value.to_string()));
    }
    Ok(vars)
}

/// Strip ONE pair of matching surrounding quotes (the sourcing-era spelling
/// `ATERM_SIGN_ID="Developer ID Application: …"`). No escape processing —
/// values are identities, key material and paths, all literal.
fn strip_quotes(v: &str) -> &str {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
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
/// tried first — see [`ReleaseConf::notary_auth`]).
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
pub fn sign_and_notarize_dmg(dmg: &Path, conf: Option<&ReleaseConf>) -> Result<bool, String> {
    let Some(conf) = conf else { return Ok(false) };
    let Some(id) = conf.sign_id() else {
        return Ok(false);
    };
    sign_dmg(dmg, id)?;
    match conf.notary_auth() {
        Some(auth) => {
            notarize(dmg, &auth)?;
            Ok(true)
        }
        None => {
            // Signed but not notarized: allowed (the owner may staple later),
            // but say so — notarize.sh's "never no-ops silently" contract.
            println!(
                "    NOTE: no notary credentials in release.conf \
                 (ATERM_NOTARY_PROFILE or ATERM_APPLE_ID/ATERM_TEAM_ID/ATERM_APP_PASSWORD) — \
                 DMG signed but NOT notarized."
            );
            Ok(false)
        }
    }
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
