// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified ship pin-verifier --from-release vX.Y.Z`: install.sh's bootstrap
//! verifier pin read back from the public release, from any checkout of main.
//!
//! The pin is install.sh's trust root for the Linux verifier: once it is committed, install.sh
//! runs whatever file hashes to it. So this path believes nothing GitHub says on its own. What
//! it trusts, in order:
//!
//! 1. this checkout's paper master keys (`pins::PAPER_MASTER_PUBKEYS`), the anchor every client
//!    compiles in;
//! 2. the channel head's machine roster (`aterm-machines.toml` on `releases/latest`), only once
//!    it verifies under those keys and is still inside its freshness window, the roster every
//!    client admits a Linux download under;
//! 3. the head's current appcast and the release's appcast (`aterm-appcast.toml`, or for a
//!    release behind the head its archived `aterm-appcast-<tag>.toml` pair), each only under the
//!    rules install.sh's `verify_manifest` holds one to (`site::linux::signed_release_appcast`):
//!    its detached signature verifies under a live, unrevoked machine of that roster, it names
//!    that machine and a roster generation no newer, its version is its tag's, and it names
//!    that release of this channel as its origin. The head's gives the
//!    channel's apply floor (`min_build`), which the release's build must reach, as a client
//!    refuses to install one below it (a yank raises it); the release's gives the build, the
//!    commit and each verifier's name, SHA-256, size and interface
//!    ([`super::Handoff::stamp`]);
//! 4. this checkout's RELEASES.ledger claim `<build> <version>` and its tag's commit, which must
//!    be the build and commit the appcast signs;
//! 5. the verifier files GitHub serves, only as the bytes that hash to those signed digests:
//!    each downloaded by its exact asset id, bracketed by two reads of its identity, into a
//!    fresh private directory, then held to its signed size and to its architecture's static
//!    ELF.
//!
//! It pins the architectures the appcast signs, as the cut's sign step records them
//! ([`super::Handoff::record`]): one a worker could not build stays on install.sh's embedded
//! verifier, as it does from the record.
//!
//! What it does not trust: the release listing's names and sizes (each is checked against the
//! signed values), and any unsigned digest. The release publishes no `.sha256` sidecar for a
//! verifier (the sidecars cover the macOS containers), and `SHA256SUMS` is the publication
//! engine's signed manifest of the public source tree, not of the release's binaries, so
//! neither can bind a verifier to the release signer; the signed appcast is the published
//! digest. A release cut before the appcast carried the verifier fields (every release up to
//! and including v0.144.0) signs no digest for its verifiers, and is refused.
//!
//! The `<record>` path ([`super::pin_verifier`]) trusts the cutting machine's `dist/` record
//! instead: the receipts its own sign step froze from the files it then uploaded, with no
//! signature over them.
//!
//! `--windows --from-release vX.Y.Z` ([`windows_pin_from_release`]) reads get-aterm.ps1's pin
//! by the same steps 1 to 4, from the `windows_verifier_*` fields the release's appcast signs
//! ([`crate::windows_pin::stamp`]), and holds the served Windows verifier to them and to an
//! x86-64 console executable in step 5. It is the only way that pin is written: no unsigned
//! record writes it ([`crate::windows_pin`] says why).

use std::path::Path;

use aterm_bootstrap_verify::pin::{self, Pin};
use aterm_json::Value;
use aterm_update_core::{Manifest, Roster, verify_roster};

use crate::ledger::GitRunner;

/// The roster pair and the appcast, by the names a release publishes them under.
const ROSTER_ASSET: &str = aterm_update_core::roster::ROSTER_ASSET;
const ROSTER_SIG_ASSET: &str = aterm_update_core::roster::ROSTER_SIG_ASSET;
const APPCAST: &str = crate::manifest_out::MANIFEST_ASSET;

/// What a `--from-release` read is held to.
pub struct Ask<'a> {
    /// The public channel (`owner/name`) the pin names and the release is read from.
    pub slug: &'a str,
    /// `vX.Y.Z`.
    pub tag: &'a str,
    /// The paper master keyset the roster must verify under.
    pub masters: &'a [&'a str],
    /// Unix seconds the roster and machines are judged at.
    pub now_unix: i64,
    /// Where the fresh, private download directory is made (and removed again).
    pub scratch: &'a Path,
}

