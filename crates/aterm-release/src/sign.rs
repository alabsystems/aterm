// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release code signing, notarization, and Apple tier hooks ([`AppleTier::Inactive`],
//! [`AppleTier::Active`], [`select_devid_identity`], [`ReleaseCredentials`],
//! [`check_credentials_perms`]).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const CODESIGN: &str = "/usr/bin/codesign";
const SPCTL: &str = "/usr/sbin/spctl";
const XCRUN: &str = "/usr/bin/xcrun";
const SECURITY: &str = "/usr/bin/security";
const DITTO: &str = "/usr/bin/ditto";
const ID_TOOL: &str = "/usr/bin/id";

/// How long `notarytool submit --wait` may run before the cut gives up.
pub const NOTARY_SUBMIT_TIMEOUT: Duration = Duration::from_secs(40 * 60);

/// Bound for `stapler staple` / `stapler validate`: a ticket fetch and a local check.
const STAPLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The owner's signing material.
///
/// ```toml
/// signing_key = "<base64 PKCS#8 Ed25519 private key>"
/// # Tier APPLE only (ignored while pins::APPLE_TEAM_ID is empty):
/// notary_profile = "<name given to `xcrun notarytool store-credentials`>"
/// # ...or, for a machine with no usable keychain, the headless fallback:
/// # notary_apple_id = "<Apple ID>"
/// # notary_password = "<app-specific password>"
/// # Machine-roster tier only (ignored while pins::PAPER_MASTER_PUBKEYS is empty):
/// machine_roster = "<path to the master-signed aterm-machines.toml; a leading ~/ is HOME>"
/// machine_id     = "<this machine's id, e.g. the one `join --id` was given>"
/// ```
#[derive(Clone)]
pub struct ReleaseCredentials {
    /// Raw PKCS#8 bytes.
    pkcs8: Vec<u8>,
    /// The derived public identity.
    pubkey_b64: String,
    /// This machine's public id as the profile declares it.
    machine_id: Option<String>,
    /// Path to the master-signed machine roster.
    machine_roster: Option<PathBuf>,
    /// Notarytool auth, if the profile names one.
    notary: Option<NotaryAuth>,
    /// Optional keychain disambiguator (`signing_identity_sha1`).
    identity_sha1: Option<String>,
    /// The profile these credentials were loaded from.
    profile: Option<PathBuf>,
}

impl std::fmt::Debug for ReleaseCredentials {
    /// Hand-written so the private key never reaches a log through a derive.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReleaseCredentials")
            .field("pubkey", &self.pubkey_b64)
            .field("machine_id", &self.machine_id)
            .field("pkcs8", &"<redacted>")
            .field("notary", &self.notary)
            .finish()
    }
}

impl ReleaseCredentials {
    /// Resolve signing material.
    pub fn resolve(explicit: Option<&Path>, repo: &Path) -> Result<Option<Self>, String> {
        Self::resolve_with(explicit, repo, std::env::var("HOME").ok().as_deref())
    }

    /// [`Self::resolve`] with the home directory injected.
    pub(crate) fn resolve_with(
        explicit: Option<&Path>,
        repo: &Path,
        home: Option<&str>,
    ) -> Result<Option<Self>, String> {
        if let Some(path) = explicit {
            return Self::load(path, home.map(Path::new)).map(Some);
        }
        let Some(home) = home else {
            return Ok(None);
        };
        let key_path = Path::new(&home).join(".aterm/machine.key");
        if !key_path.exists() {
            return Ok(None);
        }
        let meta = std::fs::metadata(&key_path)
            .map_err(|error| format!("read {}: {error}", key_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            check_credentials_perms(meta.uid(), meta.mode(), current_uid()?, &key_path)?;
        }
        let pkcs8 = std::fs::read(&key_path)
            .map_err(|error| format!("read {}: {error}", key_path.display()))?;
        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|_| {
            format!(
                "{}: not a PKCS#8 Ed25519 key — re-mint with `atpkg-keys join --id <id>`",
                key_path.display()
            )
        })?;
        let pubkey_b64 = {
            use ring::signature::KeyPair as _;
            aterm_codec::base64::encode(keypair.public_key().as_ref())
                .map_err(|_| "public key too large to encode".to_string())?
        };
        let staged = repo.join("dist/aterm-machines.toml");
        Ok(Some(Self {
            pkcs8,
            pubkey_b64,
            machine_id: None,
            machine_roster: staged.exists().then_some(staged),
            notary: None,
            identity_sha1: None,
            profile: None,
        }))
    }

