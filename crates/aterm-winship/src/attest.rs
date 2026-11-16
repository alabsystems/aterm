// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows builder's attestation: an Ed25519 signature, by a builder the release commit
//! pins, over a handoff receipt's exact bytes. It stands in for the zip's SHA-256 a person
//! reads off the Windows host and types on the Mac (`crate::handoff::Receipt::check_carried`)
//! when the pair comes through the pull lane (`crate::pull`) rather than by hand: the receipt
//! names the zip's digest, and only the pinned builder's key signs a receipt.
//!
//! * The pins, [`crate::pull::BUILDERS_FILE`], read from the release commit ([`Builders`]):
//!   `[[builder]]` tables of `id`, `public_key` (64 lowercase hex digits, the raw Ed25519 key)
//!   and `not_after` (`YYYY-MM-DD`, the last UTC day it is admitted). No builder pinned: the
//!   pull lane admits nothing.
//! * The attestation, `<zip>.handoff.sig` ([`crate::pull::attestation_name`]) beside the
//!   receipt ([`Attestation`]): `builder` and `signature` (128 lowercase hex digits) over
//!   [`crate::pull::ATTESTATION_DOMAIN`] followed by the receipt file's bytes ([`message`]).
//! * The key, a PKCS#8 Ed25519 document on the builder (`release-key init`,
//!   [`init_key`]), made once and never overwritten; nothing here prints it.
//!
//! A signature is checked by `astream_aead::signature::verify`, which refuses a small-order
//! key or commitment (clippy.toml: strict Ed25519).

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pull::{self, ATTESTATION_DOMAIN};

/// The largest key file [`read_key`] reads: a PKCS#8 Ed25519 document is under a hundred bytes.
pub const KEY_MAX_BYTES: u64 = 1024;

/// How many days `release-key` pins a key for: its suggested `not_after` is today plus these.
/// Anyone who reads the key file before then can attest a zip a pump release ships with no
/// person looking, so a pin is short and a key is replaced, never extended.
pub const PIN_DAYS: u64 = 90;

/// What an attestation is refused for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The pins, the attestation or a key does not read.
    Malformed,
    /// It names a builder the release commit does not pin.
    UnknownBuilder,
    /// The builder's `not_after` has passed.
    Expired,
    /// The signature is not the pinned key's over this receipt.
    BadSignature,
}

impl Fault {
    /// The fault, in a word or two.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::UnknownBuilder => "unknown builder",
            Self::Expired => "expired builder",
            Self::BadSignature => "BAD SIGNATURE",
        }
    }
}

/// An attestation refused: the fault and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// What kind of refusal.
    pub fault: Fault,
    /// Why, for a person.
    pub why: String,
}

impl Refused {
    fn new(fault: Fault, why: impl Into<String>) -> Self {
        Self {
            fault,
            why: why.into(),
        }
    }

    fn malformed(why: impl Into<String>) -> Self {
        Self::new(Fault::Malformed, why)
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.fault.word(), self.why)
    }
}

/// A builder id: 1 to 64 of lowercase ASCII letters, digits, `.`, `-` and `_`, starting with a
/// letter or digit (a host name, `m6-hp-32gb-1tb`).
///
/// # Errors
/// An id that is not that.
pub fn check_id(id: &str) -> Result<(), String> {
    let ok = (1..=64).contains(&id.len())
        && id
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "the builder id {id:?} is not 1 to 64 lowercase letters, digits, '.', '-' and '_', \
             starting with a letter or digit"
        ))
    }
}

/// One pinned builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Builder {
    /// Its id, [`check_id`].
    pub id: String,
    /// Its raw Ed25519 public key, 64 lowercase hex digits.
    pub public_key: String,
    /// The last UTC day its attestation is admitted, `YYYY-MM-DD`.
    pub not_after: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PinsFile {
    #[serde(default)]
    builder: Vec<Builder>,
}

/// The builders a release commit pins ([`crate::pull::BUILDERS_FILE`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Builders(Vec<Builder>);

fn raw_key(hex: &str) -> Option<[u8; 32]> {
    let mut raw = [0_u8; 32];
    (aterm_codec::hex::is_lower(hex, 64) && aterm_codec::hex::decode_into(hex.as_bytes(), &mut raw))
        .then_some(raw)
}

