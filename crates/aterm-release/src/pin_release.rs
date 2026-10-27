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
    let version = crate::publish::canonical_channel_tag_version(ask.tag)
        .map_err(|_| format!("{:?} is not a release tag (vMAJOR.MINOR.PATCH)", ask.tag))?;
    let ledger_path = root.join("RELEASES.ledger");
    let ledger = std::fs::read_to_string(&ledger_path)
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
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-pin-verifier-")
        .tempdir_in(ask.scratch)
        .map_err(|e| format!("make a private download directory: {e}"))?;
    for receipt in &receipts {
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
        let at = scratch.path().join(&receipt.asset);
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
mod tests {
    use super::*;
    use crate::github::Fixture;
    use crate::ledger::RunOut;
    use agh_fake::{AssetSpec, FakeGithub, ReleaseSpec, RepoSpec};
    use aterm_update_core::github_rest::Origins;
    use aterm_update_core::linux::LinuxTarget;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    const SLUG: &str = "fixture-pin/aterm";
    const TAG: &str = "v0.100.0";
    const NOW: i64 = 1_785_801_600;
    const MASTER: [u8; 32] = [0x41; 32];
    const MACHINE: [u8; 32] = [0x42; 32];
    const OTHER: [u8; 32] = [0x43; 32];
    const LEDGER: &str = "# ledger\n99 0.99.0\n100 0.100.0\n";

    fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
    }

    fn pk(seed: &[u8; 32]) -> String {
        aterm_codec::base64::encode(kp(seed).public_key().as_ref()).unwrap()
    }

    fn sign(seed: &[u8; 32], bytes: &[u8]) -> Vec<u8> {
        kp(seed).sign(bytes).as_ref().to_vec()
    }

    fn commit() -> String {
        "a".repeat(40)
    }

    /// A synthetic static verifier for `target` (`interpreter` makes it dynamic), as the
    /// handoff tests build one.
    fn verifier(target: LinuxTarget, interpreter: bool) -> Vec<u8> {
        let mut bytes = vec![0u8; 64 + 56];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        let machine: u16 = if target == LinuxTarget::Aarch64 {
            183
        } else {
            62
        };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
        let kind: u32 = if interpreter { 3 } else { 1 };
        bytes[64..68].copy_from_slice(&kind.to_le_bytes());
        bytes
    }

    fn digest(bytes: &[u8]) -> String {
        crate::publish::sha256_bytes(bytes)
    }

    fn roster(valid_until: &str) -> Vec<u8> {
        format!(
            "schema = 1\nroster_seq = 5\nvalid_until = \"{valid_until}\"\nrevoked = []\n\n\
             [[machine]]\nid = \"m3\"\npubkey = \"{}\"\nadded_at = \"2026-08-01T00:00:00Z\"\n",
            pk(&MACHINE)
        )
        .into_bytes()
    }

    /// An appcast's head for `version` (build `build`, commit `commit`, floor `min_build`),
    /// attributed to the roster's `m3` and naming that release of the channel as its origin.
    fn cast(version: &str, build: u64, commit: &str, min_build: u64) -> String {
        format!(
            "schema = 1\nversion = \"{version}\"\nbuild_number = {build}\ncommit = \"{commit}\"\n\
             dmg = \"aterm-{version}.dmg\"\nsha256 = \"{}\"\n\
             url = \"https://github.com/{SLUG}/releases/download/v{version}/aterm-{version}.dmg\"\n\
             min_build = {min_build}\nmachine_id = \"m3\"\nroster_seq = 5\n",
            "b".repeat(64)
        )
    }

    /// A channel head apart from the pinned release: its version and build, and the apply
    /// floor its signed appcast sets.
    #[derive(Clone)]
    struct Head {
        version: &'static str,
        build: u64,
        min_build: u64,
    }

    /// The release the fake serves, and how each case bends it.
    #[derive(Clone)]
    struct Shape {
        /// The verifier bytes the appcast signs, per architecture (`None`: not signed).
        signed: [Option<Vec<u8>>; 2],
        /// The verifier bytes the release serves, per architecture (`None`: not uploaded).
        served: [Option<Vec<u8>>; 2],
        /// The machine whose key signs the appcast.
        signer: [u8; 32],
        /// The appcast's commit and build.
        commit: String,
        build: u64,
        /// Whether the release is a prerelease (a demoted build).
        prerelease: bool,
        /// Whether the release carries its appcast only as the archived pair.
        archived: bool,
        /// One text replacement in the signed appcast (before it is signed).
        edit: Option<(&'static str, &'static str)>,
        /// The roster's `valid_until`.
        valid_until: &'static str,
        /// The channel head, when it is another release (`None`: the pinned one is the head).
        head: Option<Head>,
    }

    impl Shape {
        fn good() -> Self {
            let x86 = verifier(LinuxTarget::X86_64, false);
            let arm = verifier(LinuxTarget::Aarch64, false);
            Self {
                signed: [Some(x86.clone()), Some(arm.clone())],
                served: [Some(x86), Some(arm)],
                signer: MACHINE,
                commit: commit(),
                build: 100,
                prerelease: false,
                archived: false,
                edit: None,
                valid_until: "9999-12-31T23:59:59Z",
                head: None,
            }
        }

        /// `good`, behind a head `v0.101.0` whose appcast sets the floor `min_build`, and
        /// carrying its own appcast as the archived pair, as a release behind the head does.
        fn behind(min_build: u64) -> Self {
            Self {
                archived: true,
                head: Some(Head {
                    version: "0.101.0",
                    build: 101,
                    min_build,
                }),
                ..Self::good()
            }
        }

        fn appcast(&self) -> Vec<u8> {
            let mut text = cast("0.100.0", self.build, &self.commit, 0);
            let mut any = false;
            for (arch, bytes) in pin::ARCHES.iter().zip(&self.signed) {
                if let Some(bytes) = bytes {
                    any = true;
                    text.push_str(&format!(
                        "linux_verifier_{arch} = \"{}\"\nlinux_verifier_{arch}_sha256 = \
                         \"{}\"\nlinux_verifier_{arch}_size = {}\n",
                        pin::asset_name("0.100.0", arch),
                        digest(bytes),
                        bytes.len()
                    ));
                }
            }
            if any {
                text.push_str(&format!("linux_verifier_interface = {}\n", pin::INTERFACE));
            }
            if let Some((from, to)) = self.edit {
                assert!(text.contains(from), "the edit {from:?} applies to:\n{text}");
                text = text.replace(from, to);
            }
            text.into_bytes()
        }
    }

    /// A loopback GitHub serving `shape` as the channel's release (and head, unless the
    /// shape names another), routed for this thread while the world lives.
    struct World {
        _fake: FakeGithub,
        _routed: crate::github::FixtureGuard,
        root: aterm_tempfile::TempDir,
        scratch: aterm_tempfile::TempDir,
        script: String,
        /// The test's paper master public key.
        master: String,
    }

    fn world(shape: &Shape) -> World {
        let fake = FakeGithub::start();
        fake.add_repo(SLUG, RepoSpec::public());
        let id = fake.add_release(
            SLUG,
            ReleaseSpec::published(TAG, "2026-10-26T00:00:00Z").prerelease(shape.prerelease),
        );
        let appcast = shape.appcast();
        let name = if shape.archived {
            format!("aterm-appcast-{TAG}.toml")
        } else {
            APPCAST.to_string()
        };
        fake.add_asset(id, AssetSpec::uploaded(&name, &appcast));
        fake.add_asset(
            id,
            AssetSpec::uploaded(&format!("{name}.sig"), &sign(&shape.signer, &appcast)),
        );
        for (arch, bytes) in pin::ARCHES.iter().zip(&shape.served) {
            if let Some(bytes) = bytes {
                fake.add_asset(
                    id,
                    AssetSpec::uploaded(&pin::asset_name("0.100.0", arch), bytes),
                );
            }
        }
        let head = match &shape.head {
            None => id,
            Some(head) => {
                let tag = format!("v{}", head.version);
                let at =
                    fake.add_release(SLUG, ReleaseSpec::published(&tag, "2026-10-27T00:00:00Z"));
                let policy = cast(head.version, head.build, &"d".repeat(40), head.min_build);
                fake.add_asset(at, AssetSpec::uploaded(APPCAST, policy.as_bytes()));
                fake.add_asset(
                    at,
                    AssetSpec::uploaded(
                        &format!("{APPCAST}.sig"),
                        &sign(&MACHINE, policy.as_bytes()),
                    ),
                );
                at
            }
        };
        let roster = roster(shape.valid_until);
        fake.add_asset(head, AssetSpec::uploaded(ROSTER_ASSET, &roster));
        fake.add_asset(
            head,
            AssetSpec::uploaded(ROSTER_SIG_ASSET, &sign(&MASTER, &roster)),
        );
        fake.set_latest(SLUG, Some(head));
        let p = fake.ports();
        let routed = Fixture {
            origins: Origins::loopback(p.api, p.uploads, p.web, p.cdn),
            token: None,
            spawn: None,
        }
        .enter();
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-pin-root-")
            .tempdir()
            .unwrap();
        let script = format!(
            "#!/bin/bash\n# the installer\n{}rest\n",
            Pin::default().lines()
        );
        std::fs::create_dir_all(root.path().join("tools")).unwrap();
        std::fs::create_dir_all(root.path().join("crates/aterm-bootstrap-verify")).unwrap();
        std::fs::write(root.path().join("tools/install.sh"), &script).unwrap();
        std::fs::write(root.path().join("RELEASES.ledger"), LEDGER).unwrap();
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-pin-scratch-")
            .tempdir()
            .unwrap();
        World {
            _fake: fake,
            _routed: routed,
            root,
            scratch,
            script,
            master: pk(&MASTER),
        }
    }

    /// A checkout whose tag `v0.100.0` names `commit` (and has no other tag).
    struct Tags(String);

    impl GitRunner for Tags {
        fn git(&self, args: &[&str]) -> crate::ledger::Result<RunOut> {
            let wanted = format!("refs/tags/{TAG}^{{commit}}");
            Ok(
                if args == ["rev-parse", "--verify", "--quiet", wanted.as_str()] {
                    RunOut {
                        status: 0,
                        stdout: format!("{}\n", self.0).into_bytes(),
                        stderr: Vec::new(),
                    }
                } else {
                    RunOut {
                        status: 1,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    }
                },
            )
        }
    }

    impl World {
        /// The ask for `tag` under `masters`, downloading into this world's scratch.
        fn ask<'a>(&'a self, tag: &'a str, masters: &'a [&'a str]) -> Ask<'a> {
            Ask {
                slug: SLUG,
                tag,
                masters,
                now_unix: NOW,
                scratch: self.scratch.path(),
            }
        }

        /// `read_record`, then `write_pin` with what it read, as `pin_from_release` runs them.
        fn pin(&self, git: &dyn GitRunner, ask: &Ask<'_>) -> Result<Pin, String> {
            let record = read_record(self.root.path(), git, ask)?;
            super::super::write_pin(self.root.path(), &record)
        }

        /// [`Self::refused`] for `v0.100.0` under the test's paper master.
        fn refuses(&self, git: &dyn GitRunner, why: &str) {
            let masters = [self.master.as_str()];
            self.refused(git, &self.ask(TAG, &masters), why);
        }

        /// The refusal `why` (a fragment of its words), with nothing written into the checkout
        /// and no download left behind.
        fn refused(&self, git: &dyn GitRunner, ask: &Ask<'_>, why: &str) {
            let said = self.pin(git, ask).expect_err(why);
            assert!(said.contains(why), "{why}: {said}");
            assert_eq!(
                std::fs::read_to_string(self.root.path().join("tools/install.sh")).unwrap(),
                self.script,
                "{why}: install.sh untouched"
            );
            assert!(
                !self.root.path().join(pin::RECORD).exists(),
                "{why}: no record"
            );
            assert_eq!(
                std::fs::read_dir(self.scratch.path()).unwrap().count(),
                0,
                "{why}: the download directory is gone"
            );
        }
    }

    /// `shape`'s release pins the architectures its appcast signs: install.sh's four lines
    /// and the committed record, the record exactly the one the cut's sign step would have
    /// written from the same receipts, and the pair passes `pin::check`.
    fn pins_as_the_sign_step_would(shape: &Shape) {
        let w = world(shape);
        let masters = [w.master.as_str()];
        let ask = w.ask(TAG, &masters);
        let record = read_record(w.root.path(), &Tags(commit()), &ask).unwrap();
        let mut handoff = super::super::Handoff {
            directory: std::path::PathBuf::from("/"),
            targets: super::super::TARGETS
                .iter()
                .map(|t| t.triple().to_string())
                .collect(),
            artifacts: Vec::new(),
            verifiers: Vec::new(),
            workers: Vec::new(),
        };
        for (target, bytes) in super::super::TARGETS.iter().zip(&shape.signed) {
            let Some(bytes) = bytes else { continue };
            handoff.verifiers.push(super::super::VerifierArtifact {
                schema: 1,
                version: "0.100.0".into(),
                build_number: 100,
                commit: commit(),
                target: target.triple().into(),
                asset: pin::asset_name("0.100.0", target.arch()),
                sha256: digest(bytes),
                size: bytes.len() as u64,
                source_fingerprint: "b".repeat(64),
                compiler_version: format!("release: 1.99.1\nhost: {}\n", target.triple()),
            });
        }
        assert_eq!(
            Some(record.clone()),
            handoff.record(SLUG, "0.100.0", 100, &commit())
        );
        assert_eq!(
            std::fs::read_dir(w.scratch.path()).unwrap().count(),
            0,
            "the downloads are removed"
        );
        let pin = w.pin(&Tags(commit()), &ask).unwrap();
        assert_eq!(pin, record.pin());
        let signed = |i: usize| shape.signed[i].as_deref().map(digest).unwrap_or_default();
        assert_eq!(
            (pin.x86_64.clone(), pin.aarch64.clone()),
            (signed(0), signed(1))
        );
        let written = std::fs::read_to_string(w.root.path().join("tools/install.sh")).unwrap();
        let committed = std::fs::read_to_string(w.root.path().join(pin::RECORD)).unwrap();
        assert_eq!(pin::check(&written, Some(&committed), LEDGER), Ok(pin));
    }

    /// The head's signed verifiers pin both architectures as the sign step would.
    #[test]
    fn a_signed_release_pins_both_architectures_as_the_sign_step_would() {
        pins_as_the_sign_step_would(&Shape::good());
    }

    /// A release behind the head pins from its archived appcast, under the head's roster and
    /// a floor at its build; one whose appcast signs a single architecture pins that one alone
    /// (the other stays on install.sh's embedded verifier), as its record would.
    #[test]
    fn a_release_behind_the_head_or_with_one_architecture_pins_what_it_signs() {
        pins_as_the_sign_step_would(&Shape::behind(100));
        let mut shape = Shape::good();
        shape.signed[1] = None;
        shape.served[1] = None;
        pins_as_the_sign_step_would(&shape);
    }

    /// Negative controls: a release that lacks a signed architecture's verifier, or lists it
    /// at another size than the appcast signs, writes nothing.
    #[test]
    fn a_release_missing_a_signed_verifier_is_refused() {
        let mut shape = Shape::good();
        shape.served[0] = None;
        world(&shape).refuses(&Tags(commit()), "lacks its x86_64 bootstrap verifier");
        let mut shape = Shape::good();
        let mut longer = verifier(LinuxTarget::Aarch64, false);
        longer.push(0);
        shape.served[1] = Some(longer);
        world(&shape).refuses(
            &Tags(commit()),
            "at 121 bytes, and its signed appcast says 120",
        );
    }

    /// Negative controls: served bytes that are not the signed digest, or a signed verifier
    /// that is not a static executable, write nothing.
    #[test]
    fn a_served_verifier_that_is_not_the_signed_one_is_refused() {
        let mut shape = Shape::good();
        let mut tampered = verifier(LinuxTarget::Aarch64, false);
        tampered[100] ^= 1;
        shape.served[1] = Some(tampered);
        world(&shape).refuses(&Tags(commit()), "does not match");
        let mut shape = Shape::good();
        let dynamic = verifier(LinuxTarget::Aarch64, true);
        shape.signed[1] = Some(dynamic.clone());
        shape.served[1] = Some(dynamic);
        world(&shape).refuses(&Tags(commit()), "not a static executable");
    }

    /// Negative controls: a tag that is not a release tag, one RELEASES.ledger does not claim,
    /// a signed build or commit that is not the claim's, and a tag this checkout lacks, write
    /// nothing.
    #[test]
    fn a_release_whose_claim_this_checkout_does_not_hold_is_refused() {
        let w = world(&Shape::good());
        let masters = [w.master.as_str()];
        for bare in ["0.100.0", "v0.100", "v0.100.0-rc1"] {
            w.refused(
                &Tags(commit()),
                &w.ask(bare, &masters),
                "is not a release tag",
            );
        }
        std::fs::write(w.root.path().join("RELEASES.ledger"), "99 0.99.0\n").unwrap();
        w.refuses(&Tags(commit()), "records no claim for 0.100.0");
        let mut shape = Shape::good();
        shape.build = 101;
        world(&shape).refuses(&Tags(commit()), "not the claim this checkout holds");
        let w = world(&Shape::good());
        w.refuses(&Tags("c".repeat(40)), "not the claim this checkout holds");
        w.refuses(&Tags(String::new()), "not a full commit id");
        let masters = [w.master.as_str()];
        let older = w.ask("v0.99.0", &masters);
        w.refused(&Tags(commit()), &older, "has no tag v0.99.0");
    }

    /// Negative controls: an appcast that signs no verifier digest (every release before
    /// this change), one signed by a machine off the roster, a roster this checkout's master
    /// did not sign or whose window has lapsed, and a demoted (prerelease) release, write
    /// nothing.
    #[test]
    fn a_release_whose_verifier_digests_are_not_signed_is_refused() {
        let mut shape = Shape::good();
        shape.signed = [None, None];
        world(&shape).refuses(&Tags(commit()), "names no bootstrap verifier");
        let mut shape = Shape::good();
        shape.signer = OTHER;
        world(&shape).refuses(&Tags(commit()), "no authorized live machine signature");
        let w = world(&Shape::good());
        let other = pk(&OTHER);
        let masters = [other.as_str()];
        let foreign = w.ask(TAG, &masters);
        w.refused(&Tags(commit()), &foreign, "does not verify under");
        let mut shape = Shape::good();
        shape.valid_until = "2026-01-01T00:00:00Z";
        world(&shape).refuses(&Tags(commit()), "roster is refused (Stale)");
        let mut shape = Shape::good();
        shape.prerelease = true;
        world(&shape).refuses(&Tags(commit()), "a draft or a prerelease");
    }

    /// Negative controls: a signed appcast that names another machine than its signer,
    /// another channel's release as its origin, or an interface this tree's install.sh does
    /// not call, writes nothing.
    #[test]
    fn a_signed_appcast_of_another_signer_channel_or_interface_is_refused() {
        for (edit, why) in [
            (
                ("machine_id = \"m3\"", "machine_id = \"m4\""),
                "attribution mismatch",
            ),
            (("roster_seq = 5", "roster_seq = 6"), "attribution mismatch"),
            (
                (
                    "https://github.com/fixture-pin/aterm/",
                    "https://github.com/fixture-pin/fork/",
                ),
                "signed release origin mismatch",
            ),
            (
                (
                    "linux_verifier_interface = 1",
                    "linux_verifier_interface = 2",
                ),
                "speaks interface 2",
            ),
        ] {
            let mut shape = Shape::good();
            shape.edit = Some(edit);
            world(&shape).refuses(&Tags(commit()), why);
        }
    }

    /// Negative controls: a release newer than the head, one below the head's apply floor (a
    /// yank raised it past the build), and a head whose appcast is only an archived pair,
    /// write nothing.
    #[test]
    fn a_release_the_channel_head_does_not_serve_is_refused() {
        world(&Shape::behind(101)).refuses(&Tags(commit()), "below the apply floor 101");
        let mut shape = Shape::behind(0);
        shape.head = Some(Head {
            version: "0.99.0",
            build: 99,
            min_build: 0,
        });
        world(&shape).refuses(&Tags(commit()), "newer than the channel head v0.99.0");
        let mut shape = Shape::good();
        shape.archived = true;
        world(&shape).refuses(&Tags(commit()), "cannot supply the channel's policy");
    }

    /// `ship pin-verifier` takes a release tag after `--from-release`, or one record path,
    /// and nothing else.
    #[test]
    fn pin_verifier_takes_a_release_or_a_record_and_nothing_else() {
        use crate::cli::{Cmd, PinSource};
        let parse = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
            crate::cli::parse(&args)
        };
        assert_eq!(
            parse(&["pin-verifier", "--from-release", "v0.145.0"]),
            Ok(Cmd::PinVerifier(PinSource::Release("v0.145.0".into())))
        );
        assert_eq!(
            parse(&["pin-verifier", "dist/aterm-bootstrap-verify-pin.toml"]),
            Ok(Cmd::PinVerifier(PinSource::Record(
                "dist/aterm-bootstrap-verify-pin.toml".into()
            )))
        );
        for bad in [
            &["pin-verifier"][..],
            &["pin-verifier", "--from-release"],
            &["pin-verifier", "--from-release", "--dry-run"],
            &["pin-verifier", "--from-release", "v0.145.0", "extra"],
            &["pin-verifier", "dist/a.toml", "dist/b.toml"],
            &["pin-verifier", "--record", "dist/a.toml"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