/// `targo --unverified ship pin-verifier --from-release <tag>`: read `tag`'s verifiers from
/// the public channel this checkout's Cargo.toml names, as [`read_record`] does, and write the
/// pin and its record into the checkout at `root` ([`super::write_pin`]). The pin it wrote.
///
/// # Errors
/// As [`read_record`] and [`super::write_pin`]: nothing is written.
pub fn pin_from_release(root: &Path, tag: &str) -> Result<Pin, String> {
    let slug = crate::publish::workspace_channel_slug(root).map_err(|e| e.to_string())?;
    let ask = Ask {
        slug: &slug,
        tag,
        masters: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        now_unix: aterm_update_core::roster::now_unix(),
        scratch: &std::env::temp_dir(),
    };
    let record = read_record(root, &crate::ledger::GitCli::new(root), &ask)?;
    super::write_pin(root, &record)
}

/// The [`pin::Record`] of `ask.tag`'s verifiers, filled as the cut's sign step fills it
/// ([`super::Handoff::record`]), from what the public release signs (the module's list says
/// what is trusted). `git` answers for the checkout at `root`.
///
/// # Errors
/// A tag that is not canonical, or that RELEASES.ledger does not claim; a tag this checkout
/// does not have; a release that is missing, a draft, a prerelease or newer than the channel
/// head; a roster, or the head's or the release's appcast, that does not verify; a release
/// below the head's apply floor; an appcast whose build or commit is not the claim's; an
/// appcast that signs no verifier; a verifier asset the release lacks, lists at another size,
/// or serves as other bytes; or a verifier that is not its architecture's static executable.
pub fn read_record(root: &Path, git: &dyn GitRunner, ask: &Ask<'_>) -> Result<pin::Record, String> {
    let Signed {
        ledger,
        build,
        commit,
        release,
        manifest,
    } = signed_release(root, git, ask)?;
    let Some((receipts, interface)) = super::signed_verifiers(&manifest)? else {
        return Err(format!(
            "{}'s signed appcast names no bootstrap verifier, so nothing the release signer \
             signed binds a verifier asset to it (a release cut before the appcast carried \
             the verifier fields, or one cut without them). Pin from a release that signs \
             them, or from its cut's dist/{}",
            ask.tag,
            super::PIN_RECORD
        ));
    };
    let scratch = download_directory(ask)?;
    for receipt in &receipts {
        let at = fetch(ask, &release, scratch.path(), receipt)?;
        let target = super::TARGETS
            .into_iter()
            .find(|target| target.arch() == receipt.arch)
            .ok_or_else(|| format!("no Linux target is named {:?}", receipt.arch))?;
        super::static_verifier_file(&at, target, receipt.size, &receipt.sha256)?;
    }
    let record = pin::Record {
        repo: ask.slug.to_string(),
        tag: ask.tag.to_string(),
        build_number: build,
        commit,
        interface,
        verifiers: receipts,
    };
    record.validate()?;
    record.claimed_in(&ledger)?;
    drop(scratch);
    Ok(record)
}

/// `targo --unverified ship pin-verifier --windows --from-release <tag>`: read `tag`'s Windows
/// verifier from the public channel this checkout's Cargo.toml names, as
/// [`read_windows_record`] does, and write get-aterm.ps1's pin and its record into the
/// checkout at `root` ([`crate::windows_pin::write_windows_pin`]). The pin it wrote.
///
/// # Errors
/// As [`read_windows_record`] and [`crate::windows_pin::write_windows_pin`]: nothing is
/// written.
pub fn windows_pin_from_release(root: &Path, tag: &str) -> Result<pin::WindowsPin, String> {
    let slug = crate::publish::workspace_channel_slug(root).map_err(|e| e.to_string())?;
    let ask = Ask {
        slug: &slug,
        tag,
        masters: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        now_unix: aterm_update_core::roster::now_unix(),
        scratch: &std::env::temp_dir(),
    };
    let record = read_windows_record(root, &crate::ledger::GitCli::new(root), &ask)?;
    crate::windows_pin::write_windows_pin(root, &record)
}