    /// Load and validate the profile at `path`, reading a `~/` `machine_roster` under `home`
    /// ([`roster_path`]).
    pub fn load(path: &Path, home: Option<&Path>) -> Result<Self, String> {
        let meta =
            std::fs::metadata(path).map_err(|error| format!("read {}: {error}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            check_credentials_perms(meta.uid(), meta.mode(), current_uid()?, path)?;
        }
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let encoded = credentials_signing_key(&text)?;
        let pkcs8 =
            aterm_codec::base64::decode_strict(encoded.trim().as_bytes()).map_err(|_| {
                format!(
                    "{}: signing_key is not valid base64. Minted machine keys are BINARY \
                     PKCS#8 — base64-encode those bytes rather than pasting them",
                    path.display()
                )
            })?;
        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|_| {
            format!(
                "{}: signing_key is not a PKCS#8 Ed25519 key",
                path.display()
            )
        })?;
        let pubkey_b64 = {
            use ring::signature::KeyPair as _;
            aterm_codec::base64::encode(keypair.public_key().as_ref())
                .map_err(|_| "public key too large to encode".to_string())?
        };
        let notary =
            credentials_notary_auth(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let identity_sha1 = credentials_value(&text, "signing_identity_sha1")
            .map_err(|e| format!("{}: {e}", path.display()))?
            .map(str::to_string);
        let machine_id = credentials_value(&text, "machine_id")
            .map_err(|e| format!("{}: {e}", path.display()))?
            .map(str::to_string);
        let machine_roster = credentials_value(&text, "machine_roster")
            .map_err(|e| format!("{}: {e}", path.display()))?
            .map(|named| roster_path(named, home))
            .transpose()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            pkcs8,
            pubkey_b64,
            machine_id,
            machine_roster,
            notary,
            identity_sha1,
            profile: Some(path.to_path_buf()),
        })
    }

    /// The notarytool auth the profile names.
    #[must_use]
    pub const fn notary(&self) -> Option<&NotaryAuth> {
        self.notary.as_ref()
    }

    /// The keychain SHA-1 the operator pinned, if the profile names one.
    #[must_use]
    pub fn signing_identity_sha1(&self) -> Option<&str> {
        self.identity_sha1.as_deref()
    }

    /// The loaded key's base64 public key.
    #[must_use]
    pub fn pubkey(&self) -> &str {
        &self.pubkey_b64
    }

    /// This machine's public id as the profile declares it, if present.
    #[must_use]
    pub fn machine_id(&self) -> Option<&str> {
        self.machine_id.as_deref()
    }

    /// The master-signed roster this cut must be authorized by, if the profile names one.
    #[must_use]
    pub fn machine_roster(&self) -> Option<&Path> {
        self.machine_roster.as_deref()
    }

    /// The `--release-credentials` profile these were loaded from.
    #[must_use]
    pub fn profile(&self) -> Option<&Path> {
        self.profile.as_deref()
    }

    /// Detached Ed25519 signature over `msg`.
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(&self.pkcs8)
            .map_err(|_| "signing key became unusable after load".to_string())?;
        Ok(keypair.sign(msg).as_ref().to_vec())
    }
}

/// A profile's `machine_roster`, a leading `~/` read under `home`: the secrets vault hands
/// every Mac one profile, which names the roster where the vault puts it
/// (`~/.aterm/roster/aterm-machines.toml`) whoever's home that is. A home that is missing or
/// not absolute refuses, so the roster is never looked for under the working directory. Any
/// other value is the path as written.
fn roster_path(named: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    match named.strip_prefix("~/") {
        Some(rest) => home
            .filter(|home| home.is_absolute())
            .map(|home| home.join(rest.trim_start_matches('/')))
            .ok_or_else(|| {
                format!("machine_roster {named:?} is under ~/, but HOME is unset or not absolute")
            }),
        None => Ok(PathBuf::from(named)),
    }
}

/// Pull one quoted value out of the profile without a TOML dependency.
pub(crate) fn credentials_value<'a>(text: &'a str, want: &str) -> Result<Option<&'a str>, String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("not a `key = value` line: {line}"));
        };
        if key.trim() != want {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .ok_or_else(|| format!("{want} must be a quoted string"))?;
        if value.is_empty() {
            return Err(format!("{want} is empty"));
        }
        return Ok(Some(value));
    }
    Ok(None)
}

/// The one key of record. Absent is an error here (unlike the optional Apple keys):
/// a profile was named on the command line precisely to supply it.
fn credentials_signing_key(text: &str) -> Result<&str, String> {
    credentials_value(text, "signing_key")?
        .ok_or_else(|| "no `signing_key` in the credentials profile".to_string())
}

