// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! apps/aterm-win/get-aterm.ps1's verifier. PowerShell has no Ed25519, so the bootstrap runs
//! this crate's Windows build, which get-aterm.ps1 pins by SHA-256 as install.sh pins the Linux
//! one ([`crate::pin::WindowsPin`]). It asks the same signed chain ([`verify_manifest`]: the
//! paper master's roster, a live machine's appcast, the release's identity, every Linux triple
//! whole) for the release's Windows portable zip ([`verify_windows_manifest`]), and holds the
//! downloaded zip to the signed SHA-256 and size ([`verify_zip`]) before get-aterm.ps1 hands it
//! to install.ps1.
//!
//! The latest release need not carry a Windows zip: the release pump publishes Mac-only
//! releases with no person present, each GitHub's `latest`. The latest release is always
//! verified first, and its chain is the one that binds: its roster is the one that authorizes,
//! and its revocations and its signed minimum build hold. When its signed appcast carries no zip
//! for the asked architecture the verifier says so ([`NO_ZIP`]), get-aterm.ps1 proposes the
//! newest older release that lists one, and the verifier holds that release to the latest
//! release's chain (`main`): its appcast verified under the latest release's roster, never its
//! own older one, from a machine that roster does not revoke; the release of its tag, older than
//! the latest by version; signing this architecture's zip; and its build not below the latest
//! release's signed minimum build (nor its own). The listing that proposed it decides nothing.
//!
//! No store records a durable policy on Windows yet, so the latest release is asked under the
//! clean slate ([`Policy::default`]): every rule of the chain, no rollback floor.

use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::path::Path;

use crate::chain::{Ask, Policy, Signed, open_file, verify_manifest};
use crate::py::{bare_os_error, equals_str, uint};
use crate::{Refusal, Verdict, require};

/// The Windows architectures a release signs a portable zip for, as get-aterm.ps1 names them.
pub const ARCHES: [&str; 1] = ["x86_64"];

/// The largest portable zip a release may sign: aterm-update-core's `WINDOWS_ZIP_MAX_BYTES`.
pub const MAX_ZIP: u64 = 512 * 1024 * 1024;

/// The command line's whole answer when it was asked about the latest release alone, the latest
/// release's chain holds, and its signed appcast carries no zip for the asked architecture:
/// get-aterm.ps1 then proposes the newest older release that lists one and asks again.
pub const NO_ZIP: &str = "none";

/// The latest release's appcast, beside the proposed release's `aterm-appcast.toml`, when the
/// command line is asked about a release older than the latest (the name install.sh gives the
/// channel head's appcast).
pub const LATEST_APPCAST: &str = "aterm-policy-appcast.toml";

/// The release asset of `arch`'s portable zip in release `version`: aterm-update-core's
/// `WindowsTarget::asset_name`.
#[must_use]
pub fn asset_name(version: &str, arch: &str) -> String {
    format!("aterm-{version}-windows-{arch}.zip")
}