impl Builders {
    /// The builders `text` pins: every id well formed ([`check_id`]) and pinned once, every key
    /// 64 lowercase hex digits and pinned once, every `not_after` a real date.
    ///
    /// # Errors
    /// Text that is not that, as [`Fault::Malformed`].
    pub fn parse(text: &str) -> Result<Self, Refused> {
        let file: PinsFile = aterm_toml::from_str(text).map_err(|e| {
            Refused::malformed(format!("{} does not read: {e}", pull::BUILDERS_FILE))
        })?;
        let bad = |why: String| Refused::malformed(format!("{}: {why}", pull::BUILDERS_FILE));
        for (index, builder) in file.builder.iter().enumerate() {
            check_id(&builder.id).map_err(bad)?;
            if raw_key(&builder.public_key).is_none() {
                return Err(bad(format!(
                    "builder {}'s public_key is not 64 lowercase hex digits",
                    builder.id
                )));
            }
            if pull::date_days(&builder.not_after).is_none() {
                return Err(bad(format!(
                    "builder {}'s not_after {:?} is not a date (YYYY-MM-DD)",
                    builder.id, builder.not_after
                )));
            }
            if let Some(earlier) = file.builder[..index]
                .iter()
                .find(|b| b.id == builder.id || b.public_key == builder.public_key)
            {
                return Err(bad(format!(
                    "builder {} pins the id or the key builder {} already pins",
                    builder.id, earlier.id
                )));
            }
        }
        Ok(Self(file.builder))
    }

    /// The pinned builders, in the file's order.
    #[must_use]
    pub fn all(&self) -> &[Builder] {
        &self.0
    }

    /// The builder pinned as `id`.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&Builder> {
        self.0.iter().find(|builder| builder.id == id)
    }

    /// `id` is pinned with `public_key` and not expired on `today` (`YYYY-MM-DD`): what a
    /// builder checks before it serves.
    ///
    /// # Errors
    /// Not pinned, pinned with another key, or expired.
    pub fn admits(&self, id: &str, public_key: &str, today: &str) -> Result<&Builder, Refused> {
        let builder = self.current(id, today)?;
        if builder.public_key != public_key {
            return Err(Refused::new(
                Fault::UnknownBuilder,
                format!(
                    "{} pins builder {id} with the key {}, not this key {public_key}",
                    pull::BUILDERS_FILE,
                    builder.public_key
                ),
            ));
        }
        Ok(builder)
    }

    /// The builder pinned as `id`, when it is not expired on `today` (`YYYY-MM-DD`): one whose
    /// attestation a release of these pins would admit today.
    ///
    /// # Errors
    /// Not pinned ([`Fault::UnknownBuilder`]), expired ([`Fault::Expired`]), or a date that
    /// is none ([`Fault::Malformed`]).
    pub fn current(&self, id: &str, today: &str) -> Result<&Builder, Refused> {
        let builder = self.find(id).ok_or_else(|| {
            Refused::new(
                Fault::UnknownBuilder,
                format!(
                    "{} pins no builder {id:?} (it pins {})",
                    pull::BUILDERS_FILE,
                    if self.0.is_empty() {
                        "none".to_string()
                    } else {
                        self.0
                            .iter()
                            .map(|b| b.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ),
            )
        })?;
        let (Some(now), Some(last)) = (pull::date_days(today), pull::date_days(&builder.not_after))
        else {
            return Err(Refused::malformed(format!(
                "today {today:?} or builder {id}'s not_after {:?} is not a date",
                builder.not_after
            )));
        };
        if now > last {
            return Err(Refused::new(
                Fault::Expired,
                format!(
                    "builder {id} was admitted through {}, and today is {today}",
                    builder.not_after
                ),
            ));
        }
        Ok(builder)
    }
}

/// The attestation file beside a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    /// The builder that signed, as pinned.
    pub builder: String,
    /// The Ed25519 signature over [`message`], 128 lowercase hex digits.
    pub signature: String,
}

impl Attestation {
    /// The attestation `text` says.
    ///
    /// # Errors
    /// Text that is not one, as [`Fault::Malformed`].
    pub fn parse(text: &str) -> Result<Self, Refused> {
        if text.len() as u64 > pull::RECORD_MAX_BYTES {
            return Err(Refused::malformed("the attestation is oversized"));
        }
        let attestation: Self = aterm_toml::from_str(text)
            .map_err(|e| Refused::malformed(format!("the attestation does not read: {e}")))?;
        check_id(&attestation.builder).map_err(Refused::malformed)?;
        if !aterm_codec::hex::is_lower(&attestation.signature, 128) {
            return Err(Refused::malformed(
                "the attestation's signature is not 128 lowercase hex digits",
            ));
        }
        Ok(attestation)
    }

    /// The attestation as its file's TOML.
    ///
    /// # Errors
    /// One the serializer refuses.
    pub fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| format!("the attestation: {e}"))
    }
}