/// Assemble [`NotaryAuth`] from the profile's optional Apple keys.
pub fn credentials_notary_auth(text: &str) -> Result<Option<NotaryAuth>, String> {
    let profile = credentials_value(text, "notary_profile")?;
    let apple_id = credentials_value(text, "notary_apple_id")?;
    let password = credentials_value(text, "notary_password")?;
    match (profile, apple_id, password) {
        (None, None, None) => Ok(None),
        (Some(name), None, None) => Ok(Some(NotaryAuth::KeychainProfile(name.to_string()))),
        (None, Some(apple_id), Some(password)) => Ok(Some(NotaryAuth::AppleId {
            apple_id: apple_id.to_string(),
            team_id: String::new(),
            password: password.to_string(),
        })),
        (Some(_), _, _) => Err(
            "names both notary_profile and notary_apple_id/notary_password — pick one, \
             so the file answers which credential authenticated the submission"
                .to_string(),
        ),
        (None, Some(_), None) => Err(
            "names notary_apple_id without notary_password — the headless fallback needs both"
                .to_string(),
        ),
        (None, None, Some(_)) => Err(
            "names notary_password without notary_apple_id — the headless fallback needs both"
                .to_string(),
        ),
    }
}

/// The pure refusal rule (fixture-tested).
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

/// Current uid via `id -u`; keeps the crate free of a libc dependency for one syscall.
#[cfg(unix)]
fn current_uid() -> Result<u32, String> {
    let out = Command::new(ID_TOOL)
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

/// The Developer-ID certificate this cut signs with, resolved from the anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevIdIdentity {
    /// Uppercase 40-hex SHA-1 of the certificate, as `security` prints it.
    pub sha1: String,
    /// `Developer ID Application: <name> (<TEAMID>)`.
    pub common_name: String,
}

impl DevIdIdentity {
    /// The provenance string recorded in the sidecar.
    #[must_use]
    pub fn provenance(&self) -> String {
        format!("{} [{}]", self.common_name, self.sha1)
    }
}

/// The prefix of every Developer-ID code-signing certificate's common name. Matching on it,
/// not on the Team ID alone, keeps out a same-team "Developer ID Installer" certificate,
/// which would sign a bundle but cannot notarize one.
const DEVID_APP_PREFIX: &str = "Developer ID Application: ";

/// Choose the signing certificate from `security find-identity -v -p codesigning` output.
pub fn select_devid_identity(
    find_identity_output: &str,
    team_id: &str,
    preferred_sha1: Option<&str>,
) -> Result<DevIdIdentity, String> {
    if team_id.is_empty() {
        return Err("cannot select a Developer-ID identity with an empty APPLE_TEAM_ID".into());
    }
    let suffix = format!("({team_id})");
    let candidates: Vec<DevIdIdentity> = find_identity_output
        .lines()
        .filter_map(parse_find_identity_line)
        .filter(|id| {
            id.common_name.starts_with(DEVID_APP_PREFIX) && id.common_name.ends_with(&suffix)
        })
        .collect();
    if candidates.is_empty() {
        return Err(format!(
            "no valid \"Developer ID Application: … ({team_id})\" certificate in the login \
             keychain. `security find-identity -v -p codesigning` must list one; install the \
             Developer-ID Application certificate and its private key for team {team_id}, or \
             leave pins::APPLE_TEAM_ID empty to keep Tier APPLE off"
        ));
    }
    let chosen: Vec<&DevIdIdentity> = match preferred_sha1 {
        Some(want) => candidates
            .iter()
            .filter(|id| id.sha1.eq_ignore_ascii_case(want.trim()))
            .collect(),
        None => candidates.iter().collect(),
    };
    match chosen.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!(
            "signing_identity_sha1 = \"{}\" matches no team-{team_id} Developer-ID certificate; \
             candidates are: {}",
            preferred_sha1.unwrap_or_default().trim(),
            describe_candidates(&candidates)
        )),
        _ => Err(format!(
            "{} valid Developer-ID Application certificates match team {team_id}, so the \
             pipeline cannot pick one: {}. Remove the superseded certificate from the login \
             keychain, or add `signing_identity_sha1 = \"<sha1>\"` to the release-credentials \
             profile to name the one to use",
            chosen.len(),
            describe_candidates(&candidates)
        )),
    }
}

fn describe_candidates(candidates: &[DevIdIdentity]) -> String {
    candidates
        .iter()
        .map(DevIdIdentity::provenance)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Parse one `  1) <40-hex> "<common name>"` listing line. Anything else —
/// the trailing "N valid identities found" summary, blank lines, a policy
/// notice — yields `None` and is skipped.
fn parse_find_identity_line(line: &str) -> Option<DevIdIdentity> {
    let (_, rest) = line.trim().split_once(") ")?;
    let (sha1, rest) = rest.split_once(' ')?;
    if !aterm_codec::hex::is_hex(sha1, 40) {
        return None;
    }
    let common_name = rest.trim().strip_prefix('"')?.strip_suffix('"')?;
    Some(DevIdIdentity {
        sha1: sha1.to_ascii_uppercase(),
        common_name: common_name.to_string(),
    })
}

/// Tier APPLE, resolved once per cut, so a cut cannot change identity halfway.
#[derive(Debug)]
pub enum AppleTier {
    /// `pins::APPLE_TEAM_ID` is empty.
    Inactive,
    /// The anchor is set and everything needed to keep its promise resolved.
    Active {
        identity: DevIdIdentity,
        auth: NotaryAuth,
    },
}

impl AppleTier {
    /// The certificate this cut signs with.
    #[must_use]
    pub const fn identity(&self) -> Option<&DevIdIdentity> {
        match self {
            AppleTier::Active { identity, .. } => Some(identity),
            AppleTier::Inactive => None,
        }
    }

    /// One line for the cut transcript.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            AppleTier::Inactive => {
                "Tier APPLE inactive (pins::APPLE_TEAM_ID is empty) — ad-hoc signing, \
                 no notarization claimed"
                    .to_string()
            }
            AppleTier::Active { identity, auth } => format!(
                "Tier APPLE active — {} via {}",
                identity.provenance(),
                auth.kind()
            ),
        }
    }
}