/// The [`pin::WindowsRecord`] of `ask.tag`'s Windows verifier, admitted as [`read_record`]
/// admits the Linux ones (the module's list says what is trusted), from the
/// `windows_verifier_*` fields the release's appcast signs
/// ([`crate::windows_pin::signed_windows_verifiers`]), the served file held to them and to an
/// x86-64 console executable ([`crate::windows_pin::windows_verifier_file`]). This is the only
/// way get-aterm.ps1's pin is written: no unsigned record writes it.
///
/// # Errors
/// As [`read_record`], for the Windows verifier: among them a release whose appcast signs no
/// Windows verifier.
pub fn read_windows_record(
    root: &Path,
    git: &dyn GitRunner,
    ask: &Ask<'_>,
) -> Result<pin::WindowsRecord, String> {
    let Signed {
        ledger,
        build,
        commit,
        release,
        manifest,
    } = signed_release(root, git, ask)?;
    let Some((receipts, interface)) = crate::windows_pin::signed_windows_verifiers(&manifest)?
    else {
        return Err(format!(
            "{}'s signed appcast names no Windows bootstrap verifier, so nothing the release \
             signer signed binds one to it (a release cut before the appcast carried the \
             windows_verifier fields, or one cut without a Windows verifier). Pin from a \
             release that signs one",
            ask.tag
        ));
    };
    let scratch = download_directory(ask)?;
    for receipt in &receipts {
        let at = fetch(ask, &release, scratch.path(), receipt)?;
        crate::windows_pin::windows_verifier_file(&at, receipt.size, &receipt.sha256)?;
    }
    let record = pin::WindowsRecord(pin::Record {
        repo: ask.slug.to_string(),
        tag: ask.tag.to_string(),
        build_number: build,
        commit,
        interface,
        verifiers: receipts,
    });
    record.validate()?;
    record.0.claimed_in(&ledger)?;
    drop(scratch);
    Ok(record)
}

/// What a public release signs, once admitted: this checkout's claim (its RELEASES.ledger text,
/// build and tag commit), the release as GitHub lists it, and its appcast.
struct Signed {
    ledger: String,
    build: u64,
    commit: String,
    release: Release,
    manifest: Manifest,
}

/// `ask.tag`'s release and its appcast, admitted by the module's list of trust (1 to 4): a
/// canonical tag this checkout claims and has, a published release no newer than the channel
/// head, the head's roster under the paper master, the head's and the release's appcasts under
/// that roster, the release at or above the head's apply floor, and the appcast's version,
/// build and commit the claim's.
fn signed_release(root: &Path, git: &dyn GitRunner, ask: &Ask<'_>) -> Result<Signed, String> {
    let version = crate::publish::canonical_channel_tag_version(ask.tag)
        .map_err(|_| format!("{:?} is not a release tag (vMAJOR.MINOR.PATCH)", ask.tag))?;
    let ledger_path = root.join("RELEASES.ledger");
    let ledger = crate::durable::read_text(&ledger_path)
        .map_err(|e| format!("{}: {e}", ledger_path.display()))?;
    let build = claimed_build(&ledger, &version)?;
    let commit = tag_commit(git, ask.tag)?;

    let release = public_release(ask.slug, ask.tag)?;
    let head = head_release(ask.slug)?;
    if version_order(&release.tag)? > version_order(&head.tag)? {
        return Err(format!(
            "{} is newer than the channel head {} on {}: pin a release the channel serves",
            ask.tag, head.tag, ask.slug
        ));
    }
    let roster = head_roster(ask, &head)?;
    let current = signed_appcast(ask, &head, &roster, Proof::Policy)?;
    let manifest = if release.tag == head.tag {
        current.clone()
    } else {
        signed_appcast(ask, &release, &roster, Proof::Release)?
    };
    // A manifest's own floor never exceeds its build (`Manifest::parse`), so the head's is the
    // channel's: the one a yank raises past the withdrawn build.
    let floor = current.min_build.unwrap_or(0);
    if manifest.build_number < floor {
        return Err(format!(
            "{} is build {}, below the apply floor {floor} the channel head {}'s signed appcast \
             sets: no client installs it (a yank or rollback withdrew it), so pin a release the \
             channel still serves",
            ask.tag, manifest.build_number, head.tag
        ));
    }
    if manifest.version != version
        || manifest.build_number != build
        || manifest.commit.as_deref() != Some(commit.as_str())
    {
        return Err(format!(
            "{}'s signed appcast is v{} build {} from {:?}, not the claim this checkout holds \
             (RELEASES.ledger `{build} {version}`, tag {} at {commit})",
            ask.tag, manifest.version, manifest.build_number, manifest.commit, ask.tag
        ));
    }
    Ok(Signed {
        ledger,
        build,
        commit,
        release,
        manifest,
    })
}