/// What an attestation signs: [`ATTESTATION_DOMAIN`], then the receipt file's exact bytes.
#[must_use]
pub fn message(receipt: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ATTESTATION_DOMAIN.len() + receipt.len());
    out.extend_from_slice(ATTESTATION_DOMAIN);
    out.extend_from_slice(receipt);
    out
}

fn pair(pkcs8: &[u8]) -> Result<ring::signature::Ed25519KeyPair, String> {
    ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8)
        .map_err(|_| "the builder key is not a PKCS#8 Ed25519 key".to_string())
}

/// A new PKCS#8 Ed25519 key, from the system's random source. The secret: write it where
/// only its owner reads, and never print it.
///
/// # Errors
/// The random source failed.
pub fn generate() -> Result<Vec<u8>, String> {
    let rng = ring::rand::SystemRandom::new();
    ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
        .map(|doc| doc.as_ref().to_vec())
        .map_err(|_| "key generation failed".to_string())
}

/// The raw public key of the PKCS#8 key `pkcs8`, as 64 lowercase hex digits: what
/// [`crate::pull::BUILDERS_FILE`] pins.
///
/// # Errors
/// A key that is not a PKCS#8 Ed25519 key.
pub fn public_key(pkcs8: &[u8]) -> Result<String, String> {
    use ring::signature::KeyPair as _;
    Ok(aterm_codec::hex::encode_lower(
        pair(pkcs8)?.public_key().as_ref(),
    ))
}

/// `builder`'s attestation of the receipt file whose exact bytes are `receipt`, signed with
/// the PKCS#8 key `pkcs8`.
///
/// # Errors
/// A builder id that is not one, or a key that is not a PKCS#8 Ed25519 key.
pub fn sign(pkcs8: &[u8], builder: &str, receipt: &[u8]) -> Result<Attestation, String> {
    check_id(builder)?;
    let signature = pair(pkcs8)?.sign(&message(receipt));
    Ok(Attestation {
        builder: builder.to_string(),
        signature: aterm_codec::hex::encode_lower(signature.as_ref()),
    })
}

/// The attestation is `public_key`'s (64 lowercase hex digits) over `receipt`.
///
/// # Errors
/// A key or signature that does not decode ([`Fault::Malformed`]), or a signature that is not
/// that key's over that receipt ([`Fault::BadSignature`]).
pub fn verify_with(
    public_key: &str,
    receipt: &[u8],
    attestation: &Attestation,
) -> Result<(), Refused> {
    let key = raw_key(public_key)
        .ok_or_else(|| Refused::malformed("the public key is not 64 lowercase hex digits"))?;
    let mut signature = [0_u8; 64];
    if !aterm_codec::hex::is_lower(&attestation.signature, 128)
        || !aterm_codec::hex::decode_into(attestation.signature.as_bytes(), &mut signature)
    {
        return Err(Refused::malformed(
            "the attestation's signature is not 128 lowercase hex digits",
        ));
    }
    if astream_aead::signature::verify(&key, &message(receipt), &signature) {
        Ok(())
    } else {
        Err(Refused::new(
            Fault::BadSignature,
            format!(
                "the attestation is not builder {}'s signature (key {public_key}) over this \
                 receipt",
                attestation.builder
            ),
        ))
    }
}

/// Who attested a receipt, as [`verify`] proved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attested {
    /// The builder's id.
    pub builder: String,
    /// Its pinned public key.
    pub public_key: String,
}

/// The attestation is a signature over `receipt` (the receipt file's exact bytes) by a builder
/// `builders` pins and that is not expired on `today` (`YYYY-MM-DD`, UTC).
///
/// # Errors
/// [`Fault::UnknownBuilder`], [`Fault::Expired`], [`Fault::BadSignature`], or
/// [`Fault::Malformed`] for what does not decode.
pub fn verify(
    builders: &Builders,
    today: &str,
    receipt: &[u8],
    attestation: &Attestation,
) -> Result<Attested, Refused> {
    let builder = builders.current(&attestation.builder, today)?;
    verify_with(&builder.public_key, receipt, attestation)?;
    Ok(Attested {
        builder: builder.id.clone(),
        public_key: builder.public_key.clone(),
    })
}

