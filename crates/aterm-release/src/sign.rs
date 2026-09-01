// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signing (release spec §6 `sign.rs`, absorbing notarize.sh): inside-out
//! codesign, plus THE Tier APPLE hook.
//!
//! # The two states, and nothing in between
//!
//! Tier APPLE is a two-state machine driven by ONE committed constant,
//! `aterm_update_core::pins::APPLE_TEAM_ID`:
//!
//! * **anchor empty → [`AppleTier::Inactive`]**. The bundle is signed ad-hoc
//!   (`--sign -`), the DMG is not signed, nothing is notarized, and the
//!   manifest's `team_id` is empty. This is what aterm ships today and this
//!   change does not move one byte of it.
//! * **anchor set → [`AppleTier::Active`]**. Developer-ID sign, notarize,
//!   staple and VERIFY — or fail the cut. There is deliberately no third state
//!   in which the pipeline signs with a Developer-ID identity and then declines
//!   to notarize, because the manifest emits `team_id` from the anchor
//!   unconditionally and both `tools/install.sh` and the in-app updater read a
//!   non-empty `team_id` as a PROMISE that the artifact is notarized. A code
//!   path that signs-but-does-not-notarize is a path that ships a broken
//!   promise, so it does not exist: every fallible operation on the active path
//!   propagates its error to the caller and aborts the cut.
//!
//! # Where each value comes from
//!
//! Nothing about Tier APPLE is ambient. The Team ID is a committed constant
//! (never an environment variable — see `pins.rs`). The certificate lives in the
//! cutting machine's keychain and its identity STRING is **derived** from the
//! anchor by [`select_devid_identity`], so no identity is ever committed or
//! typed on a command line. The notarytool credential is named — not carried —
//! by the same `--release-credentials` profile that already holds the Ed25519
//! signing key; see [`NotaryAuth`].
//!
//! # Ports three shell sources at once
//!
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
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Tool paths — absolute, never PATH-resolved
// ---------------------------------------------------------------------------

// A release pipeline is exactly the process you least want resolving `codesign`
// through `$PATH`: whoever can drop a file earlier on the path chooses what
// "signed" and "Gatekeeper approved" mean for every copy of aterm that ships.
// The in-app updater already spawns `/usr/bin/codesign` and `/usr/sbin/spctl`
// absolutely (crates/aterm-update/src/verify.rs); the producer side had drifted
// to bare names. These constants close that gap, and every spawn that decides a
// signing verdict now goes through [`RealAppleTools`], which is the one place
// they are named — so a new call site cannot re-introduce a bare `codesign`
// without also stepping outside the seam its own test needs.
const CODESIGN: &str = "/usr/bin/codesign";
const SPCTL: &str = "/usr/sbin/spctl";
const XCRUN: &str = "/usr/bin/xcrun";
const SECURITY: &str = "/usr/bin/security";
const DITTO: &str = "/usr/bin/ditto";
const ID_TOOL: &str = "/usr/bin/id";

/// How long `notarytool submit --wait` may run before the cut gives up.
///
/// Notarization is network I/O in the middle of a pipeline that is HOLDING a
/// release lease, a publisher fence, and a burned single-use build number. When
/// Apple's service is degraded `submit --wait` does not fail — it waits, and an
/// unbounded wait converts a service outage into a wedged cut that nobody can
/// resume because the lease never comes back. Forty minutes is far beyond the
/// minutes a healthy submission takes and far short of "overnight"; past it, the
/// honest answer is to fail, release the lease, and resume later.
pub const NOTARY_SUBMIT_TIMEOUT: Duration = Duration::from_secs(40 * 60);

/// Bound for the short notarization tools (`stapler staple` / `stapler
/// validate`). These are a ticket fetch and a local check; if either is still
/// running after five minutes something is wrong with the network path, and
/// waiting longer only delays the same verdict.
const STAPLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

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
/// The profile's key of record is the Ed25519 signing key. It ALSO carries the
/// Tier APPLE notarization credential — because the alternative was worse.
///
/// ```toml
/// signing_key = "<base64 PKCS#8 Ed25519 private key>"
///
/// # Tier APPLE only (ignored while pins::APPLE_TEAM_ID is empty):
/// notary_profile = "<name given to `xcrun notarytool store-credentials`>"
/// # ...or, for a machine with no usable keychain, the headless fallback:
/// # notary_apple_id = "<Apple ID>"
/// # notary_password = "<app-specific password>"
///
/// # Machine-roster tier only (ignored while pins::PAPER_MASTER_PUBKEYS is empty):
/// machine_roster = "<path to the master-signed aterm-machines.toml>"
/// machine_id     = "<this machine's id, e.g. the one `join --id` was given>"
/// ```
///
/// Base64 is required, not incidental: the minted machine key is BINARY PKCS#8, so a
/// raw paste into TOML cannot round-trip. The loader says so rather than failing with
/// a parse error nobody can act on.
///
/// # Why the notary credential lives HERE and not in a new channel
///
/// The pipeline already has exactly one answer to "what signed this?": the path
/// you named on the command line. A second, differently-shaped channel for the
/// Apple credential — an environment variable, a per-machine dotfile, a second
/// flag — would re-introduce the ambient discovery that `--release-credentials`
/// exists to abolish, and would do it for the one credential whose absence is
/// discovered LAST, twenty minutes into a cut. So the profile grew two optional
/// keys instead of the pipeline growing a second credential channel.
///
/// Note what is NOT stored: `notary_profile` is a keychain LABEL, not a secret,
/// which is why it is the default and only spelling most machines need. The
/// `notary_apple_id`/`notary_password` fallback does put a live secret in this
/// file — which is precisely why [`check_credentials_perms`] already refuses any
/// profile readable by group or other.
///
/// # Why the machine roster is named HERE too
///
/// Same argument, one tier further along. Under one paper master and many machine
/// keys the cut has to answer "may THIS machine publish?", and answering it needs
/// the master-signed roster (`aterm-machines.toml`) plus its detached signature.
/// Those bytes had to come from somewhere, and the three candidates were a CLI
/// flag, a conventional path, and this file.
///
/// This file wins for the reason `notary_profile` did: the profile is ALREADY the
/// single answer to "what signed this?", it is already loaded once at the top of
/// the cut, and its keys are already parsed eagerly — so a profile that names a
/// roster it cannot read fails beside a signing key it cannot parse, minutes
/// before a ledger number is burned, rather than at the moment of signing. A
/// second, differently-shaped channel would re-introduce exactly the ambient
/// discovery `--release-credentials` exists to abolish. And it keeps the pairing
/// honest: the key that signs and the roster that authorizes it are named by the
/// same file, so they cannot be swapped independently.
///
/// Neither roster key is a SECRET (a roster is published as a release asset, and a
/// machine id is printed in every appcast), and neither is a trust ANCHOR: the
/// master that must have signed the roster is `pins::PAPER_MASTER_PUBKEYS`, a
/// committed constant this file cannot influence. `machine_id` is a cross-check
/// only — it can narrow what is accepted (a profile that disagrees with the roster
/// refuses the cut) and can never widen it, exactly like
/// [`ReleaseCredentials::signing_identity_sha1`].
#[derive(Clone)]
pub struct ReleaseCredentials {
    /// Raw PKCS#8 bytes. Never logged, never journaled, never serialized.
    pkcs8: Vec<u8>,
    /// The derived public identity — the only part that is ever recorded.
    pubkey_b64: String,
    /// This machine's PUBLIC id, as the profile declares it. Optional, and never
    /// authority: the roster's own key→id map decides who this machine is, and
    /// this value is compared against that answer so a stale or copied profile
    /// refuses the cut instead of publishing a wrong attribution.
    machine_id: Option<String>,
    /// Path to the master-signed machine roster. Its detached master signature is
    /// the sibling `<path>.sig`, exactly as the appcast's is — one name in the
    /// file, no second key that can drift from it.
    machine_roster: Option<PathBuf>,
    /// notarytool auth, if the profile names any. Parsed eagerly so a malformed
    /// Apple stanza is reported at load time — beside every other credential
    /// error — rather than after a build has already been paid for.
    notary: Option<NotaryAuth>,
    /// Optional keychain disambiguator (`signing_identity_sha1`). Per-machine
    /// state, never a trust anchor: it selects among certificates that ALREADY
    /// match the committed Team ID, and can therefore never widen what is
    /// accepted, only narrow it. See [`select_devid_identity`].
    identity_sha1: Option<String>,
}