/// A fresh private directory under `ask.scratch` for the verifiers' downloads, removed when
/// dropped.
fn download_directory(ask: &Ask<'_>) -> Result<aterm_tempfile::TempDir, String> {
    aterm_tempfile::Builder::new()
        .prefix("aterm-pin-verifier-")
        .suffix(".noindex")
        .tempdir_in(ask.scratch)
        .map_err(|e| format!("make a private download directory: {e}"))
}

/// `receipt`'s asset of `release` downloaded into `scratch` by its exact asset id, and held to
/// the signed size and digest: where it is.
fn fetch(
    ask: &Ask<'_>,
    release: &Release,
    scratch: &Path,
    receipt: &pin::Receipt,
) -> Result<std::path::PathBuf, String> {
    let listed = release.assets.iter().find(|a| a.name == receipt.asset);
    match listed {
        None => {
            return Err(format!(
                "{} lacks its {} bootstrap verifier asset {} (the signed appcast names it)",
                ask.tag, receipt.arch, receipt.asset
            ));
        }
        Some(asset) if asset.size != receipt.size => {
            return Err(format!(
                "{} lists {} at {} bytes, and its signed appcast says {}",
                ask.tag, receipt.asset, asset.size, receipt.size
            ));
        }
        Some(_) => {}
    }
    let at = scratch.join(&receipt.asset);
    let fetched = crate::publish::verify_release_asset_digest_for_release_id_to(
        ask.slug,
        release.id,
        ask.tag,
        &receipt.asset,
        &receipt.sha256,
        &at,
    )
    .map_err(|e| {
        format!(
            "{}'s {} is not the file its signed appcast names: {e}",
            ask.tag, receipt.asset
        )
    })?;
    if fetched.size != receipt.size || fetched.sha256 != receipt.sha256 {
        return Err(format!(
            "{}'s {} downloaded as {} bytes of SHA-256 {}, and its signed appcast says {} \
             bytes of {}",
            ask.tag, receipt.asset, fetched.size, fetched.sha256, receipt.size, receipt.sha256
        ));
    }
    Ok(at)
}

/// The one build RELEASES.ledger (`ledger`, its text) claims for `version`.
fn claimed_build(ledger: &str, version: &str) -> Result<u64, String> {
    let claims: Vec<u64> = crate::ledger::parse(ledger)
        .map_err(|e| format!("RELEASES.ledger: {e}"))?
        .into_iter()
        .filter(|record| record.version == version)
        .map(|record| record.build)
        .collect();
    match claims.as_slice() {
        [build] => Ok(*build),
        [] => Err(format!(
            "RELEASES.ledger records no claim for {version}: this checkout never claimed that \
             release (pull main into it first)"
        )),
        _ => Err(format!(
            "RELEASES.ledger claims {version} {} times",
            claims.len()
        )),
    }
}

/// The commit this checkout's tag `tag` names.
fn tag_commit(git: &dyn GitRunner, tag: &str) -> Result<String, String> {
    let peeled = format!("refs/tags/{tag}^{{commit}}");
    let out = crate::ledger::git_ok(git, &["rev-parse", "--verify", "--quiet", &peeled]).map_err(
        |e| format!("this checkout has no tag {tag} ({e}): fetch the tags from origin first"),
    )?;
    let commit = out.stdout_utf8().trim().to_string();
    if !aterm_codec::hex::is_lower(&commit, 40) {
        return Err(format!("tag {tag} names {commit:?}, not a full commit id"));
    }
    Ok(commit)
}

/// One asset of a release as GitHub lists it.
struct Listed {
    name: String,
    size: u64,
}

/// A published release: its id, tag and assets.
struct Release {
    id: u64,
    tag: String,
    assets: Vec<Listed>,
}

impl Release {
    fn lists(&self, name: &str) -> bool {
        self.assets.iter().any(|asset| asset.name == name)
    }
}