/// One Windows zip a verified appcast signs: its architecture, name, SHA-256 and size.
type Artifact = (&'static str, String, String, u64);

/// [`verify_manifest`] asked for a Windows portable zip: `ask.arch` names a Windows
/// architecture ([`ARCHES`]), or none to ask for the appcast alone. The chain's every rule
/// holds, Linux triples included; then every Windows triple
/// (`windows_<arch>`, `_sha256`, `_size`) is whole and canonical, as aterm-update-core's
/// `Manifest::windows_artifact` reads it, and the asked one is present.
///
/// # Errors
/// The first rule the proofs break.
pub fn verify_windows_manifest(ask: &Ask<'_>, policy: &Policy) -> Verdict<Signed> {
    require(
        ask.arch.is_none_or(|arch| ARCHES.contains(&arch)),
        "unsupported Windows architecture",
    )?;
    let (mut signed, artifacts) = windows_chain(ask, policy)?;
    signed.artifact = match ask.arch {
        None => None,
        Some(arch) => Some(pick(artifacts, arch)?),
    };
    Ok(signed)
}

/// The chain ([`verify_manifest`], asked for no Linux architecture) and every Windows zip the
/// appcast signs, each triple whole and canonical.
fn windows_chain(ask: &Ask<'_>, policy: &Policy) -> Verdict<(Signed, Vec<Artifact>)> {
    let signed = verify_manifest(
        &Ask {
            directory: ask.directory,
            tag: ask.tag,
            slug: ask.slug,
            arch: None,
            manifest_name: ask.manifest_name,
            now: ask.now,
            pins: ask.pins,
        },
        policy,
    )?;
    // verify_manifest admitted the tag as v<MAJOR>.<MINOR>.<PATCH>.
    let version = &ask.tag[1..];
    let mut artifacts = Vec::new();
    for target in ARCHES {
        let field = format!("windows_{target}");
        let present = ["", "_sha256", "_size"]
            .map(|suffix| signed.manifest.get(&format!("{field}{suffix}")).is_some());
        require(
            !present.iter().any(|p| *p) || present.iter().all(|p| *p),
            "incomplete Windows artifact triple",
        )?;
        if !present[0] {
            continue;
        }
        let name = asset_name(version, target);
        require(
            equals_str(signed.manifest.get(&field), &name),
            "noncanonical Windows asset",
        )?;
        let digest = signed
            .manifest
            .get(&format!("{field}_sha256"))
            .and_then(aterm_toml::Value::as_str)
            .filter(|digest| aterm_codec::hex::is_lower(digest, 64))
            .ok_or_else(|| Refusal::new("invalid Windows digest"))?
            .to_string();
        let size = uint(
            signed.manifest.get(&format!("{field}_size")),
            "Windows size",
            MAX_ZIP,
        )?;
        require(size >= 64, "Windows artifact too small")?;
        artifacts.push((target, name, digest, size));
    }
    Ok((signed, artifacts))
}

/// `arch`'s zip among `artifacts`: its name, SHA-256 and size.
fn pick(artifacts: Vec<Artifact>, arch: &str) -> Verdict<(String, String, u64)> {
    artifacts
        .into_iter()
        .find(|(target, ..)| *target == arch)
        .map(|(_, name, digest, size)| (name, digest, size))
        .ok_or_else(|| Refusal::new(format!("signed release has no Windows artifact for {arch}")))
}

/// Hold the file at `path` to the signed `size` and `digest` (64 lowercase hex digits): a
/// regular file of exactly `size` bytes whose SHA-256 is `digest`.
///
/// # Errors
/// The first of those it is not.
pub fn verify_zip(path: &OsStr, digest: &str, size: u64) -> Verdict<()> {
    let (file, meta) = open_file(Path::new(path))?;
    require(meta.len() == size, "Windows artifact size mismatch")?;
    let mut hasher = aterm_digest::Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    let mut read_total: u64 = 0;
    let mut body = file.take(size.saturating_add(1));
    loop {
        let n = body.read(&mut buffer).map_err(|e| bare_os_error(&e))?;
        if n == 0 {
            break;
        }
        read_total = read_total.saturating_add(n as u64);
        require(read_total <= size, "Windows artifact size mismatch")?;
        hasher.update(&buffer[..n]);
    }
    require(read_total == size, "Windows artifact size mismatch")?;
    require(
        aterm_codec::hex::encode_lower(&hasher.finalize()) == digest,
        "Windows artifact SHA-256 mismatch",
    )
}

/// The numbers of a `v<MAJOR>.<MINOR>.<PATCH>` tag, each a canonical decimal within `u64`.
fn version_key(tag: &str) -> Option<[u64; 3]> {
    let mut parts = tag.strip_prefix('v')?.split('.');
    let mut key = [0u64; 3];
    for slot in &mut key {
        let part = parts.next()?;
        let canonical = !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'));
        *slot = part.parse().ok().filter(|_| canonical)?;
    }
    parts.next().is_none().then_some(key)
}

/// Whether nothing is at `path`: an entry that cannot be looked at counts as one that is there.
fn absent(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// get-aterm.ps1's command line (`args` without the program name) under the trust anchor
/// `pins`, at `now` (Unix seconds): `<dir> <latest> <tag> <slug> <arch> [<zip>]`, where
/// `<latest>` is the release the latest-release pointer named and `<tag>` the release to
/// install. `<dir>` holds the latest release's `aterm-machines.toml(.sig)` and `<tag>`'s
/// `aterm-appcast.toml(.sig)`; with `<zip>` the downloaded zip is held to the signed digest and
/// size. Prints the signed zip's `name\tdigest\tsize\tbuild\troster_seq` line.
///
/// With `<tag>` the latest release, [`LATEST_APPCAST`] is absent and the release's chain is
/// asked under the clean slate; when its signed appcast carries no zip for `<arch>` and no
/// `<zip>` is given, the answer is [`NO_ZIP`].
///
/// With `<tag>` an older release, the folder also holds the latest release's appcast as
/// [`LATEST_APPCAST`]`(.sig)`. The latest release is verified first, under the clean slate, and
/// must carry no zip for `<arch>` (else it is the release to install); then `<tag>` must be
/// older than it by version, and its appcast is asked under the latest release's roster (the
/// folder's one), with the latest release's roster sequence, revocations and signed minimum
/// build as the floors, and must sign `<arch>`'s zip. Each refusal names the release it is of.
///
/// # Errors
/// Every refusal [`verify_windows_manifest`] and [`verify_zip`] make, and those above.
pub(crate) fn main(args: &[OsString], now: i64, pins: &[String]) -> Verdict<Option<String>> {
    require(
        args.len() == 5 || args.len() == 6,
        "invalid Windows verifier invocation",
    )?;
    let lossy = |n: usize| args[n].to_string_lossy().into_owned();
    let (latest, tag, slug, arch) = (lossy(1), lossy(2), lossy(3), lossy(4));
    require(
        ARCHES.contains(&arch.as_str()),
        "unsupported Windows architecture",
    )?;
    let directory = args[0].as_os_str();
    let zip = args.get(5);
    let ask = |tag, manifest_name| Ask {
        directory,
        tag,
        slug: &slug,
        arch: None,
        manifest_name,
        now,
        pins,
    };
    let (name, digest, size, signed) = if latest == tag {
        let beside = Path::new(directory);
        require(
            absent(&beside.join(LATEST_APPCAST))
                && absent(&beside.join(format!("{LATEST_APPCAST}.sig"))),
            "a latest-release appcast lies beside the release's own, which is the latest",
        )?;
        let (signed, artifacts) =
            windows_chain(&ask(tag.as_str(), "aterm-appcast.toml"), &Policy::default())?;
        if zip.is_none() && artifacts.iter().all(|(target, ..)| *target != arch) {
            return Ok(Some(NO_ZIP.to_string()));
        }
        let (name, digest, size) = pick(artifacts, &arch)?;
        (name, digest, size, signed)
    } else {
        let of_latest = |why: Refusal| {
            Refusal::new(format!("the latest release {latest} is refused: {}", why.0))
        };
        let (head, head_artifacts) =
            windows_chain(&ask(latest.as_str(), LATEST_APPCAST), &Policy::default())
                .map_err(of_latest)?;
        require(
            head_artifacts.iter().all(|(target, ..)| *target != arch),
            &format!(
                "the latest release {latest} signs a Windows zip for {arch}: it is the release \
                 to install, not {tag}"
            ),
        )?;
        let of_tag = |why: Refusal| {
            Refusal::new(format!(
                "release {tag}, proposed in place of the latest release {latest}, is refused: {}",
                why.0
            ))
        };
        let older = version_key(&tag)
            .zip(version_key(&latest))
            .is_some_and(|(key, latest_key)| key < latest_key);
        if !older {
            return Err(of_tag(Refusal::new(
                "it is not a release older than the latest",
            )));
        }
        // The folder's roster is the latest release's, so the release is asked under it, at its
        // sequence, with its revocations and its signed minimum build as the floors.
        let policy = Policy {
            floors: [0, head.minimum, head.roster_seq],
            revoked_machines: head.roster_revoked.clone(),
            rejected_build: 0,
            min_build_sources: None,
        };
        let (signed, artifacts) =
            windows_chain(&ask(tag.as_str(), "aterm-appcast.toml"), &policy).map_err(of_tag)?;
        let (name, digest, size) = pick(artifacts, &arch).map_err(of_tag)?;
        (name, digest, size, signed)
    };
    if let Some(zip) = zip {
        verify_zip(zip, &digest, size)?;
    }
    Ok(Some(format!(
        "{name}\t{digest}\t{size}\t{}\t{}",
        signed.build, signed.roster_seq
    )))
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