impl std::fmt::Debug for ReleaseCredentials {
    /// Hand-written so the private key can never reach a log through a derive.
    /// Only the public identity is printable. [`NotaryAuth`] carries the same
    /// hand-written treatment for the same reason.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `machine_id` is printable for the same reason `pubkey` is: it is public
        // identity, and it is the one field a reader of a failure log actually needs
        // ("which machine refused?"). The roster PATH is deliberately absent — it is
        // per-machine noise that commonly embeds a home directory, and no decision is
        // ever explained by it.
        f.debug_struct("ReleaseCredentials")
            .field("pubkey", &self.pubkey_b64)
            .field("machine_id", &self.machine_id)
            .field("pkcs8", &"<redacted>")
            .field("notary", &self.notary)
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
    /// Resolve signing material the ONE-PATH way: an explicit `--release-credentials`
    /// profile when given, else THIS MACHINE's provisioned identity
    /// (`~/.aterm/machine.key` + `machine.toml`, written by `atpkg-keys setup`/`join`),
    /// else `None` — the ordinary unsigned/ad-hoc cut.
    ///
    /// # Why ambient machine state is now correct where `release.conf` was not
    ///
    /// The doc above retired `~/.aterm/release.conf` because an ambient file silently
    /// decided WHAT KEY signed, and "what signed this?" was answerable only by shell
    /// archaeology. Both premises have flipped. The machine key is not "some key a
    /// file happens to name" — it is this machine's identity, minted once by
    /// `atpkg-keys setup`/`join`, useless unless the master-signed roster names it,
    /// and the atpkg producer tools already read it from exactly this location. And
    /// "what signed this?" is now answered by the ATTRIBUTION inside the signed
    /// manifest (`machine_id`, bound by every client), which is a strictly better
    /// answer than the path on a command line. Requiring the owner to base64 the same
    /// key into a second file per product was the last two-channel seam in the design.
    ///
    /// The flag still wins when present (Apple stanza, or a deliberately different
    /// key), and a HALF-provisioned machine — key without identity, or unreadable
    /// either — is an ERROR, never a silent fall-through to unsigned: fail closed,
    /// name the file, name the remedy.
    pub fn resolve(explicit: Option<&Path>, repo: &Path) -> Result<Option<Self>, String> {
        Self::resolve_with(explicit, repo, std::env::var("HOME").ok().as_deref())
    }