/// `object` (a release as GitHub's API spells it) as a [`Release`], refused unless it is
/// published: not a draft and not a prerelease (a demoted build).
fn release_from(slug: &str, object: &Value) -> Result<Release, String> {
    let tag = object["tag_name"].as_str().unwrap_or_default().to_string();
    let id = object["id"]
        .as_u64()
        .ok_or_else(|| format!("{slug}'s release {tag:?} has no numeric id"))?;
    if object["draft"].as_bool() != Some(false) || object["prerelease"].as_bool() != Some(false) {
        return Err(format!(
            "{slug}'s release {tag} is a draft or a prerelease, not a public release"
        ));
    }
    let assets = crate::publish::release_asset_inventory_for_release_id(slug, id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|asset| Listed {
            name: asset.name,
            size: asset.size,
        })
        .collect();
    Ok(Release { id, tag, assets })
}

/// The one public release of `slug` tagged `tag`.
fn public_release(slug: &str, tag: &str) -> Result<Release, String> {
    let listing = crate::publish::release_listing(slug).map_err(|e| e.to_string())?;
    let found: Vec<&Value> = listing
        .iter()
        .filter(|object| object["tag_name"].as_str() == Some(tag))
        .collect();
    let [object] = found.as_slice() else {
        return Err(format!(
            "{slug} has {} releases tagged {tag}, not exactly one",
            found.len()
        ));
    };
    release_from(slug, object)
}

/// The channel head: `releases/latest`.
fn head_release(slug: &str) -> Result<Release, String> {
    let object =
        crate::github::get(&format!("repos/{slug}/releases/latest")).map_err(|e| e.to_string())?;
    release_from(slug, &object)
}

fn version_order(tag: &str) -> Result<(u64, u64, u64), String> {
    crate::publish::canonical_channel_tag_order(tag).map_err(|e| e.to_string())
}

/// A small asset of `release` (a `.toml` or its `.sig`), refused when the release lists none.
fn small(slug: &str, release: &Release, name: &str) -> Result<Vec<u8>, String> {
    if !release.lists(name) {
        return Err(format!("{} carries no {name}", release.tag));
    }
    crate::publish::download_release_asset_for_release_id(slug, release.id, name)
        .map_err(|e| e.to_string())
}

/// The head's roster, verified under the paper master and admitted now.
fn head_roster(ask: &Ask<'_>, head: &Release) -> Result<Roster, String> {
    let bytes = small(ask.slug, head, ROSTER_ASSET)?;
    let sig = small(ask.slug, head, ROSTER_SIG_ASSET)?;
    let verified = verify_roster(ask.masters, bytes, &sig).map_err(|e| {
        format!(
            "the channel head {}'s machine roster does not verify under this checkout's paper \
             master ({e:?})",
            head.tag
        )
    })?;
    let roster = Roster::parse(&verified)
        .map_err(|e| format!("the channel head {}'s roster is unusable ({e:?})", head.tag))?;
    roster
        .admit(0, ask.now_unix)
        .map_err(|e| format!("the channel head {}'s roster is refused ({e:?})", head.tag))?;
    Ok(roster)
}

/// Which appcast of a release is read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Proof {
    /// The channel head's current one: the channel's policy (its apply floor).
    Policy,
    /// The pinned release's: current, else its archived pair.
    Release,
}

/// `release`'s appcast, admitted under `roster` by the rules install.sh holds one to
/// ([`crate::site::linux::signed_release_appcast`]). Only a current appcast supplies the
/// channel's policy ([`Proof::Policy`]); a release behind the head may carry only its archived
/// pair.
fn signed_appcast(
    ask: &Ask<'_>,
    release: &Release,
    roster: &Roster,
    proof: Proof,
) -> Result<Manifest, String> {
    let archived = format!("aterm-appcast-{}.toml", release.tag);
    let name = if release.lists(APPCAST) {
        APPCAST.to_string()
    } else if proof == Proof::Policy {
        return Err(format!(
            "the channel head {} carries no current {APPCAST}: an archived appcast cannot \
             supply the channel's policy",
            release.tag
        ));
    } else if release.lists(&archived) {
        archived
    } else {
        return Err(format!(
            "{} carries no signed appcast ({APPCAST} or {archived})",
            release.tag
        ));
    };
    let bytes = small(ask.slug, release, &name)?;
    let sig = small(ask.slug, release, &format!("{name}.sig"))?;
    crate::site::linux::signed_release_appcast(
        ask.slug,
        roster,
        &bytes,
        &sig,
        &release.tag,
        ask.now_unix,
    )
    .map_err(|e| {
        format!(
            "{}'s {name} is refused under the channel head's roster: {e}",
            release.tag
        )
    })
}

#[cfg(all(test, unix))]
#[path = "pin_release_tests.rs"]
mod tests;