/// [`verify`] of the receipt and attestation of the zip `asset` in `directory`, read from
/// their files (`<asset>.handoff.toml` and `<asset>.handoff.sig`).
///
/// # Errors
/// A file that cannot be read ([`Fault::Malformed`]), and what [`verify`] refuses.
pub fn verify_beside(
    directory: &Path,
    asset: &str,
    builders: &Builders,
    today: &str,
) -> Result<Attested, Refused> {
    let receipt = pull::read_record(&directory.join(crate::handoff::receipt_name(asset)))
        .map_err(Refused::malformed)?;
    let text = pull::read_record(&directory.join(pull::attestation_name(asset)))
        .map_err(Refused::malformed)?;
    let text =
        String::from_utf8(text).map_err(|_| Refused::malformed("the attestation is not UTF-8"))?;
    verify(builders, today, &receipt, &Attestation::parse(&text)?)
}

/// Signs the receipt of the zip `asset` in `directory` as `builder` with the PKCS#8 key
/// `pkcs8`, writes the attestation beside it (a new file, never one already there) and checks
/// it against the key. Its path.
///
/// # Errors
/// A receipt that cannot be read, a key or id that is not one, a write that fails.
pub fn write_beside(
    directory: &Path,
    asset: &str,
    pkcs8: &[u8],
    builder: &str,
) -> Result<PathBuf, String> {
    let receipt = pull::read_record(&directory.join(crate::handoff::receipt_name(asset)))?;
    let attestation = sign(pkcs8, builder, &receipt)?;
    verify_with(&public_key(pkcs8)?, &receipt, &attestation).map_err(|e| e.to_string())?;
    let path = directory.join(pull::attestation_name(asset));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(attestation.to_toml()?.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The key `release-key`, `release-build --sign-with` and `release-serve` read when none is
/// named: `%USERPROFILE%\.aterm\windows-builder\ed25519.pk8` of `env`.
#[must_use]
pub fn default_key(env: &[(String, String)]) -> Option<PathBuf> {
    crate::env::lookup(env, "USERPROFILE")
        .filter(|home| Path::new(home).is_absolute())
        .map(|home| {
            Path::new(home)
                .join(".aterm")
                .join("windows-builder")
                .join("ed25519.pk8")
        })
}

/// The builder id used when none is named: this computer's name (`COMPUTERNAME` of `env`),
/// lowercased.
///
/// # Errors
/// No computer name, or one that is no builder id ([`check_id`]).
pub fn default_builder(env: &[(String, String)]) -> Result<String, String> {
    let name = crate::env::lookup(env, "COMPUTERNAME")
        .filter(|name| !name.is_empty())
        .ok_or("the environment names no COMPUTERNAME: name the builder with --builder <id>")?
        .to_ascii_lowercase();
    check_id(&name).map_err(|e| format!("{e}: name the builder with --builder <id>"))?;
    Ok(name)
}

/// The PKCS#8 key at `path`: a regular file of at most [`KEY_MAX_BYTES`] that is an Ed25519
/// key.
///
/// # Errors
/// Any of those it is not.
pub fn read_key(path: &Path) -> Result<Vec<u8>, String> {
    let (_, _, mut file) = crate::handoff::hash_regular(path, KEY_MAX_BYTES)?;
    use std::io::{Read as _, Seek as _};
    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut key = Vec::new();
    file.read_to_end(&mut key)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    public_key(&key).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(key)
}

/// Makes a new key at `path` when there is none: written whole beside it, then linked to its
/// name, which never replaces a file already there (two runs racing keep one key). `Ok(true)`:
/// this run made it; `Ok(false)`: a key was there and is kept. Either way the key at `path` is
/// one ([`read_key`]).
///
/// # Errors
/// The folder or the file cannot be made, or what is at `path` is not a key.
pub fn init_key(path: &Path) -> Result<bool, String> {
    if std::fs::symlink_metadata(path).is_ok() {
        read_key(path)?;
        return Ok(false);
    }
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no folder", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let key = generate()?;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{}.new", std::process::id()));
    let fresh = PathBuf::from(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // Readable by its owner alone: on Windows the profile folder's own access list does that
    // (its account, SYSTEM and elevated administrators). Any code that runs as that account
    // reads it too, which is why the key belongs to an account that runs release-serve and
    // nothing else (docs/RELEASING.md, *The pull lane*).
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&fresh)
        .and_then(|mut file| file.write_all(&key))
        .map_err(|e| format!("{}: {e}", fresh.display()));
    let linked = written.and_then(|()| {
        std::fs::hard_link(&fresh, path).map_err(|e| format!("{}: {e}", path.display()))
    });
    let _ = std::fs::remove_file(&fresh);
    match linked {
        Ok(()) => Ok(true),
        Err(_) if std::fs::symlink_metadata(path).is_ok() => {
            read_key(path)?;
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[path = "attest_tests.rs"]
mod tests;