    /// [`Self::resolve`] with the home directory injected — the seam the tests drive,
    /// because `std::env::set_var` in a test races every other test in the binary.
    pub(crate) fn resolve_with(
        explicit: Option<&Path>,
        repo: &Path,
        home: Option<&str>,
    ) -> Result<Option<Self>, String> {
        if let Some(path) = explicit {
            return Self::load(path).map(Some);
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
        // BINARY PKCS#8, exactly as `atpkg-keys` wrote it — no base64 hop, which is
        // the point: the profile needed one only because TOML cannot carry raw bytes.
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
        // machine_id stays None here: `machines::declared_machine_id` already reads
        // `~/.aterm/machine.toml` as its fallback, and a second reader would be a
        // second parser to keep honest. The roster defaults to the repo's staged
        // `dist/aterm-machines.toml` — the same default every atpkg producer tool
        // uses — but only if it exists; the armed gate decides whether its absence
        // is fatal, because only it knows whether the tier is armed.
        let staged = repo.join("dist/aterm-machines.toml");
        Ok(Some(Self {
            pkcs8,
            pubkey_b64,
            machine_id: None,
            machine_roster: staged.exists().then_some(staged),
            notary: None,
            identity_sha1: None,
        }))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
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
        // Parsed EAGERLY, beside every other credential: a malformed roster stanza is
        // reported here — before the gates, before the claim — rather than at the
        // moment the roster is needed. The bytes are NOT read here, because reading
        // them is a decision the roster tier makes (`publish::preflight_signature_policy`)
        // and this loader has no business knowing whether the tier is armed.
        let machine_id = credentials_value(&text, "machine_id")
            .map_err(|e| format!("{}: {e}", path.display()))?
            .map(str::to_string);
        let machine_roster = credentials_value(&text, "machine_roster")
            .map_err(|e| format!("{}: {e}", path.display()))?
            .map(PathBuf::from);
        Ok(Self {
            pkcs8,
            pubkey_b64,
            machine_id,
            machine_roster,
            notary,
            identity_sha1,
        })
    }

    /// The notarytool auth the profile names, if any.
    ///
    /// `None` here is not an error at load time — Tier APPLE is off in the
    /// shipped build, and demanding an Apple credential from every cut would
    /// break the tier that actually ships. It becomes a hard error only in
    /// [`resolve_apple_tier`], i.e. only once the anchor says the artifact is
    /// going to CLAIM notarization.
    #[must_use]
    pub fn notary(&self) -> Option<&NotaryAuth> {
        self.notary.as_ref()
    }

    /// The keychain SHA-1 the operator pinned, if the profile names one.
    /// `None` on every ordinary machine; see [`select_devid_identity`].
    #[must_use]
    pub fn signing_identity_sha1(&self) -> Option<&str> {
        self.identity_sha1.as_deref()
    }

    /// The public identity of the loaded key — what preflight matches against the
    /// committed anchor, and the first of the two values the journal records.
    #[must_use]
    pub fn pubkey(&self) -> &str {
        &self.pubkey_b64
    }

    /// This machine's PUBLIC id as the profile declares it, if it declares one.
    ///
    /// `None` on every machine that has not been minted, which is every machine
    /// while `pins::PAPER_MASTER_PUBKEYS` is empty. Declaring it is optional even
    /// on the armed path — the roster maps the signing key to an id by itself —
    /// but declaring it WRONG is fatal, which is the whole value of the key.
    #[must_use]
    pub fn machine_id(&self) -> Option<&str> {
        self.machine_id.as_deref()
    }

    /// The master-signed roster this cut must be authorized by, if the profile
    /// names one. Its detached master signature is the sibling `<path>.sig`.
    #[must_use]
    pub fn machine_roster(&self) -> Option<&Path> {
        self.machine_roster.as_deref()
    }

    /// Detached Ed25519 signature over `msg`.
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(&self.pkcs8)
            .map_err(|_| "signing key became unusable after load".to_string())?;
        Ok(keypair.sign(msg).as_ref().to_vec())
    }
}