/// Resolve the tier before anything irreversible happens.
pub fn resolve_apple_tier(
    team_id: &str,
    credentials: Option<&ReleaseCredentials>,
) -> Result<AppleTier, String> {
    // Fail-closed: an empty anchor never reaches the signing machinery below.
    if team_id.is_empty() {
        return Ok(AppleTier::Inactive);
    }
    let Some((credentials, profile)) =
        credentials.and_then(|c| c.profile().map(|profile| (c, profile)))
    else {
        return Err(format!(
            "no --release-credentials profile was given, and pins::APPLE_TEAM_ID ({team_id}) \
             notarizes every artifact, which needs the profile's notary credential. Cut with `--release-credentials ~/.aterm/release-credentials.toml` (`{} provision \
             --id <machine-id>` writes it)",
            crate::publish::SHIP_COMMAND
        ));
    };
    let auth = credentials.notary().ok_or_else(|| {
        format!(
            "pins::APPLE_TEAM_ID is set to {team_id}, but {} names no notarytool credential. \
             Run `xcrun notarytool store-credentials <name> --apple-id <id> --team-id {team_id} \
             --password <app-specific-password>` once, then add `notary_profile = \"<name>\"` \
             to it",
            profile.display()
        )
    })?;
    let listing = Command::new(SECURITY)
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .map_err(|e| format!("spawn {SECURITY} find-identity: {e}"))?;
    let identity = select_devid_identity(
        &String::from_utf8_lossy(&listing.stdout),
        team_id,
        credentials.signing_identity_sha1(),
    )?;
    Ok(AppleTier::Active {
        identity,
        auth: auth.clone().with_team_id(team_id),
    })
}

/// Sign the assembled bundle and return the `signed_by` provenance string.
pub fn sign_app(
    app: &Path,
    entitlements: &Path,
    sign_id: Option<&DevIdIdentity>,
) -> Result<String, String> {
    let signed_by = match sign_id {
        Some(id) => {
            println!(
                "==> codesign (Developer-ID, hardened runtime + entitlements): {}",
                id.common_name
            );
            run_checked(
                Command::new(CODESIGN)
                    .args([
                        "--force",
                        "--options",
                        "runtime",
                        "--timestamp",
                        "--entitlements",
                    ])
                    .arg(entitlements)
                    .args(["--sign", &id.sha1])
                    .arg(app),
                "codesign app",
            )?;
            id.provenance()
        }
        None => {
            println!("==> codesign (ad-hoc): runs locally, not distributable to other Macs");
            run_checked(
                Command::new(CODESIGN)
                    .args(["--force", "--sign", "-"])
                    .arg(app),
                "codesign app (ad-hoc)",
            )?;
            "ad-hoc".to_string()
        }
    };
    if let Ok(out) = Command::new(CODESIGN)
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

/// Sign the DMG.
pub fn sign_dmg(dmg: &Path, sign_id: &DevIdIdentity) -> Result<(), String> {
    println!("==> codesign dmg (Developer-ID): {}", sign_id.common_name);
    run_checked(
        Command::new(CODESIGN)
            .args(["--force", "--timestamp", "--sign", &sign_id.sha1])
            .arg(dmg),
        "codesign dmg",
    )
}

/// Notarytool credentials.
#[derive(Clone)]
pub enum NotaryAuth {
    KeychainProfile(String),
    AppleId {
        apple_id: String,
        /// Always `pins::APPLE_TEAM_ID`, stamped in by [`NotaryAuth::with_team_id`].
        team_id: String,
        password: String,
    },
}

impl std::fmt::Debug for NotaryAuth {
    /// Hand-written so the `AppleId` password never reaches a transcript, journal or panic
    /// message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotaryAuth::KeychainProfile(name) => {
                f.debug_tuple("KeychainProfile").field(name).finish()
            }
            NotaryAuth::AppleId {
                apple_id, team_id, ..
            } => f
                .debug_struct("AppleId")
                .field("apple_id", apple_id)
                .field("team_id", team_id)
                .field("password", &"<redacted>")
                .finish(),
        }
    }
}

