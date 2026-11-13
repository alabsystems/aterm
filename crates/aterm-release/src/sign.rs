// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release code signing, notarization, and Apple tier hooks ([`AppleTier::Inactive`],
//! [`AppleTier::Active`], [`select_devid_identity`], [`ReleaseCredentials`],
//! [`check_credentials_perms`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Command, Stdio, command};

const CODESIGN: &str = aterm_uds::os_path!("codesign");
const SPCTL: &str = aterm_uds::os_path!("spctl");
const XCRUN: &str = aterm_uds::os_path!("xcrun");
const SECURITY: &str = aterm_uds::os_path!("security");
const DITTO: &str = aterm_uds::os_path!("ditto");
const ID_TOOL: &str = aterm_uds::os_path!("id");

/// Total bound for upload, reconciliation and acceptance polling.
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
        let key_path = Path::new(&home).join(atpkg_keys::provision::MACHINE_KEY_REL);
        let meta = match std::fs::symlink_metadata(&key_path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("read {}: {error}", key_path.display())),
        };
        if !meta.file_type().is_file() {
            return Err(format!(
                "REFUSING {} — not a regular file",
                key_path.display()
            ));
        }
        let pkcs8 = read_credentials_bytes(&key_path, 4 * 1024)?;
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
        Self::load_with_identity(path, home, |home, profile| {
            #[cfg(unix)]
            {
                let paths = aterm_secrets::client::Paths::for_home(home);
                aterm_secrets::client::release::effective_signing_identity(&paths, profile)
                    .ok()
                    .flatten()
            }
            #[cfg(not(unix))]
            {
                let _ = (home, profile);
                None
            }
        })
    }

    fn load_with_identity(
        path: &Path,
        home: Option<&Path>,
        shared_identity: impl FnOnce(&Path, &Path) -> Option<String>,
    ) -> Result<Self, String> {
        let bytes = read_credentials_bytes(path, 64 * 1024)?;
        let text = String::from_utf8(bytes)
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
        let identity_sha1 = home
            .and_then(|home| shared_identity(home, path))
            .or(identity_sha1);
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
/// (`~/.aterm/release-ring/roster/aterm-machines.toml`) whoever's home that is. A home that is missing or
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

fn read_credentials_bytes(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    #[cfg(not(unix))]
    {
        let pre = std::fs::symlink_metadata(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if !pre.file_type().is_file() {
            return Err(format!("REFUSING {} — not a regular file", path.display()));
        }
    }
    let file = match astream_sys::open_nonblocking(path, astream_sys::Links::Refuse) {
        Ok(file) => file,
        #[cfg(unix)]
        Err(_) if std::fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_file()) => {
            return Err(format!("REFUSING {} — not a regular file", path.display()));
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let meta = file
        .metadata()
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("REFUSING {} — not a regular file", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        check_credentials_perms(meta.uid(), meta.mode(), current_uid()?, path)?;
    }
    if meta.len() > max {
        return Err(format!(
            "read {}: credential file exceeds {max} bytes",
            path.display()
        ));
    }
    let cap = usize::try_from(meta.len().min(64 * 1024).saturating_add(1)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(cap);
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err(format!(
            "read {}: credential file exceeds {max} bytes",
            path.display()
        ));
    }
    Ok(bytes)
}

/// Current uid via `id -u`; keeps the crate free of a libc dependency for one syscall.
#[cfg(unix)]
pub(crate) fn current_uid() -> Result<u32, String> {
    let out = command(ID_TOOL)
        .arg("-u")
        .output()
        .map_err(|e| format!("spawn id -u: {e}"))?;
    if !out.status.success() {
        return Err("id -u failed".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let trimmed = text.trim();
    if !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("id -u parse: invalid uid {trimmed:?}"));
    }
    trimmed.parse().map_err(|e| format!("id -u parse: {e}"))
}

/// The Developer-ID certificate this cut signs with, resolved from the anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevIdIdentity {
    /// Uppercase 40-hex SHA-1 of the certificate, as `security` prints it.
    pub sha1: String,
    /// `Developer ID Application: <name> (<TEAMID>)`.
    pub common_name: String,
    /// The keychain codesign is told to find it in (`--keychain`): aterm's release keychain
    /// when it holds the identity, so the signature does not depend on search order. `None`
    /// for one only the keychain search list holds (the login keychain of a Mac provisioned
    /// before the release keychain), which codesign finds there as it always has.
    pub keychain: Option<PathBuf>,
}

impl DevIdIdentity {
    /// The provenance string recorded in the sidecar.
    #[must_use]
    pub fn provenance(&self) -> String {
        format!("{} [{}]", self.common_name, self.sha1)
    }
}

/// The `codesign` arguments that name `id`: `--keychain <file> --sign <sha1>` when a keychain
/// holds it ([`DevIdIdentity::keychain`]), else `--sign <sha1>`.
#[must_use]
pub fn devid_sign_args(id: &DevIdIdentity) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    if let Some(keychain) = &id.keychain {
        args.push("--keychain".into());
        args.push(keychain.into());
    }
    args.push("--sign".into());
    args.push((&id.sha1).into());
    args
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
            "no valid \"Developer ID Application: … ({team_id})\" certificate in aterm's \
             release keychain or on the keychain search list. `security find-identity -v -p \
             codesigning` must list one; `{} provision --id <machine-id>` imports the \
             Developer-ID Application certificate and its private key for team {team_id} into \
             the release keychain, or leave pins::APPLE_TEAM_ID empty to keep Tier APPLE off",
            crate::publish::SHIP_COMMAND
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
             pipeline cannot pick one: {}. Remove the superseded certificate from its \
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
        keychain: None,
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
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_apple_tier_with(
        team_id,
        credentials,
        home.as_deref(),
        &crate::dev_identity::Bounded,
    )
}

fn resolve_apple_tier_with(
    team_id: &str,
    credentials: Option<&ReleaseCredentials>,
    home: Option<&Path>,
    run: &dyn crate::dev_identity::Run,
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
             notarizes every artifact, which needs the profile's notary credential. Cut with `--release-credentials ~/.aterm/release-ring/release-credentials.toml` (`{} provision \
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
    let identity = locate_identity(team_id, credentials.signing_identity_sha1(), home, run)?;
    Ok(AppleTier::Active {
        identity,
        auth: auth.clone().with_team_id(team_id),
    })
}

/// The identity a cut signs with: first in aterm's release keychain under `home`, opened with
/// its stored password and named by path
/// ([`crate::release_keychain::Keychains::signing_keychain`]); else on the keychain search
/// list, read-only, as every cut read it before the release keychain existed. A release
/// keychain that holds the identity and will not open is refused, so codesign never asks for
/// its password in a dialog; one that holds none is no reason to refuse an identity the search
/// list holds.
pub(crate) fn locate_identity(
    team_id: &str,
    preferred: Option<&str>,
    home: Option<&Path>,
    run: &dyn crate::dev_identity::Run,
) -> Result<DevIdIdentity, String> {
    #[cfg(unix)]
    {
        if let Some(home) = home {
            let kc = crate::release_keychain::Keychains::waiting(home, run);
            if let Ok(mut identity) =
                select_devid_identity(&kc.release_listing(true)?, team_id, preferred)
            {
                // A cut signs next, so the partition list is re-asserted first.
                identity.keychain = kc.signing_keychain(
                    team_id,
                    &identity.sha1,
                    crate::release_keychain::Leave::Write,
                )?;
                return Ok(identity);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = home;
    let mut listing = aterm_uds::spawnfd::command(SECURITY);
    listing.args(["find-identity", "-v", "-p", "codesigning"]);
    let listing = run
        .run(&mut listing, crate::dev_identity::WAIT)
        .map_err(|e| format!("{SECURITY} find-identity: {e}"))?;
    select_devid_identity(
        &String::from_utf8_lossy(&listing.stdout),
        team_id,
        preferred,
    )
}

/// Sign the assembled bundle and return the `signed_by` provenance string.
pub fn sign_app(
    app: &Path,
    entitlements: &Path,
    sign_id: Option<&DevIdIdentity>,
) -> Result<String, String> {
    let signed_by = match sign_id {
        Some(id) => {
            #[cfg(unix)]
            let _lease = {
                let home = std::env::var_os("HOME").ok_or("HOME is missing for signing")?;
                aterm_secrets::release_keychain::wait_signing_lease(Path::new(&home))?
            };
            println!(
                "==> codesign (Developer-ID, hardened runtime + entitlements): {}",
                id.common_name
            );
            run_checked(
                command(CODESIGN).args(app_sign_args(app, entitlements, id)),
                "codesign app",
            )?;
            id.provenance()
        }
        None => {
            println!("==> codesign (ad-hoc): runs locally, not distributable to other Macs");
            run_checked(
                command(CODESIGN).args(["--force", "--sign", "-"]).arg(app),
                "codesign app (ad-hoc)",
            )?;
            "ad-hoc".to_string()
        }
    };
    if let Ok(out) = command(CODESIGN)
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
    #[cfg(unix)]
    let _lease = {
        let home = std::env::var_os("HOME").ok_or("HOME is missing for signing")?;
        aterm_secrets::release_keychain::wait_signing_lease(Path::new(&home))?
    };
    println!("==> codesign dmg (Developer-ID): {}", sign_id.common_name);
    run_checked(
        command(CODESIGN).args(dmg_sign_args(dmg, sign_id)),
        "codesign dmg",
    )
}

/// `codesign`'s arguments for the bundle: hardened runtime, entitlements, a timestamp, `id`.
fn app_sign_args(app: &Path, entitlements: &Path, id: &DevIdIdentity) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = [
        "--force",
        "--options",
        "runtime",
        "--timestamp",
        "--entitlements",
    ]
    .map(Into::into)
    .into();
    args.push(entitlements.into());
    args.extend(devid_sign_args(id));
    args.push(app.into());
    args
}

/// `codesign`'s arguments for the DMG: a timestamp and `id`.
fn dmg_sign_args(dmg: &Path, id: &DevIdIdentity) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = ["--force", "--timestamp"].map(Into::into).into();
    args.extend(devid_sign_args(id));
    args.push(dmg.into());
    args
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
    let has = |haystack: &str, needle: &[u8]| {
        haystack
            .as_bytes()
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
    };
    let Some(requirement) = designated_requirement(codesign_output) else {
        return if has(codesign_output, b"code object is not signed") {
            DrClass::Unsigned
        } else {
            DrClass::Unknown
        };
    };
    if has(requirement, b"cdhash") {
        return DrClass::Cdhash;
    }
    let has_identifier = has(requirement, b"identifier ") || has(requirement, b"identifier\"");
    let has_anchor = has(requirement, b"anchor apple")
        || has(requirement, b"certificate leaf")
        || has(requirement, b"certificate root")
        || has(requirement, b"subject.ou");
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
    let out = command(CODESIGN)
        .args(["-dv", "--verbose=2"])
        .arg(target)
        .output()
        .map_err(|e| format!("spawn codesign -dv: {e}"))?;
    let mut info = String::from_utf8_lossy(&out.stderr).to_string();
    info.push_str(&String::from_utf8_lossy(&out.stdout));
    if let Ok(dr) = command(CODESIGN).args(["-d", "-r-"]).arg(target).output() {
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
    let app_digest = if ext == "app" {
        Some(crate::artifact::bundle_digest(artifact)?)
    } else {
        None
    };
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
                command(DITTO)
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

    let result = notary_accept(
        &submit,
        auth,
        XCRUN,
        NOTARY_SUBMIT_TIMEOUT,
        Duration::from_secs(15),
    );
    if let Some(zip) = cleanup {
        let _ = std::fs::remove_file(zip);
    }
    result?;
    if let Some(before) = app_digest
        && crate::artifact::bundle_digest(artifact)? != before
    {
        return Err("notary app changed while awaiting acceptance; refusing staple".into());
    }

    println!("==> xcrun stapler staple {}", artifact.display());
    run_streamed(
        command(XCRUN).args(["stapler", "staple"]).arg(artifact),
        "stapler staple",
        STAPLE_TIMEOUT,
    )?;
    run_streamed(
        command(XCRUN).args(["stapler", "validate"]).arg(artifact),
        "stapler validate",
        STAPLE_TIMEOUT,
    )?;

    println!("==> spctl assessment");
    let mut spctl = command(SPCTL);
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

// History names identify candidates, not bytes. Only Apple's completed log with
// the candidate's ID and the snapshot's digest can authorize acceptance. Never
// use an empty history as evidence of non-submission or repeat an uncertain submit.
fn notary_accept(
    artifact: &Path,
    auth: &NotaryAuth,
    xcrun: &str,
    bound: Duration,
    pause: Duration,
) -> Result<(), String> {
    notary_accept_with_clock(artifact, auth, xcrun, bound, pause, Instant::now)
}

fn notary_accept_with_clock(
    artifact: &Path,
    auth: &NotaryAuth,
    xcrun: &str,
    bound: Duration,
    pause: Duration,
    now: impl Fn() -> Instant,
) -> Result<(), String> {
    let scratch = aterm_tempfile::tempdir().map_err(|e| format!("notary scratch: {e}"))?;
    let nonce = scratch
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("notary scratch name is not UTF-8")?;
    let copy = scratch.path().join("upload");
    std::fs::copy(artifact, &copy).map_err(|e| format!("notary upload copy: {e}"))?;
    let digest = crate::dmg::sha256_file(&copy)?;
    let name = format!(
        "aterm-{}-{nonce}.{}",
        digest,
        artifact
            .extension()
            .and_then(|e| e.to_str())
            .ok_or("notary upload extension")?
    );
    let upload = scratch.path().join(&name);
    std::fs::rename(copy, &upload).map_err(|e| format!("notary upload rename: {e}"))?;
    let deadline = now() + bound;
    let mut id: Option<String> = None;
    let mut accepted = false;
    let mut attempt = 0;
    loop {
        if now() >= deadline || attempt >= 160 {
            return Err(format!(
                "notary recovery exhausted {}s; outcome unknown, no acceptance claimed",
                bound.as_secs()
            ));
        }
        attempt += 1;
        let verb = if attempt == 1 {
            "submit"
        } else if accepted {
            "log"
        } else if id.is_some() {
            "info"
        } else {
            "history"
        };
        println!("==> notary attempt {attempt}: {verb}, upload {name}");
        let mut cmd = command(xcrun);
        cmd.args(["notarytool", verb]);
        if verb == "submit" {
            cmd.arg(&upload);
        }
        if matches!(verb, "info" | "log") {
            cmd.arg(id.as_deref().ok_or("missing notary id")?);
        }
        cmd.args(auth.args()).args(["--output-format", "json"]);
        let call_bound = if verb == "submit" {
            Duration::from_secs(5 * 60)
        } else {
            Duration::from_secs(60)
        };
        // Both streams must be complete: dropping a prefix could turn garbled
        // JSON into an Accepted object or hide a credential failure.
        let out = match aterm_uds::deadline::output_with_input_until(
            &mut cmd,
            aterm_uds::secret::Secret::new(&[]),
            deadline.min(now() + call_bound),
            aterm_uds::deadline::OUTPUT_KEEP,
        ) {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                println!("==> notary attempt {attempt}: {verb} timed out; outcome unknown");
                std::thread::sleep(pause.min(deadline.saturating_duration_since(now())));
                continue;
            }
            Err(e) => return Err(format!("notary {verb}: {e}; no acceptance claimed")),
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Tool output (including response IDs) may echo secrets. Log only our
        // own operation and the process status, never either captured stream.
        println!("==> notary attempt {attempt}: {}", out.status);
        if now() >= deadline {
            return Err("notary recovery deadline reached; no acceptance claimed".into());
        }
        let json = aterm_json::from_slice::<aterm_json::Value>(&out.stdout).ok();
        // Credentials and definitive verdicts outrank transient-looking text.
        let diagnostic = format!("{stdout}\n{stderr}").to_ascii_lowercase();
        if ["invalid", "rejected"].iter().any(|s| {
            json.as_ref()
                .and_then(|j| j.get("status"))
                .and_then(|v| v.as_str())
                .is_some_and(|v| v.eq_ignore_ascii_case(s))
        }) {
            return Err(format!("notary {verb}: definitive rejection"));
        }
        if [
            "http 401",
            "http 403",
            "statuscode: optional(401)",
            "statuscode: optional(403)",
            "unauthorized",
            "forbidden",
            "credential",
            "authentication",
            "password",
            "agreement",
        ]
        .iter()
        .any(|s| diagnostic.contains(s))
        {
            return Err(format!("notary {verb}: credential/authorization error"));
        }
        if !out.status.success() {
            if !notary_transient(&diagnostic) {
                return Err(format!(
                    "notary {verb} failed ({}); no acceptance claimed",
                    out.status
                ));
            }
            if verb == "submit" {
                id = json
                    .as_ref()
                    .and_then(|j| j.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
            }
        } else if let Some(json) = json.as_ref() {
            if verb == "history" {
                if let Some(entries) = json.get("history").and_then(|v| v.as_array()) {
                    let matches: Vec<_> = entries
                        .iter()
                        .filter(|v| v.get("name").and_then(|v| v.as_str()) == Some(name.as_str()))
                        .collect();
                    if matches.len() > 1 {
                        return Err("ambiguous notary history; no acceptance claimed".into());
                    }
                    if let Some(entry) = matches.first() {
                        if entry
                            .get("status")
                            .and_then(|v| v.as_str())
                            .is_some_and(|s| {
                                s.eq_ignore_ascii_case("Invalid")
                                    || s.eq_ignore_ascii_case("Rejected")
                            })
                        {
                            return Err("notary history: definitive rejection".into());
                        }
                        id = entry.get("id").and_then(|v| v.as_str()).map(str::to_owned);
                    }
                }
            } else if verb == "log" {
                let field = |key| json.get(key).and_then(|v| v.as_str());
                if !field("jobId")
                    .zip(id.as_deref())
                    .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
                    || field("sha256") != Some(digest.as_str())
                    || field("archiveFilename") != Some(name.as_str())
                    || field("status") != Some("Accepted")
                {
                    return Err(
                        "notary log does not attest to this upload; no acceptance claimed".into(),
                    );
                }
                if crate::dmg::sha256_file(artifact)? != digest {
                    return Err("notary artifact changed while awaiting acceptance".into());
                }
                if now() >= deadline {
                    return Err("notary recovery deadline reached; no acceptance claimed".into());
                }
                return Ok(());
            } else {
                let response_id = json
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or("notary response missing id")?;
                if id.as_deref().is_some_and(|id| id != response_id) {
                    return Err("notary response id mismatch".into());
                }
                id = Some(response_id.to_owned());
                match json.get("status").and_then(|v| v.as_str()) {
                    Some("Accepted") => accepted = true,
                    Some("In Progress") | None if verb == "submit" => {}
                    Some("In Progress") => {}
                    _ => return Err("unknown notary verdict; no acceptance claimed".into()),
                }
            }
        } else {
            return Err("malformed notary response; no acceptance claimed".into());
        }
        std::thread::sleep(pause.min(deadline.saturating_duration_since(now())));
    }
}

fn notary_transient(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| {
            word.len() == 3 && word.starts_with('5') && word.bytes().all(|b| b.is_ascii_digit())
        })
        || [
            "gatewaytimeout",
            "timed out",
            "timeout",
            "connection reset",
            "connection lost",
            "connection was lost",
            "connection closed",
            "connection dropped",
            "could not connect",
            "network is unreachable",
        ]
        .iter()
        .any(|s| text.contains(s))
}

#[cfg(all(test, unix))]
#[path = "sign_notary_tests.rs"]
mod notary_tests;

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
    let out = command(program)
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
        let out = command(CODESIGN)
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
        let out = command(CODESIGN)
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
        .match_indices("TeamIdentifier=")
        .any(|(i, _)| e.app_codesign_dv[i + "TeamIdentifier=".len()..].starts_with(e.team_id))
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
        .stdin(Stdio::null())
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

#[cfg(all(test, unix))]
#[path = "sign_keychain_tests.rs"]
mod keychain_tests;

#[cfg(test)]
#[path = "sign_dr_tests.rs"]
mod dr_tests;