/// Pull one quoted value out of the profile without a TOML dependency: the file has
/// a handful of flat keys, and a hand-rolled reader keeps the parse surface as small
/// as the secrets it is reading.
///
/// `Ok(None)` means the key is absent — legal for the optional Apple keys. An empty
/// value is NOT absent: writing `notary_profile = ""` is a mistake with a silent
/// failure mode (the tier would look configured and then behave as if it were not),
/// so it is refused by name.
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
///
/// The two spellings are mutually exclusive and the refusal is deliberate: a
/// profile naming both leaves "which one authenticated this submission?"
/// unanswerable by reading the file, which is the exact property this whole
/// credentials redesign exists to guarantee.
///
/// Note the ABSENCE of a `team_id` key. `NotaryAuth::AppleId` carries a team id
/// because notarytool's argv demands one, but it is filled from
/// `pins::APPLE_TEAM_ID` at [`resolve_apple_tier`] and never read from this
/// file. A second writable home for a Team ID is a second thing that can
/// silently disagree with the anchor — the failure mode `pins.rs` exists to
/// abolish — so the profile has no such key.
///
/// Be precise about what that buys, because it is one-directional. This reader
/// looks up only the keys it knows, so a `team_id = "…"` line written into the
/// profile is INERT, not refused: no error, no warning, no effect. What is
/// guaranteed is the direction that matters — no value from this file can reach
/// notarytool's `--team-id`, because the only thing that fills it is
/// [`NotaryAuth::with_team_id`] and the only thing that calls it is
/// [`resolve_apple_tier`], from the committed anchor. The two cannot disagree
/// because only one of them is ever read.
pub fn credentials_notary_auth(text: &str) -> Result<Option<NotaryAuth>, String> {
    let profile = credentials_value(text, "notary_profile")?;
    let apple_id = credentials_value(text, "notary_apple_id")?;
    let password = credentials_value(text, "notary_password")?;
    match (profile, apple_id, password) {
        (None, None, None) => Ok(None),
        (Some(name), None, None) => Ok(Some(NotaryAuth::KeychainProfile(name.to_string()))),
        (None, Some(apple_id), Some(password)) => Ok(Some(NotaryAuth::AppleId {
            apple_id: apple_id.to_string(),
            // Filled in by `resolve_apple_tier` from the committed anchor. It is
            // left empty here so that no code path can construct a NotaryAuth
            // whose team id came from anywhere but pins.rs.
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

// ---------------------------------------------------------------------------
// Tier APPLE — resolving the anchor into a usable identity
// ---------------------------------------------------------------------------

/// The Developer-ID certificate this cut signs with, resolved from the anchor.
///
/// Both halves are load-bearing. `sha1` is what `codesign --sign` is given,
/// because it names ONE certificate: passing the common name instead makes
/// codesign substring-match, which on a keychain holding both a "Developer ID
/// Application" and a "Developer ID Installer" cert (the normal case for a team
/// that has ever shipped a .pkg) is ambiguous. `common_name` is what provenance
/// records, because a bare hash tells a reader nothing about who signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevIdIdentity {
    /// Uppercase 40-hex SHA-1 of the certificate, as `security` prints it.
    pub sha1: String,
    /// `Developer ID Application: <name> (<TEAMID>)`.
    pub common_name: String,
}

impl DevIdIdentity {
    /// The provenance string recorded in the sidecar: readable AND exact.
    #[must_use]
    pub fn provenance(&self) -> String {
        format!("{} [{}]", self.common_name, self.sha1)
    }
}

/// The prefix every Developer-ID *code-signing* certificate's common name
/// carries. Matching on it — rather than on the Team ID alone — is what keeps a
/// "Developer ID Installer" certificate, which shares the team and would sign a
/// bundle without complaint but cannot notarize one, from ever being chosen.
const DEVID_APP_PREFIX: &str = "Developer ID Application: ";

/// Choose the signing certificate from `security find-identity -v -p codesigning`
/// output, using the committed Team ID as the only selector.
///
/// This is why no identity string has to be invented, committed, or typed on a
/// command line: given the anchor, the certificate is DERIVABLE. `-v` restricts
/// the listing to identities that are valid and have a usable private key, so a
/// match here is a certificate that can actually sign.
///
/// Ambiguity is a hard error rather than a choice. A team routinely ends up with
/// two valid Developer-ID Application certificates at once — a renewal issued
/// before the incumbent expired — and they have the SAME common name, so there
/// is no rule this function could apply that a human would recognise as correct.
/// Picking one silently produces an artifact signed by a certificate nobody
/// chose; the operator resolves it by removing the superseded certificate or by
/// naming the survivor's hash in `signing_identity_sha1`.
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
        // Case-insensitive because an operator copying a hash out of Keychain
        // Access can easily paste it lowercased; the value is a fingerprint, not
        // a string whose case carries meaning.
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
    if sha1.len() != 40 || !sha1.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let common_name = rest.trim().strip_prefix('"')?.strip_suffix('"')?;
    Some(DevIdIdentity {
        sha1: sha1.to_ascii_uppercase(),
        common_name: common_name.to_string(),
    })
}

/// Tier APPLE, resolved. The pipeline holds exactly one of these for the life of
/// a cut, so a cut cannot change identity halfway through.
///
/// `Debug` is derived rather than hand-written, and safely so: the only secret
/// reachable from here is inside [`NotaryAuth`], whose own `Debug` is
/// hand-written precisely to redact it.
#[derive(Debug)]
pub enum AppleTier {
    /// `pins::APPLE_TEAM_ID` is empty. Ad-hoc signing, no notarization, no
    /// claim — what aterm ships today.
    Inactive,
    /// The anchor is set AND everything needed to keep its promise resolved.
    /// Constructing this value is the proof: there is no way to reach the active
    /// path without a certificate in hand and a notarytool credential named.
    Active {
        identity: DevIdIdentity,
        auth: NotaryAuth,
    },
}

impl AppleTier {
    /// The certificate this cut signs with — `None` on the inactive tier, which
    /// is also the "is the tier on?" predicate every caller needs, so there is
    /// deliberately no second `is_active()` beside it to fall out of step.
    #[must_use]
    pub fn identity(&self) -> Option<&DevIdIdentity> {
        match self {
            AppleTier::Active { identity, .. } => Some(identity),
            AppleTier::Inactive => None,
        }
    }

    /// One line for the cut transcript. Never prints the auth, only its kind.
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

/// Resolve the tier BEFORE anything irreversible happens.
///
/// Every failure this can produce is a failure of the machine's setup, not of
/// the build: no certificate, an ambiguous keychain, no notarytool credential.
/// Those must surface at the top of `run_cut`, beside the credentials load and
/// ahead of the ledger claim, because the claim burns a single-use build number
/// — discovering at minute twenty that the keychain is empty must not cost one.
///
/// `team_id` is passed in rather than read here so this module never imports the
/// anchor: `pins.rs` stays the single place the constant is named, and this
/// function stays a pure-ish resolver a test can drive with any value.
///
/// # What the two halves actually prove — they are not equally strong
///
/// The certificate half is strong: `security find-identity -v -p codesigning`
/// lists only identities that are valid AND have a usable private key, so a
/// resolved [`DevIdIdentity`] is a certificate this machine can genuinely sign
/// with, right now.
///
/// The notarytool half is weaker, and deliberately so: it proves only that the
/// profile NAMES a credential, not that Apple will accept it. A profile naming a
/// keychain entry that was never stored, was stored on a different machine, or
/// whose app-specific password has since been revoked resolves happily here and
/// fails minutes later at `notarytool submit`. That is a real gap and this
/// comment is the honest size of it, rather than the impression that "resolved"
/// means "usable".
///
/// It is left open on purpose. Proving USABILITY means asking Apple — a network
/// round trip against a live Developer Program account (`notarytool history`) —
/// which cannot be exercised, or even meaningfully reviewed, from a tree that
/// has no account; adding an unrunnable network probe to the pre-claim path
/// would trade a bounded, well-reported failure for an unbounded, untested one.
/// The cheap offline alternative — probing the login keychain for the item
/// `store-credentials` writes — was considered and rejected: that item's service
/// name and account layout are undocumented and have changed across Xcode
/// versions, so a probe that guessed wrong would REFUSE a cut whose credential
/// is perfectly good, which is a worse failure than the one it prevents.
///
/// The residual cost is bounded and never silent: a named-but-unusable
/// credential fails inside [`notarize`], which propagates, which aborts the cut
/// before anything is published. It costs a build, never a false claim.
pub fn resolve_apple_tier(
    team_id: &str,
    credentials: Option<&ReleaseCredentials>,
) -> Result<AppleTier, String> {
    // Fail-closed by construction: an empty anchor never reaches any of the
    // machinery below, so an unpinned build cannot accidentally sign, submit, or
    // claim anything. This mirrors `pins::anchor_active`, which the caller uses.
    if team_id.is_empty() {
        return Ok(AppleTier::Inactive);
    }
    let credentials = credentials.ok_or_else(|| {
        format!(
            "pins::APPLE_TEAM_ID is set to {team_id}, so every artifact must be Developer-ID \
             signed and notarized — but no --release-credentials profile was given, so there \
             is no notarytool credential to submit with"
        )
    })?;
    let auth = credentials.notary().ok_or_else(|| {
        format!(
            "pins::APPLE_TEAM_ID is set to {team_id}, but the release-credentials profile \
             names no notarytool credential. Run `xcrun notarytool store-credentials <name> \
             --apple-id <id> --team-id {team_id} --password <app-specific-password>` once, then \
             add `notary_profile = \"<name>\"` to the profile"
        )
    })?;
    let listing = Command::new(SECURITY)
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .map_err(|e| format!("spawn {SECURITY} find-identity: {e}"))?;
    // `find-identity` reports "0 valid identities found" on stdout with a
    // SUCCESS status, so the status is not the signal — the parsed listing is,
    // and `select_devid_identity` says exactly which way it came up short.
    let identity = select_devid_identity(
        &String::from_utf8_lossy(&listing.stdout),
        team_id,
        credentials.signing_identity_sha1(),
    )?;
    Ok(AppleTier::Active {
        identity,
        // The team id notarytool is told is the ANCHOR's, always. See
        // `credentials_notary_auth`: the profile cannot express one.
        auth: auth.clone().with_team_id(team_id),
    })
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
/// Ad-hoc path (Tier APPLE inactive): same ONE-Mach-O shape as the Developer-ID
/// branch above, so no `--deep` — there is no nested code for it to reach, and
/// Apple deprecates `--deep` for SIGNING (sign nested code inside-out instead).
/// Dropping it keeps the two branches honest about the one-Mach-O story; it is
/// not a perf fix (re-hashing the ~715 MB seed costs ~0.2 s warm).
/// Runs locally; NOT distributable to other Macs.
pub fn sign_app(
    app: &Path,
    entitlements: &Path,
    sign_id: Option<&DevIdIdentity>,
) -> Result<String, String> {
    let signed_by = match sign_id {
        Some(id) => {
            // ONE Mach-O: nothing nested to sign. The argv0 alias symlinks in
            // Contents/MacOS are resources, sealed by the outer signature.
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
                    // The SHA-1, not the common name: `--sign` substring-matches
                    // its argument against every identity in the keychain, and a
                    // team holding both an Application and an Installer
                    // certificate has two names containing "Developer ID".
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
    // Post-sign verification print (best-effort, like the script's `|| true`);
    // the cut's hard `codesign --verify --deep --strict` gate lives in the
    // publish self-check (spec §7 step 4).
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

/// Sign the DMG (make-dmg.sh's owner path). `--timestamp` embeds a secure
/// timestamp so the signature stays valid after the signing cert expires (the
/// .app inside is already timestamped by [`sign_app`]).
pub fn sign_dmg(dmg: &Path, sign_id: &DevIdIdentity) -> Result<(), String> {
    println!("==> codesign dmg (Developer-ID): {}", sign_id.common_name);
    run_checked(
        Command::new(CODESIGN)
            .args(["--force", "--timestamp", "--sign", &sign_id.sha1])
            .arg(dmg),
        "codesign dmg",
    )
}

// ---------------------------------------------------------------------------
// notarization — the absorbed notarize.sh
// ---------------------------------------------------------------------------

/// notarytool credentials — notarize.sh's two spellings, built by
/// [`credentials_notary_auth`] from the release-credentials profile.
///
/// # Why `KeychainProfile` is the default and the fallback is a fallback
///
/// `--password <secret>` lands in notarytool's ARGV, where it is readable by any
/// local `ps` for the several minutes `submit --wait` runs. `--keychain-profile`
/// puts no secret on a command line, ever, and Apple's own tooling treats
/// `store-credentials` as the supported path. The `AppleId` variant stays
/// reachable because a CI-style machine with no usable login keychain has no
/// other option — but it is documented as the exception, not the shape to reach
/// for.
#[derive(Clone)]
pub enum NotaryAuth {
    KeychainProfile(String),
    AppleId {
        apple_id: String,
        /// ALWAYS `pins::APPLE_TEAM_ID`, stamped in by [`NotaryAuth::with_team_id`].
        /// Never read from the credentials profile — see [`credentials_notary_auth`].
        team_id: String,
        password: String,
    },
}

impl std::fmt::Debug for NotaryAuth {
    /// Hand-written for the same reason [`ReleaseCredentials`]'s is: the
    /// `AppleId` variant holds a live app-specific password, and a derived Debug
    /// would put it in the first transcript line, journal field, or panic
    /// message that formatted a `CutCtx`.
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
    /// Which spelling this is — safe to print, unlike the value itself.
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
    ///
    /// This exists so that the Team ID notarytool receives is provably the
    /// anchor's: there is no setter that takes a team id from anywhere else, and
    /// the profile parser refuses to express one.
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
///
/// And one refusal notarization does not care about, added for macOS privacy
/// consent (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §3.1):
///
///   4. a **cdhash-class designated requirement**. `tccd` stores every access
///      grant beside the client's designated requirement as computed at grant
///      time, and re-checks the running code against it. An identity-class DR
///      (`identifier "…" and anchor apple generic …`) names no bytes, so a
///      grant survives a rebuild and an in-place update; a cdhash-class DR
///      (`cdhash H"…"`, what ad-hoc signing produces even when `--identifier`
///      is passed) pins the exact bytes, so the next build is a different
///      client and the grant is silently dead. Shipping one would mean every
///      release re-asks the human for consent it was already given.
///
/// The DR is not in `codesign -dv --verbose=2` output; [`check_devid_signed`]
/// appends a `codesign -d -r-` probe to the same text, and this function reads
/// whichever `designated =>` line it finds. Only a positively identified
/// [`DrClass::Cdhash`] refuses: text with no requirement in it classifies
/// [`DrClass::Unsigned`] or [`DrClass::Unknown`] and is left alone, because an
/// artifact carrying no requirement at all is already rejected by rule 2's
/// missing Authority line, and the pure fixtures in tests/signconf.rs predate
/// the probe.
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

// ---------------------------------------------------------------------------
// Designated-requirement class (macOS privacy consent, design §3.1)
// ---------------------------------------------------------------------------

/// What a designated requirement is keyed on, and therefore whether a macOS
/// privacy (TCC) access grant recorded against it can outlive a rebuild.
///
/// aterm-release deliberately does NOT depend on `aterm-containment`, where the
/// runtime consent module keeps the same classification: the release cutter is
/// an owner-only binary and its dependency graph is kept minimal on purpose
/// (see this crate's Cargo.toml — every edge there is justified in a comment).
/// The two are therefore held to the SAME variant names and the same
/// precedence, so they can be unified in one move if that edge is ever added.
/// They differ only in what they are handed: `aterm_containment::consent`
/// classifies a requirement STRING, while this one classifies the whole
/// `codesign` output [`check_devid_signed`] already has in hand, and finds the
/// `designated =>` line itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrClass {
    /// `identifier "com.aterm.aterm" and anchor apple generic and …` — names an
    /// identity and pins a certificate, no bytes. A grant against it survives
    /// every rebuild and every in-place update.
    Identity,
    /// The requirement pins a `cdhash` — the shape ad-hoc signing produces (with
    /// no identifier clause, even when `--identifier` is passed). Every rebuild
    /// is a different identity, so the grant is silently dead.
    Cdhash,
    /// The code object is not signed at all.
    Unsigned,
    /// Not recognised, or no requirement in the text at all. Never treated as
    /// stable, and never a refusal on its own.
    #[default]
    Unknown,
}

/// Classify the designated requirement in `codesign -d -r-` output.
///
/// Recorded shapes this is written against (captured on macOS 26.6.2):
///
/// ```text
/// designated => identifier "com.mitchellh.ghostty" and anchor apple generic and …
/// # designated => cdhash H"240c6c45…" or cdhash H"458a3ae8…"
/// code object is not signed at all
/// ```
///
/// The leading `# ` marks a requirement codesign DERIVED rather than read from
/// the signature — ad-hoc signatures always take that path — so it is stripped
/// before matching.
///
/// Precedence matches `aterm_containment::consent::classify_dr` and fails
/// toward "unstable": a `cdhash` clause wins even beside an identifier, because
/// a requirement that pins a code-directory hash is invalidated by the next
/// build whatever else it says.
#[must_use]
pub fn classify_dr(codesign_output: &str) -> DrClass {
    let Some(requirement) = designated_requirement(codesign_output) else {
        // No requirement line. An unsigned object is the one case worth naming;
        // anything else is simply not something this gate can reason about.
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

/// The text after `designated =>` on the one line that carries it, if any.
fn designated_requirement(codesign_output: &str) -> Option<&str> {
    const MARKER: &str = "designated =>";
    codesign_output.lines().find_map(|line| {
        // Strip codesign's derived-requirement comment marker, then require the
        // line to START with the marker: `host => …` and `library => …` lines
        // must not be mistaken for it.
        let line = line.trim_start().trim_start_matches('#').trim_start();
        line.strip_prefix(MARKER).map(str::trim)
    })
}

/// Run the preflight against a real on-disk artifact (`codesign -dv` prints
/// to stderr; unsigned targets only produce an error — both flow into the
/// pure check, which then rejects for the missing Authority).
///
/// Called on the `.app` the moment [`sign_app`] returns, and again inside
/// [`notarize`] on whatever is being submitted. The first call is the one that
/// matters for the hardened-runtime rule: that rule is `is_app`-gated, so a
/// pipeline that only ever preflighted the DMG would leave it as unexercised as
/// it was when nothing called this at all — and would defer a missing
/// `--options runtime` to a rejection from Apple after a full build.
pub fn check_devid_signed(target: &Path) -> Result<(), String> {
    let out = Command::new(CODESIGN)
        .args(["-dv", "--verbose=2"])
        .arg(target)
        .output()
        .map_err(|e| format!("spawn codesign -dv: {e}"))?;
    let mut info = String::from_utf8_lossy(&out.stderr).to_string();
    info.push_str(&String::from_utf8_lossy(&out.stdout));
    // The designated requirement is a SEPARATE probe — `-dv` never prints it —
    // and it is what rule 4 reads. A failure to spawn or a codesign error is
    // left as absent text: the DR rule is written to refuse only a positively
    // identified cdhash requirement, so a missing probe cannot invent a
    // refusal, and rules 1-3 still run on the `-dv` output above.
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
                Command::new(DITTO)
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
        Command::new(XCRUN)
            .args(["notarytool", "submit"])
            .arg(&submit)
            .args(auth.args())
            .arg("--wait"),
        "notarytool submit",
        NOTARY_SUBMIT_TIMEOUT,
    );
    // The zip is a throwaway either way — remove it before propagating.
    if let Some(zip) = cleanup {
        let _ = std::fs::remove_file(zip);
    }
    result?;

    // Stapling is not a nicety. The zip the entire fleet downloads is made from
    // this bundle and is assessed OFFLINE by the client (`spctl -a -t exec` under
    // a fail-closed timeout); without an embedded ticket Gatekeeper has to ask
    // Apple over the network, which is slow when it works and fatal when the
    // user is offline. So a staple failure fails the cut.
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

    // Gatekeeper's own verdict, printed for the record (best-effort — the
    // script `|| true`s this too; notarization already succeeded above).
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

// ---------------------------------------------------------------------------
// The two Tier APPLE hooks, and the seam that makes them testable
// ---------------------------------------------------------------------------

/// EVERY Apple tool spawn whose outcome decides whether a release ships, behind
/// one trait.
///
/// Not an abstraction for its own sake. Two different kinds of call live here
/// for two different reasons:
///
/// * the three IRREVERSIBLE operations ([`AppleTools::sign_dmg`],
///   [`AppleTools::check_devid_signed`], [`AppleTools::notarize`]) need a real
///   certificate, a real Apple ID and minutes of network, so without a seam
///   their ORDER and their fail-closed propagation could only be verified by
///   cutting a real release;
/// * the four VERDICT queries the self-check runs ([`AppleTools::codesign_verify_strict`],
///   [`AppleTools::codesign_dv`], [`AppleTools::stapler_validate`],
///   [`AppleTools::gatekeeper_ok`]) decide whether an already-built artifact is
///   allowed out, so the branch that consults them is exactly as load-bearing as
///   the branch that produces them — and was, until this seam existed, reachable
///   only by having a signed bundle on disk.
///
/// With the seam, one test drives the real decision code with a recording fake
/// and proves, offline and in milliseconds, that the active path really does
/// notarize, that a notarization failure really does abort, and that the
/// self-check really does refuse an unstapled artifact.
///
/// The implementation is also the single place these tools' PATHS are decided:
/// [`RealAppleTools`] spawns each by absolute path, so nothing earlier on `$PATH`
/// can decide what "signed", "stapled" or "Gatekeeper approved" means for a
/// release.
pub trait AppleTools {
    fn sign_dmg(&self, dmg: &Path, id: &DevIdIdentity) -> Result<(), String>;
    fn check_devid_signed(&self, target: &Path) -> Result<(), String>;
    fn notarize(&self, artifact: &Path, auth: &NotaryAuth) -> Result<(), String>;
    /// `codesign --verify --deep --strict` — the cut's HARD signature gate, run
    /// on every tier including the ad-hoc one. `Err` carries the tool's own
    /// complaint, because "what did codesign object to?" is the only useful
    /// thing to print when a release stops here.
    fn codesign_verify_strict(&self, target: &Path) -> Result<(), String>;
    /// `codesign -dv --verbose=2`, stderr and stdout concatenated — codesign
    /// prints this report to stderr, and merging is what keeps the parse
    /// insensitive to which stream a given macOS release chose.
    fn codesign_dv(&self, target: &Path) -> Result<String, String>;
    /// `xcrun stapler validate` — did the artifact come out with the ticket
    /// EMBEDDED, as opposed to merely notarized somewhere in Apple's records.
    fn stapler_validate(&self, target: &Path) -> Result<bool, String>;
    /// `spctl -a` under the assessment this artifact kind actually gets on a
    /// user's Mac. See [`spctl_argv`].
    fn gatekeeper_ok(&self, target: &Path, kind: GatekeeperKind) -> Result<bool, String>;
}

/// Which Gatekeeper assessment an artifact is subject to.
///
/// A DMG is not assessed the way an executable is, and asking the wrong question
/// gets a useless answer rather than an error — which is why the choice is a type
/// rather than a string typed at each call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatekeeperKind {
    /// The `.app` — `-t exec`, the assessment a launched bundle gets.
    App,
    /// The `.dmg` — `-t open --context context:primary-signature`, the
    /// assessment a double-clicked disk image gets. `-t exec` on a DMG does not
    /// apply and passes vacuously, so getting this wrong would silently retire
    /// the DMG half of the self-check.
    Dmg,
}

/// The `spctl` argv for each artifact kind, as a pure function so the argv
/// itself is a tested fact rather than a literal buried in a spawn.
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

/// The real tools. The only implementation the pipeline ever constructs.
pub struct RealAppleTools;

/// Run a verification tool over `target` and report only whether it PASSED.
///
/// A spawn failure is an error rather than `false`, because "the tool is
/// missing" and "the tool said no" are different problems with different fixes,
/// and collapsing them would report a missing `xcrun` as an unstapled artifact —
/// i.e. would let a broken toolchain masquerade as a security finding, or (worse,
/// were the polarity ever flipped) the reverse.
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
        // No status check: `codesign -dv` fails on an unsigned target and its
        // complaint is exactly the evidence the verdict needs — an empty report
        // carries no `TeamIdentifier=`, so the verdict refuses on the same rule
        // it would use for a mismatched one.
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

/// Verify + notarize + staple the `.app`, BEFORE either container is built.
///
/// Returns whether anything was done, so the caller knows whether the bundle's
/// bytes moved under it.
///
/// # Why the app is notarized first, and separately from the DMG
///
/// The DMG is what a human downloads; the ZIP — built from this bundle — is what
/// every self-updating install downloads, and the client verifies the EXTRACTED
/// bundle offline. Notarizing only the DMG therefore produces a perfectly green
/// cut that strands the fleet. Stapling the bundle from the DMG's submission
/// instead of its own is the tempting shortcut, and it usually works, because it
/// relies on Apple having recorded the enclosed app's cdhash and on that ticket
/// being retrievable at staple time. "Usually" is not a property to build a
/// release pipeline on, and the second round trip is minutes against a release
/// that lasts months — so the app is submitted explicitly.
///
/// Sequencing it before `dmg::create` also means the DMG is built around an
/// ALREADY-STAPLED bundle, so the human's artifact carries the ticket twice over.
///
/// `tools` is [`RealAppleTools`] in the pipeline and a recording fake in tests.
/// There is deliberately no convenience wrapper that supplies the real tools for
/// you: a wrapper is a second, untested entry point, and the one that gets called
/// by accident is always the one nothing drives.
pub fn notarize_app(app: &Path, tier: &AppleTier, tools: &dyn AppleTools) -> Result<bool, String> {
    let AppleTier::Active { auth, .. } = tier else {
        // Inactive: the bundle is ad-hoc signed and nothing claims otherwise.
        // Not an error, not a warning, not a printed line — byte-identical to
        // the pipeline before Tier APPLE existed.
        return Ok(false);
    };
    // The verifier runs HERE, on the `.app`, where the hardened-runtime rule is
    // live. Failing now costs seconds; failing after submission costs an Apple
    // round trip on a build that was never going to be accepted.
    tools.check_devid_signed(app)?;
    tools.notarize(app, auth)?;
    Ok(true)
}

/// Sign + notarize + staple the DMG. THE hook the pipeline calls after
/// `dmg::create`.
///
/// Two states, no third:
///
/// * [`AppleTier::Inactive`] → `Ok(false)`, having done nothing at all. The DMG
///   keeps the digest `dmg::create` minted and the release proceeds exactly as
///   it does today.
/// * [`AppleTier::Active`] → Developer-ID sign, verify, notarize, staple, and
///   `Ok(true)`. Every step propagates its error, so a failure anywhere aborts
///   the cut.
///
/// The state this function used to have — sign with Developer-ID, print that it
/// was NOT notarized, and return `Ok(false)` so the release continued — is gone
/// deliberately. The manifest emits `team_id` from the anchor unconditionally,
/// and `tools/install.sh` plus the in-app updater both read a non-empty
/// `team_id` as a promise that the artifact is Developer-ID signed AND
/// notarized. Shipping a signed-but-unnotarized artifact under that manifest is
/// a lie the pipeline tells its own clients, and the clients reject it — after
/// the release is already published. A release must not be able to claim what it
/// did not do, so the only way past this function on the active path is to have
/// actually done it.
///
/// `tools` is injected for the reason [`notarize_app`] gives.
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

// ---------------------------------------------------------------------------
// The self-check's Apple branch, as a pure verdict over captured tool output
// ---------------------------------------------------------------------------

/// What the self-check observed about a finished cut's artifacts.
///
/// Gathered by spawning tools; judged by [`apple_selfcheck_verdict`], which is
/// pure. The split matters because the self-check is the invariant a RESUMED cut
/// re-proves after skipping the build entirely — it is the last thing standing
/// between a half-built artifact and the fleet, so its rules deserve tests that
/// do not need an Apple account to run.
pub struct AppleSelfcheck<'a> {
    /// The manifest's `team_id`, i.e. the anchor this artifact claims.
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

/// Judge a Tier APPLE cut. Called only when the manifest claims a team.
///
/// The two staple checks are the ones that are easy to leave out and expensive
/// to omit. `spctl` PASSES for a notarized-but-unstapled artifact whenever the
/// machine running the check has network, because Gatekeeper silently falls back
/// to an online lookup — so a green `spctl` on the cutting machine says nothing
/// about a customer's offline Mac. Only `stapler validate` proves the ticket is
/// embedded in the bytes that ship.
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
/// (notarytool submit --wait can take minutes; its status lines matter), with a
/// hard deadline.
///
/// The deadline is the difference between a cut that fails and a cut that
/// wedges. This runs while the pipeline holds a release lease, a publisher
/// fence, and a burned build number; `Command::status()` waits forever, so an
/// Apple-side stall would hold all three indefinitely with no operator signal
/// beyond silence. Polling rather than blocking costs one wakeup a second and
/// buys a bounded, reportable failure.
fn run_streamed(cmd: &mut Command, what: &str, timeout: Duration) -> Result<(), String> {
    // stdio stays inherited so notarytool's status lines still stream to the
    // transcript in real time; only the WAIT is ours.
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
            // Kill before reporting: leaving an orphaned notarytool holding a
            // submission open is how the NEXT cut inherits this one's mess.
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

// ---------------------------------------------------------------------------
// Designated-requirement classification, over RECORDED codesign output
// ---------------------------------------------------------------------------

#[cfg(test)]
mod dr_tests {
    use super::{DrClass, classify_dr, devid_preflight};

    /// Captured on macOS 26.6.2 from a Developer-ID signed app in
    /// /Applications. This is the shape a shipped aterm.app must have: an
    /// identifier clause plus an anchor, and no cdhash anywhere.
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

    /// Captured from `codesign --force --sign - <binary>` — what tools/dev-app.sh
    /// produces without $ATERM_SIGN_ID. Note the `# ` comment marker (codesign
    /// DERIVED this requirement; it is not stored in the signature) and the
    /// total absence of an identifier clause.
    const ADHOC_DR: &str = "Executable=/tmp/adhoc-probe\n\
         # designated => cdhash H\"240c6c45749cff92effe2dbe2782cd3ac8bee253\" or \
         cdhash H\"458a3ae87054006e9463429282897dfafa67e862\"\n";

    /// Same, but signed with `--identifier com.aterm.aterm.dev`. The identifier
    /// is NOT reflected into the requirement — this is the measurement behind
    /// design §1.2, and the reason a dev bundle id alone does not buy a durable
    /// grant.
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
        // An identifier with nothing pinning WHO signed it vouches for nothing,
        // so it is Unknown rather than Identity — same precedence as
        // aterm_containment::consent::classify_dr, and it fails toward
        // "unstable" without ever becoming a refusal of its own.
        assert_eq!(
            classify_dr("designated => identifier \"com.aterm.aterm\"\n"),
            DrClass::Unknown
        );
    }

    #[test]
    fn a_byte_pinned_requirement_is_cdhash_even_with_an_identifier() {
        // Explicit `-r` requirements can mix clauses. Anything naming a cdhash
        // dies on the next build, so the identifier clause does not redeem it.
        let mixed = "designated => identifier \"com.aterm.aterm\" and anchor apple generic \
             and cdhash H\"deadbeef\"\n";
        assert_eq!(classify_dr(mixed), DrClass::Cdhash);
    }

    #[test]
    fn only_the_designated_line_is_read() {
        // `host =>` / `library =>` requirements share the file and must not be
        // mistaken for the designated one.
        let other = "host => cdhash H\"deadbeef\"\nlibrary => cdhash H\"feedface\"\n";
        assert_eq!(classify_dr(other), DrClass::Unknown);
        let both = "host => cdhash H\"deadbeef\"\n\
             designated => identifier \"x\" and anchor apple generic\n";
        assert_eq!(classify_dr(both), DrClass::Identity);
    }

    #[test]
    fn preflight_refuses_a_cdhash_designated_requirement() {
        // A Developer-ID .app whose DR somehow came back cdhash-class: every
        // other rule passes, and this one still stops the cut.
        let info = format!(
            "Identifier=com.aterm.aterm\n\
             CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=38+7 location=embedded\n\
             Signature size=8980\n\
             Authority=Developer ID Application: Jane Doe (TEAMID)\n\
             {ADHOC_DR}"
        );
        let err = devid_preflight(&info, true).unwrap_err();
        assert!(err.contains("cdhash-class"), "{err}");
        assert!(
            err.contains("privacy grant"),
            "the refusal must say WHY it matters: {err}"
        );
        // The same requirement on a DMG is refused too: a cdhash DR can only
        // come from ad-hoc signing, which nothing in the release tier may do.
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
        // Rule 4 is a positive test. Output with no probe (the pre-existing
        // fixtures in tests/signconf.rs) must keep its old verdict, and an
        // UNSIGNED artifact is already caught by the Authority rule — with that
        // message, not this one.
        let no_probe = "Identifier=com.aterm.aterm\n\
             CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=38+7 location=embedded\n\
             Signature size=8980\n\
             Authority=Developer ID Application: Jane Doe (TEAMID)\n";
        devid_preflight(no_probe, true).expect("no DR probe in the text is not a refusal");
        let err = devid_preflight(UNSIGNED_DR, true).unwrap_err();
        assert!(err.contains("not Developer-ID signed"), "{err}");
    }
}