impl NotaryAuth {
    /// Which credential spelling is configured.
    #[must_use]
    pub fn kind(&self) -> String {
        match self {
            NotaryAuth::KeychainProfile(name) => format!("keychain profile {name:?}"),
            NotaryAuth::AppleId { apple_id, .. } => {
                format!("Apple ID {apple_id} + app-specific password")
            }
        }
    }

    /// Stamp the committed anchor into the variant that needs one.
    #[must_use]
    pub fn with_team_id(self, team_id: &str) -> Self {
        match self {
            NotaryAuth::KeychainProfile(name) => NotaryAuth::KeychainProfile(name),
            NotaryAuth::AppleId {
                apple_id, password, ..
            } => NotaryAuth::AppleId {
                apple_id,
                team_id: team_id.to_string(),
                password,
            },
        }
    }

    /// The notarytool argument list this credential expands to.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
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

/// The Developer-ID preflight, pure over `codesign -dv --verbose=2` output.
pub fn devid_preflight(codesign_info: &str, is_app: bool) -> Result<(), String> {
    if codesign_info.contains("Signature=adhoc") {
        return Err(
            "artifact is ad-hoc signed — it cannot be notarized. Tier APPLE resolves the \
                    Developer-ID identity from pins::APPLE_TEAM_ID before the build; reaching \
                    here means the artifact was signed on the inactive path"
                .into(),
        );
    }
    if !codesign_info.contains("Authority=Developer ID Application") {
        return Err(
            "artifact is not Developer-ID signed (no matching Authority) — the certificate \
                    resolved from pins::APPLE_TEAM_ID must be a \"Developer ID Application\" \
                    certificate, not an Installer one"
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
    if classify_dr(codesign_info) == DrClass::Cdhash {
        return Err(
            "artifact's designated requirement is cdhash-class (it pins the exact bytes, \
                    with no identifier clause) — a macOS privacy grant recorded against it \
                    dies on the next build, so every release would re-ask the human for \
                    consent already given. Sign with the Developer-ID identity resolved from \
                    pins::APPLE_TEAM_ID, which yields an identity-class requirement"
                .into(),
        );
    }
    Ok(())
}

/// What a designated requirement is keyed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrClass {
    /// `identifier "com.aterm.aterm" and anchor apple generic and …`.
    Identity,
    /// The requirement pins a `cdhash`.
    Cdhash,
    /// The code object is not signed at all.
    Unsigned,
    /// Not recognised, or no requirement in the text at all.
    #[default]
    Unknown,
}

/// Classify the designated requirement in `codesign -d -r-` output.
///
/// ```text
/// designated => identifier "com.mitchellh.ghostty" and anchor apple generic and …
/// # designated => cdhash H"240c6c45…" or cdhash H"458a3ae8…"
/// code object is not signed at all
/// ```
#[must_use]
pub fn classify_dr(codesign_output: &str) -> DrClass {
    let Some(requirement) = designated_requirement(codesign_output) else {
        let lower = codesign_output.to_ascii_lowercase();
        return if lower.contains("code object is not signed") {
            DrClass::Unsigned
        } else {
            DrClass::Unknown
        };
    };
    let lower = requirement.to_ascii_lowercase();
    if lower.contains("cdhash") {
        return DrClass::Cdhash;
    }
    let has_identifier = lower.contains("identifier ") || lower.contains("identifier\"");
    let has_anchor = lower.contains("anchor apple")
        || lower.contains("certificate leaf")
        || lower.contains("certificate root")
        || lower.contains("subject.ou");
    if has_identifier && has_anchor {
        return DrClass::Identity;
    }
    DrClass::Unknown
}

/// The text after `designated =>` on the one line that carries it.
pub(crate) fn designated_requirement(codesign_output: &str) -> Option<&str> {
    const MARKER: &str = "designated =>";
    codesign_output.lines().find_map(|line| {
        let line = line.trim_start().trim_start_matches('#').trim_start();
        line.strip_prefix(MARKER).map(str::trim)
    })
}

/// Run [`devid_preflight`] against an on-disk artifact.
pub fn check_devid_signed(target: &Path) -> Result<(), String> {
    let out = Command::new(CODESIGN)
        .args(["-dv", "--verbose=2"])
        .arg(target)
        .output()
        .map_err(|e| format!("spawn codesign -dv: {e}"))?;
    let mut info = String::from_utf8_lossy(&out.stderr).to_string();
    info.push_str(&String::from_utf8_lossy(&out.stdout));
    if let Ok(dr) = Command::new(CODESIGN)
        .args(["-d", "-r-"])
        .arg(target)
        .output()
    {
        info.push_str(&String::from_utf8_lossy(&dr.stderr));
        info.push_str(&String::from_utf8_lossy(&dr.stdout));
    }
    let is_app = target.extension().is_some_and(|e| e == "app");
    devid_preflight(&info, is_app).map_err(|e| format!("{}: {e}", target.display()))
}

/// Notarize + staple a .dmg or .app.
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
                Command::new(DITTO)
                    .args(["-c", "-k"])
                    .args(crate::dmg::ARCHIVE_NO_METADATA)
                    .arg("--keepParent")
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
        Command::new(XCRUN)
            .args(["notarytool", "submit"])
            .arg(&submit)
            .args(auth.args())
            .arg("--wait"),
        "notarytool submit",
        NOTARY_SUBMIT_TIMEOUT,
    );
    if let Some(zip) = cleanup {
        let _ = std::fs::remove_file(zip);
    }
    result?;

    println!("==> xcrun stapler staple {}", artifact.display());
    run_streamed(
        Command::new(XCRUN)
            .args(["stapler", "staple"])
            .arg(artifact),
        "stapler staple",
        STAPLE_TIMEOUT,
    )?;
    run_streamed(
        Command::new(XCRUN)
            .args(["stapler", "validate"])
            .arg(artifact),
        "stapler validate",
        STAPLE_TIMEOUT,
    )?;

    println!("==> spctl assessment");
    let mut spctl = Command::new(SPCTL);
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

/// Every Apple tool spawn whose outcome decides whether a release ships.
pub trait AppleTools {
    fn sign_dmg(&self, dmg: &Path, id: &DevIdIdentity) -> Result<(), String>;
    fn check_devid_signed(&self, target: &Path) -> Result<(), String>;
    fn notarize(&self, artifact: &Path, auth: &NotaryAuth) -> Result<(), String>;
    /// `codesign --verify --deep --strict`: the cut's hard signature gate, on every tier
    /// including ad-hoc.
    fn codesign_verify_strict(&self, target: &Path) -> Result<(), String>;
    /// `codesign -dv --verbose=2`, stderr and stdout concatenated, so the parse does not
    /// depend on which stream a macOS release prints the report to.
    fn codesign_dv(&self, target: &Path) -> Result<String, String>;
    /// `xcrun stapler validate`: is the notarization ticket embedded in the artifact.
    fn stapler_validate(&self, target: &Path) -> Result<bool, String>;
    /// `spctl -a` under the assessment this artifact kind gets; see [`spctl_argv`].
    fn gatekeeper_ok(&self, target: &Path, kind: GatekeeperKind) -> Result<bool, String>;
}

/// Which Gatekeeper assessment an artifact gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatekeeperKind {
    /// The `.app` — `-t exec`, the assessment a launched bundle gets.
    App,
    /// The `.dmg` — `-t open --context context:primary-signature`.
    Dmg,
}

/// The `spctl` argv for each artifact kind, a pure function so the argv is tested.
#[must_use]
pub fn spctl_argv(kind: GatekeeperKind) -> Vec<&'static str> {
    match kind {
        GatekeeperKind::App => vec![SPCTL, "-a", "-t", "exec"],
        GatekeeperKind::Dmg => vec![
            SPCTL,
            "-a",
            "-t",
            "open",
            "--context",
            "context:primary-signature",
        ],
    }
}

/// The real tools.
pub struct RealAppleTools;

/// Run a verification tool over `target` and report only whether it passed. A spawn failure
/// is an error, not `false`: a missing tool must not read as a failed check.
fn tool_ok(argv: &[&str], target: &Path) -> Result<bool, String> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| "tool_ok called with an empty argv".to_string())?;
    let out = Command::new(program)
        .args(args)
        .arg(target)
        .output()
        .map_err(|e| format!("spawn {program}: {e}"))?;
    Ok(out.status.success())
}

impl AppleTools for RealAppleTools {
    fn sign_dmg(&self, dmg: &Path, id: &DevIdIdentity) -> Result<(), String> {
        sign_dmg(dmg, id)
    }
    fn check_devid_signed(&self, target: &Path) -> Result<(), String> {
        check_devid_signed(target)
    }
    fn notarize(&self, artifact: &Path, auth: &NotaryAuth) -> Result<(), String> {
        notarize(artifact, auth)
    }
    fn codesign_verify_strict(&self, target: &Path) -> Result<(), String> {
        let out = Command::new(CODESIGN)
            .args(["--verify", "--deep", "--strict"])
            .arg(target)
            .output()
            .map_err(|e| format!("spawn {CODESIGN} --verify: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
    fn codesign_dv(&self, target: &Path) -> Result<String, String> {
        let out = Command::new(CODESIGN)
            .args(["-dv", "--verbose=2"])
            .arg(target)
            .output()
            .map_err(|e| format!("spawn {CODESIGN} -dv: {e}"))?;
        Ok(format!(
            "{}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        ))
    }
    fn stapler_validate(&self, target: &Path) -> Result<bool, String> {
        tool_ok(&[XCRUN, "stapler", "validate"], target)
    }
    fn gatekeeper_ok(&self, target: &Path, kind: GatekeeperKind) -> Result<bool, String> {
        tool_ok(&spctl_argv(kind), target)
    }
}

/// Verify + notarize + staple the `.app`, before either container is built.
pub fn notarize_app(app: &Path, tier: &AppleTier, tools: &dyn AppleTools) -> Result<bool, String> {
    let AppleTier::Active { auth, .. } = tier else {
        // Inactive: ad-hoc signed, nothing claimed, nothing printed.
        return Ok(false);
    };
    tools.check_devid_signed(app)?;
    tools.notarize(app, auth)?;
    Ok(true)
}

/// Sign + notarize + staple the DMG.
pub fn sign_and_notarize_dmg(
    dmg: &Path,
    tier: &AppleTier,
    tools: &dyn AppleTools,
) -> Result<bool, String> {
    let AppleTier::Active { identity, auth } = tier else {
        return Ok(false);
    };
    tools.sign_dmg(dmg, identity)?;
    tools.check_devid_signed(dmg)?;
    tools.notarize(dmg, auth)?;
    Ok(true)
}

/// What the self-check observed about a finished cut's artifacts.
pub struct AppleSelfcheck<'a> {
    /// The manifest's `team_id`.
    pub team_id: &'a str,
    /// `codesign -dv --verbose=2` output for the `.app` (stderr + stdout).
    pub app_codesign_dv: &'a str,
    /// `xcrun stapler validate <app>` succeeded.
    pub app_stapled: bool,
    /// `spctl -a -t exec <app>` succeeded.
    pub app_gatekeeper_ok: bool,
    /// `xcrun stapler validate <dmg>` succeeded.
    pub dmg_stapled: bool,
    /// `spctl -a -t open --context context:primary-signature <dmg>` succeeded.
    pub dmg_gatekeeper_ok: bool,
}

/// Judge a Tier APPLE cut.
pub fn apple_selfcheck_verdict(e: &AppleSelfcheck<'_>) -> Result<(), String> {
    if e.team_id.is_empty() {
        return Err(
            "apple_selfcheck_verdict called with an empty team_id — the Apple branch must \
             only run for a cut that CLAIMS a team"
                .into(),
        );
    }
    if !e
        .app_codesign_dv
        .contains(&format!("TeamIdentifier={}", e.team_id))
    {
        return Err(format!(
            "self-check failed: bundle TeamIdentifier does not match manifest team_id {}",
            e.team_id
        ));
    }
    if !e.app_gatekeeper_ok {
        return Err("self-check failed: spctl assessment rejected the signed app".into());
    }
    if !e.app_stapled {
        return Err(
            "self-check failed: the .app carries no stapled notarization ticket. The zip the \
             fleet downloads is made from this bundle and is assessed OFFLINE by the client, \
             so an unstapled bundle fails every update on a machine that cannot reach Apple"
                .into(),
        );
    }
    if !e.dmg_gatekeeper_ok {
        return Err("self-check failed: Gatekeeper rejected the DMG".into());
    }
    if !e.dmg_stapled {
        return Err(
            "self-check failed: the DMG carries no stapled notarization ticket — a first-run \
             download would have to ask Apple before it could open"
                .into(),
        );
    }
    Ok(())
}

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

/// Run `cmd` with inherited stdio (notarytool's progress streams to the transcript) under a
/// hard deadline: the cut holds a release lease, a publisher fence and a burned build number,
/// and `Command::status()` would wait forever on an Apple stall.
fn run_streamed(cmd: &mut Command, what: &str, timeout: Duration) -> Result<(), String> {
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {what}: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("{what} failed ({status})"))
                };
            }
            Ok(None) => {}
            Err(e) => return Err(format!("wait for {what}: {e}")),
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{what} exceeded {}s and was killed — Apple's notary service may be degraded; \
                 the cut is abandoned rather than left holding the release lease",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod dr_tests {
    use super::{DrClass, classify_dr, devid_preflight};

    /// A Developer-ID app's DR (macOS 26.6.2): an identifier clause plus an anchor and
    /// no cdhash, the shape a shipped aterm.app must have.
    const DEVID_DR: &str = "Executable=/Applications/Ghostty.app/Contents/MacOS/ghostty\n\
         designated => identifier \"com.mitchellh.ghostty\" and anchor apple generic and \
         certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and \
         certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and \
         certificate leaf[subject.OU] = \"24VZTF6M5V\"\n";

    /// The other real Developer-ID spelling: anchor first, unquoted team OU.
    const DEVID_DR_ANCHOR_FIRST: &str = "Executable=/Applications/iTerm.app/Contents/MacOS/iTerm2\n\
         designated => anchor apple generic and identifier \"com.googlecode.iterm2\" and \
         (certificate leaf[field.1.2.840.113635.100.6.1.9] /* exists */ or \
         certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and \
         certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and \
         certificate leaf[subject.OU] = H7V7XYVQ7D)\n";

    /// `codesign --force --sign - <binary>` (`ship dev-app` without the "aterm dev"
    /// identity): a derived (`# `) requirement with no identifier clause.
    const ADHOC_DR: &str = "Executable=/tmp/adhoc-probe\n\
         # designated => cdhash H\"240c6c45749cff92effe2dbe2782cd3ac8bee253\" or \
         cdhash H\"458a3ae87054006e9463429282897dfafa67e862\"\n";

    /// Same, signed with `--identifier com.aterm.aterm.dev`: the identifier does not
    /// reach the requirement, so a dev bundle id alone buys no durable grant.
    const ADHOC_DR_WITH_IDENTIFIER: &str = "Executable=/tmp/adhoc-probe2\n\
         # designated => cdhash H\"291d5a2ba2302d7e67699538f48684c2a1e3babf\" or \
         cdhash H\"bcbfaab1440e2f97f26f0d7b31b9e4710f647918\"\n";

    /// `codesign -d -r-` on an unsigned Mach-O: one stderr line, no requirement.
    const UNSIGNED_DR: &str = "/tmp/unsigned-probe: code object is not signed at all\n";

    #[test]
    fn classifies_the_recorded_shapes() {
        assert_eq!(classify_dr(DEVID_DR), DrClass::Identity);
        assert_eq!(classify_dr(DEVID_DR_ANCHOR_FIRST), DrClass::Identity);
        assert_eq!(classify_dr(ADHOC_DR), DrClass::Cdhash);
        assert_eq!(classify_dr(ADHOC_DR_WITH_IDENTIFIER), DrClass::Cdhash);
        assert_eq!(classify_dr(UNSIGNED_DR), DrClass::Unsigned);
        assert_eq!(classify_dr(""), DrClass::Unknown);
        assert_eq!(
            classify_dr("designated => identifier \"com.aterm.aterm\"\n"),
            DrClass::Unknown
        );
    }

    /// A self-signed identity's requirement (`certificate leaf` for `ship dev-app`'s
    /// "aterm dev", `certificate root` for a certificate with an Organization) pins the
    /// certificate, not the code's bytes, so a grant against it survives a rebuild.
    #[test]
    fn a_self_signed_identity_requirement_is_identity() {
        for requirement in [
            "designated => identifier \"com.aterm.aterm.dev\" and certificate leaf = \
             H\"e11d6156e11d6156e11d6156e11d6156e11d6156\"\n",
            "designated => identifier \"com.aterm.aterm.dev\" and certificate root = \
             H\"0123456789abcdef0123456789abcdef01234567\"\n",
        ] {
            assert_eq!(classify_dr(requirement), DrClass::Identity, "{requirement}");
        }
        let mixed = "designated => identifier \"com.aterm.aterm\" and anchor apple generic \
             and cdhash H\"deadbeef\"\n";
        assert_eq!(classify_dr(mixed), DrClass::Cdhash);
        let other = "host => cdhash H\"deadbeef\"\nlibrary => cdhash H\"feedface\"\n";
        assert_eq!(classify_dr(other), DrClass::Unknown);
        let both = "host => cdhash H\"deadbeef\"\n\
             designated => identifier \"x\" and anchor apple generic\n";
        assert_eq!(classify_dr(both), DrClass::Identity);
    }

    #[test]
    fn preflight_refuses_a_cdhash_designated_requirement() {
        let info = format!(
            "Identifier=com.aterm.aterm\n\
             CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=38+7 location=embedded\n\
             Signature size=8980\n\
             Authority=Developer ID Application: Jane Doe (TEAMID)\n\
             {ADHOC_DR}"
        );
        let err = devid_preflight(&info, true).unwrap_err();
        assert!(err.contains("cdhash-class"), "{err}");
        assert!(devid_preflight(&info, false).is_err());
    }

    #[test]
    fn preflight_accepts_an_identity_designated_requirement() {
        let info = format!(
            "Identifier=com.aterm.aterm\n\
             CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=38+7 location=embedded\n\
             Signature size=8980\n\
             Authority=Developer ID Application: Jane Doe (TEAMID)\n\
             {DEVID_DR}"
        );
        devid_preflight(&info, true).expect("identity-class DR ships");
        devid_preflight(&info, false).expect("identity-class DR ships (dmg)");
    }

    #[test]
    fn requirement_free_text_does_not_invent_a_refusal() {
        let no_probe = "Identifier=com.aterm.aterm\n\
             CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=38+7 location=embedded\n\
             Signature size=8980\n\
             Authority=Developer ID Application: Jane Doe (TEAMID)\n";
        devid_preflight(no_probe, true).expect("no DR probe in the text is not a refusal");
        devid_preflight(UNSIGNED_DR, true).unwrap_err();
    }
}
