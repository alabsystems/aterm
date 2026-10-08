// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CUT PUBLISHES ONCE, AND OWNS `latest` — proved against a fake GitHub.
//!
//! `pub publish` creates aterm's vX.Y.0 release on the public channel first, as a
//! SOURCE release, and the cut's `publish` step ADOPTS it and puts the app on it — the
//! one release object of the version (since 2026-09-26 there is no second, private
//! one, flipped and verified first and then mirrored here).
//! Every credential-less updater finds the channel head with one request,
//! `releases/latest/download/aterm-appcast.toml`, so whichever release GitHub calls
//! `latest` IS the channel head. Until 2026-09-23 two things were wrong at once:
//!
//! * the source release was created as a full release, so GitHub made it `latest`
//!   about twenty minutes before it carried any appcast — every updater 302'd to a
//!   release with nothing to install (v0.79.0 .. v0.91.0);
//! * the adopt path sent no `make_latest` at all, and passed its pointer gate only
//!   BECAUSE of that steal.
//!
//! The engine now creates the source release as a prerelease (never `latest`), and
//! [`channel::publish_on_channel`] sends one guarded head PATCH ([`channel::HEAD_PATCH`]:
//! `draft=false`, `prerelease=false`, `make_latest=true`) on every path, after the
//! appcast pair is up and the release's exact asset set is proved. The fake below
//! models the three GitHub rules that decide
//! `latest` — a newly published full release takes it, a prerelease never can,
//! `make_latest=true` on a full release takes it — applies exactly the PATCH fields
//! the cutter's own table sends, and records every moment at which `latest` named a
//! release a client could not install from. The pointer verdict is the cutter's own
//! gate, [`publish::prove_pointer_serves`], fed from the fake.

use crate::{channel, ledger, manifest_out, publish, verify};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use channel::{ChannelRelease, HeadPatchValue};
use ledger::{Error, Result, RunOut};
use publish::PointerProbe;

const OWNER: &str = "alabsystems";
const REPO: &str = "aterm";
const SLUG: &str = "alabsystems/aterm";
const PREVIOUS: &str = "v0.90.0";
const TAG: &str = "v0.91.0";
const VERSION: &str = "0.91.0";
const PREVIOUS_BUILD: u64 = 1_789_000_000;
const BUILD: u64 = 1_789_100_000;
/// The machine-roster generation this cut's signed appcast carries.
const CARRIED_ROSTER: u64 = 7;

fn download_url(tag: &str, name: &str) -> String {
    aterm_update_core::cdn::release_download_url(OWNER, REPO, tag, name)
        .expect("canonical names build a download URL")
}

/// A valid appcast for `version`, whose `url` binds it to its tag and DMG exactly as
/// the web-lane client demands.
fn appcast(version: &str, build: u64) -> Vec<u8> {
    let tag = format!("v{version}");
    let dmg = channel::dmg_asset_name(version);
    format!(
        "schema = 1\nversion = \"{version}\"\nbuild_number = {build}\n\
         dmg = \"{dmg}\"\nsha256 = \"{}\"\nurl = \"{}\"\n",
        "ab".repeat(32),
        download_url(&tag, &dmg)
    )
    .into_bytes()
}

#[derive(Debug, Clone)]
struct Release {
    tag: String,
    draft: bool,
    prerelease: bool,
    /// `DELETE /releases/{id}` — only a negative control ever sets it.
    deleted: bool,
    assets: BTreeMap<String, Vec<u8>>,
}

/// GitHub, as far as `latest` is concerned.
#[derive(Default)]
struct FakeGithub {
    releases: Vec<Release>,
    /// Index into `releases` of the release GitHub calls `latest`.
    latest: Option<usize>,
    /// The pre-fix adopt path: the cutter sends no head PATCH at all.
    drop_head_patch: bool,
    /// The head PATCH body, when a test replaces the cutter's own
    /// [`channel::HEAD_PATCH`] (a negative control).
    head_fields: Option<Vec<(&'static str, HeadPatchValue)>>,
    /// Every time the cut bound its channel release.
    binds: usize,
    /// Every upload name, in order.
    uploads: Vec<String>,
    /// For each head PATCH: were the appcast and its signature on the release?
    head_patches: Vec<(bool, bool)>,
    /// Every moment `latest` named a release a client cannot install from.
    exposures: Vec<String>,
    /// The journal's bound release, as a resume reads it back (`bind` records it).
    journal_release: Option<usize>,
    /// The machine-roster generation the fleet has observed on the channel.
    roster_seq: u64,
    /// GitHub's `releases/latest` cache: while set, the pointer still names this release
    /// whatever `latest` is (v0.93.0: five minutes after the PATCH landed).
    stale_pointer: Option<usize>,
    /// NEGATIVE CONTROL: a resume that does not ask whether its head PATCH landed — the
    /// sequence before 2026-09-26.
    forget_head: bool,
    /// The next upload POST of this name fails, once, the way [`UploadFailure`] says.
    fail_upload_once: Option<(String, UploadFailure)>,
    /// The journal's durable upload intents (`Journal::upload_intents`): persisted
    /// before every upload POST, retracted only when the live cutter's own verdict
    /// ([`publish::post_outcome`]) says the POST created nothing.
    upload_intents: Vec<String>,
    /// NEGATIVE CONTROL: a bind that adopts whatever release is under the tag, a FULL
    /// source release included — the cutter before it asked the release's flag.
    adopt_unchecked: bool,
}

/// How one upload POST fails, as curl reports it to the live cutter.
#[derive(Debug, Clone, Copy)]
enum UploadFailure {
    /// curl refused its own arguments (exit 2): nothing left the machine.
    NeverSent,
    /// The server answered 422: understood, refused, nothing created.
    Refused,
    /// A 502 from the edge: whether GitHub stored the object is unknowable to the cutter
    /// (here it did not).
    ServerError,
    /// The server answered 422 — `already_exists`, for an object nobody journaled that the
    /// pre-POST probe could not see yet: the POST created nothing, and the object is there.
    RefusedButStored,
    /// A 502 that hid a POST GitHub did store: the object landed, only the answer was lost.
    ServerErrorButStored,
}

impl UploadFailure {
    fn out(self) -> RunOut {
        let (status, stderr) = match self {
            Self::NeverSent => (2, "curl: option --upload-file: out of memory"),
            Self::Refused | Self::RefusedButStored => {
                (22, "curl: (22) The requested URL returned error: 422")
            }
            Self::ServerError | Self::ServerErrorButStored => {
                (22, "curl: (22) The requested URL returned error: 502")
            }
        };
        RunOut {
            status,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }
}

impl FakeGithub {
    /// The channel as it stands when a release is cut: the previous app release is
    /// complete and holds `latest`.
    fn with_previous_release() -> Self {
        let mut github = Self {
            roster_seq: CARRIED_ROSTER,
            ..Self::default()
        };
        github.app_release("0.90.0", PREVIOUS_BUILD);
        github
    }

    /// `atpkg-keys join`: the fleet's roster moves to the next generation, and the join
    /// re-dresses the channel head with the new pair — no lease, no re-signed manifest.
    fn roster_join(&mut self) {
        self.roster_seq += 1;
        let bytes = format!("roster generation {}", self.roster_seq).into_bytes();
        if let Some(head) = self.latest {
            for name in [
                aterm_update_core::roster::ROSTER_ASSET,
                aterm_update_core::roster::ROSTER_SIG_ASSET,
            ] {
                if let Some(asset) = self.releases[head].assets.get_mut(name) {
                    asset.clone_from(&bytes);
                }
            }
        }
    }

    /// Is this release the channel's PUBLISHED app release — what only a head PATCH makes
    /// of an adopted prerelease or a draft (`verify::ReleaseState::Published`)?
    fn published(&self, id: usize) -> bool {
        let release = &self.releases[id];
        !release.draft
            && !release.prerelease
            && !release.deleted
            && release.assets.contains_key(manifest_out::MANIFEST_ASSET)
    }

    /// What the cutter's own classifier ([`verify::classify_release_state`], the one
    /// `--abandon` and `head_made` read) makes of release `id`, over the row
    /// `release_metadata_listing` would emit for it.
    fn release_state(&self, id: usize) -> verify::ReleaseState {
        let release = &self.releases[id];
        let appcast = usize::from(release.assets.contains_key(manifest_out::MANIFEST_ASSET));
        let row = format!(
            "{id}\t{}\t{}\t{}\t{appcast}\n",
            release.tag, release.draft, release.prerelease
        );
        verify::classify_release_state(&row, &release.tag).expect("a well-formed row")
    }

    /// A complete app release, published as a full release — so it takes `latest`.
    fn app_release(&mut self, version: &str, build: u64) -> usize {
        let mut assets = BTreeMap::new();
        for name in channel::required_asset_names(version, true, true) {
            let bytes = if name == manifest_out::MANIFEST_ASSET {
                appcast(version, build)
            } else {
                name.clone().into_bytes()
            };
            assets.insert(name, bytes);
        }
        self.releases.push(Release {
            tag: format!("v{version}"),
            draft: false,
            prerelease: false,
            deleted: false,
            assets,
        });
        let id = self.releases.len() - 1;
        self.latest = Some(id);
        self.observe(&format!("publish v{version}"));
        id
    }

    /// `POST /releases`. GitHub's rule: a release published as a full release (not a
    /// draft, not a prerelease) takes `latest` (`make_latest` defaults to `"true"`).
    fn create(&mut self, tag: &str, draft: bool, prerelease: bool, assets: &[&str]) -> usize {
        self.releases.push(Release {
            tag: tag.into(),
            draft,
            prerelease,
            deleted: false,
            assets: assets
                .iter()
                .map(|name| ((*name).to_string(), name.as_bytes().to_vec()))
                .collect(),
        });
        let id = self.releases.len() - 1;
        if !draft && !prerelease {
            self.latest = Some(id);
        }
        self.observe(&format!("create {tag}"));
        id
    }

    /// `pub publish`'s sign-release: the source release, with its attestation pair and
    /// the roster pair it re-uploads. `prerelease` is the engine's reading of
    /// `BINARY_CUT_FOLLOWS_DEFAULT` in the published tree's `publish/config.sh`.
    fn source_release(&mut self, prerelease: bool) -> usize {
        self.create(
            TAG,
            false,
            prerelease,
            &[
                "SHA256SUMS",
                "SHA256SUMS.sig",
                aterm_update_core::roster::ROSTER_ASSET,
                aterm_update_core::roster::ROSTER_SIG_ASSET,
            ],
        )
    }

    /// `PATCH /releases/{id}` with exactly the fields the cutter sends — its own
    /// [`channel::HEAD_PATCH`] unless a test replaced it. GitHub's rule: `make_latest=true`
    /// makes a FULL release `latest`, and moves nothing for a draft or a prerelease.
    fn head_patch(&mut self, id: usize) {
        let fields = self
            .head_fields
            .clone()
            .unwrap_or_else(|| channel::HEAD_PATCH.to_vec());
        let release = &mut self.releases[id];
        self.head_patches.push((
            release.assets.contains_key(manifest_out::MANIFEST_ASSET),
            release
                .assets
                .contains_key(manifest_out::MANIFEST_SIG_ASSET),
        ));
        let mut make_latest = false;
        for (name, value) in fields {
            match (name, value) {
                ("draft", HeadPatchValue::Bool(value)) => release.draft = value,
                ("prerelease", HeadPatchValue::Bool(value)) => release.prerelease = value,
                ("make_latest", HeadPatchValue::Str("true")) => make_latest = true,
                other => panic!("the fake GitHub does not model PATCH field {other:?}"),
            }
        }
        if make_latest && !release.draft && !release.prerelease {
            self.latest = Some(id);
        }
        self.observe("head PATCH");
    }

    /// Record an exposure if `latest` now names a release with no installable head.
    fn observe(&mut self, after: &str) {
        let Some(id) = self.latest else {
            return;
        };
        let release = &self.releases[id];
        let has = |name: &str| release.assets.contains_key(name);
        if !(has(manifest_out::MANIFEST_ASSET) && has(manifest_out::MANIFEST_SIG_ASSET)) {
            self.exposures.push(format!(
                "after {after}: latest → {} without its appcast pair",
                release.tag
            ));
        }
    }

    /// What `releases/latest/download/aterm-appcast.toml` answers a stranger, read the
    /// way the deployed client reads it (a tag outside its grammar is not an app).
    fn pointer(&self) -> PointerProbe {
        match self.stale_pointer.or(self.latest) {
            Some(id) => {
                let tag = self.releases[id].tag.clone();
                if !aterm_update_core::pointer::canonical_app_tag(&tag) {
                    return PointerProbe::NotAnApp { tag };
                }
                PointerProbe::Tag {
                    location: download_url(&tag, manifest_out::MANIFEST_ASSET),
                    tag,
                }
            }
            None => PointerProbe::NoRelease,
        }
    }

    /// An anonymous GET of a tag-specific download URL: the bytes, or `None` for 404.
    fn get(&self, url: &str) -> Option<Vec<u8>> {
        self.releases
            .iter()
            .filter(|release| !release.draft && !release.deleted)
            .flat_map(|release| {
                release
                    .assets
                    .iter()
                    .map(move |(name, bytes)| (download_url(&release.tag, name), bytes))
            })
            .find(|(candidate, _)| candidate == url)
            .map(|(_, bytes)| bytes.clone())
    }

    /// [`Self::get`] for an asset that must be there (the pointer gate's fetch).
    fn fetch(&self, url: &str) -> Result<Vec<u8>> {
        self.get(url)
            .ok_or_else(|| Error::new(format!("GET {url}: 404")))
    }
}

/// The cut's view of its channel release, through the same trait the real step drives.
/// `id` is the release the bind will find; like the live step, every release call
/// before the bind is refused.
struct FakeChannel<'a> {
    github: &'a mut FakeGithub,
    id: usize,
    local_appcast: Vec<u8>,
    /// This cut's `dist/`, as `artifacts` hands it over.
    files: Vec<PathBuf>,
    bound: bool,
}

impl<'a> FakeChannel<'a> {
    fn new(
        github: &'a mut FakeGithub,
        id: usize,
        local_appcast: Vec<u8>,
        files: Vec<PathBuf>,
    ) -> Self {
        Self {
            github,
            id,
            local_appcast,
            files,
            bound: false,
        }
    }

    /// The head half of the ratchet, decided by the cutter's own function over what the
    /// fake serves a stranger.
    fn head_half(&self) -> Result<()> {
        let github = &*self.github;
        publish::prove_channel_head_is_older(
            SLUG,
            TAG,
            BUILD,
            &mut || Ok(github.pointer()),
            &mut |url| Ok(github.get(url)),
        )
    }

    /// The roster half, decided by the cutter's own floor: this cut's generation against
    /// the one the fleet has observed. (The lineage-fork half reads signed roster
    /// documents; `machine_roster.rs` owns its proofs.)
    fn roster_half(&self) -> Result<()> {
        publish::roster_floor_covered(Some(CARRIED_ROSTER), Some(self.github.roster_seq))
    }

    fn bound(&self) -> Result<()> {
        if self.bound {
            Ok(())
        } else {
            Err(Error::new("a release call before the bind"))
        }
    }
}

impl ChannelRelease for FakeChannel<'_> {
    fn head_made(&mut self) -> Result<bool> {
        if self.github.forget_head || self.github.journal_release != Some(self.id) {
            return Ok(false);
        }
        self.bound = self.github.published(self.id);
        Ok(self.bound)
    }

    fn artifacts(&mut self) -> Result<Vec<PathBuf>> {
        Ok(self.files.clone())
    }

    fn bind(&mut self) -> Result<()> {
        // A visible release the journal holds no intent for is adopted only as the
        // engine's source PRERELEASE — the cutter's own two verdicts, as the live bind
        // asks them (`adoptable_channel_release`).
        let release = &self.github.releases[self.id];
        if !release.draft
            && self.github.journal_release != Some(self.id)
            && !self.github.adopt_unchecked
        {
            let names: Vec<String> = release.assets.keys().cloned().collect();
            channel::unclaimed_is_source_release(&names)
                .and_then(|()| channel::unclaimed_source_is_prerelease(release.prerelease))
                .map_err(|why| Error::new(format!("{} {why}", release.tag)))?;
        }
        self.github.binds += 1;
        self.github.journal_release = Some(self.id);
        self.bound = true;
        Ok(())
    }

    fn upload(&mut self, file: &Path) -> Result<()> {
        self.bound()?;
        let name = file.file_name().unwrap().to_str().unwrap().to_string();
        let bytes = std::fs::read(file).map_err(|e| Error::new(e.to_string()))?;
        self.github.uploads.push(name.clone());
        // `upload_channel_asset`'s admission, by the cutter's own decision.
        let prior_intent = self.github.upload_intents.contains(&name);
        let visible = self.github.releases[self.id].assets.get(&name).cloned();
        match publish::durable_post_decision(prior_intent, visible.is_some()) {
            publish::DurablePostDecision::AwaitVisibility => {
                return Err(Error::new(format!(
                    "upload intent for {name} was already durably issued, but the asset is \
                     not visible; refusing a duplicate POST"
                )));
            }
            publish::DurablePostDecision::ConvergeVisible => match visible {
                Some(existing) if existing == bytes => return Ok(()),
                // The source release's roster copy may be stale: the one permitted
                // replacement (delete by ID, then the same one-shot POST).
                Some(_)
                    if name == aterm_update_core::roster::ROSTER_ASSET
                        || name == aterm_update_core::roster::ROSTER_SIG_ASSET =>
                {
                    self.github.releases[self.id].assets.remove(&name);
                }
                _ => return Err(Error::new(format!("{name} exists with other bytes"))),
            },
            publish::DurablePostDecision::PersistIntentThenPost => {}
        }
        if prior_intent {
            return Err(Error::new(format!(
                "upload intent for {name} already exists; refusing to mint another permit"
            )));
        }
        self.github.upload_intents.push(name.clone());
        let failure = self
            .github
            .fail_upload_once
            .take_if(|(failing, _)| *failing == name)
            .map(|(_, failure)| failure);
        if let Some(failure) = failure {
            // The cutter's own verdict on what this POST's outcome proves, and the
            // cutter's own decision on what to do about it (`upload_channel_asset`).
            let out = failure.out();
            let outcome = publish::post_outcome(&out);
            if matches!(
                failure,
                UploadFailure::RefusedButStored | UploadFailure::ServerErrorButStored
            ) {
                self.github.releases[self.id]
                    .assets
                    .insert(name.clone(), bytes.clone());
                self.github.observe(&format!("stored {name}"));
            }
            if outcome != publish::PostOutcome::Unknown {
                self.github.upload_intents.retain(|issued| *issued != name);
            }
            let stored = self.github.releases[self.id].assets.contains_key(&name);
            match publish::after_upload_post(outcome, || Ok(stored))? {
                publish::AfterUploadPost::Converge { journal } => {
                    // Its bytes are ours by construction; the journal names it again when
                    // the retraction had dropped it.
                    if journal {
                        self.github.upload_intents.push(name.clone());
                    }
                    return Ok(());
                }
                _ => {
                    return Err(Error::new(format!(
                        "upload {name}: {}",
                        String::from_utf8_lossy(&out.stderr)
                    )));
                }
            }
        }
        self.github.releases[self.id]
            .assets
            .insert(name.clone(), bytes);
        self.github.observe(&format!("upload {name}"));
        Ok(())
    }

    fn prove_assets(&mut self) -> Result<()> {
        self.bound()?;
        let names: Vec<String> = self.github.releases[self.id]
            .assets
            .keys()
            .cloned()
            .collect();
        channel::validate_channel_asset_set(&names, VERSION, true, true, &[])
    }

    fn ratchet_head(&mut self) -> Result<()> {
        self.head_half()
    }

    fn ratchet(&mut self) -> Result<()> {
        self.head_half()?;
        self.roster_half()
    }

    fn make_head(&mut self) -> Result<()> {
        self.bound()?;
        if !self.github.drop_head_patch {
            self.github.head_patch(self.id);
        }
        Ok(())
    }

    fn prove_head(&mut self) -> Result<()> {
        self.bound()?;
        let github = &*self.github;
        publish::prove_pointer_serves(
            SLUG,
            TAG,
            &self.local_appcast,
            &mut || Ok(github.pointer()),
            &mut |url| github.fetch(url),
            &mut |_| {},
        )
        .map(|_| ())
    }
}

/// This cut's dist/: every asset the updater elects, the appcast a real manifest.
fn dist() -> (aterm_tempfile::TempDir, Vec<PathBuf>, Vec<u8>) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-channel-latest-")
        .tempdir()
        .unwrap();
    let local_appcast = appcast(VERSION, BUILD);
    let mut files = Vec::new();
    for name in channel::required_asset_names(VERSION, true, true) {
        let path = dir.path().join(&name);
        let bytes = if name == manifest_out::MANIFEST_ASSET {
            local_appcast.clone()
        } else {
            name.clone().into_bytes()
        };
        std::fs::write(&path, bytes).unwrap();
        files.push(path);
    }
    // The order `required_asset_names` returns is alphabetical — appcast before its
    // signature, and before most binaries. The sequence must reorder it.
    assert!(
        files
            .iter()
            .position(|f| f.ends_with(manifest_out::MANIFEST_ASSET))
            < files
                .iter()
                .position(|f| f.ends_with(manifest_out::MANIFEST_SIG_ASSET)),
        "the fixture must hand the sequence the WRONG order, or the order test is vacuous"
    );
    (dir, files, local_appcast)
}

fn position(uploads: &[String], name: &str) -> usize {
    uploads
        .iter()
        .position(|n| n == name)
        .unwrap_or_else(|| panic!("{name} was never uploaded: {uploads:?}"))
}

/// THE PROOF. The engine's prerelease source release never takes `latest`; the cut
/// adopts it, uploads the appcast signature and then the appcast LAST, sends the head
/// PATCH once both are up, and the cutter's own pointer gate then passes — with
/// `latest` never once naming a release a client could not install from.
#[test]
fn the_adopted_source_release_takes_latest_only_after_its_appcast_pair() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    assert!(
        matches!(github.pointer(), PointerProbe::Tag { ref tag, .. } if tag == PREVIOUS),
        "a prerelease source release leaves the pointer on the previous app release"
    );

    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast, files);
    channel::publish_on_channel(&mut channel)
        .expect("the pointer gate passes once the cut owns latest");

    let uploads = &github.uploads;
    let sig = position(uploads, manifest_out::MANIFEST_SIG_ASSET);
    let toml = position(uploads, manifest_out::MANIFEST_ASSET);
    assert!(
        sig < toml,
        "the signature must land before the appcast: {uploads:?}"
    );
    assert_eq!(
        toml,
        uploads.len() - 1,
        "the appcast is the last upload: {uploads:?}"
    );
    assert_eq!(
        github.head_patches,
        vec![(true, true)],
        "exactly one head PATCH, sent with the appcast and its signature both up"
    );
    let head = &github.releases[id];
    assert!(!head.prerelease && !head.draft);
    assert_eq!(github.latest, Some(id));
    assert!(
        github.exposures.is_empty(),
        "latest named a release with no installable head: {:?}",
        github.exposures
    );
}

/// This cut's upload names in the order the sequence sends them (the rank sort, stable
/// like the live one).
fn ranked_upload_names(files: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = files
        .iter()
        .map(|f| f.file_name().unwrap().to_str().unwrap().to_string())
        .collect();
    names.sort_by_key(|name| channel::channel_upload_rank(name));
    names
}

/// The first pass of the adopt path, dying on the appcast signature's upload POST the
/// way `failure` says — every binary already on the adopted prerelease, the appcast pair
/// not. Whatever the failure, until the PATCH nothing about the channel head has moved:
/// `latest` and the evergreen pointer still name the previous app release, no PATCH was
/// sent, the source release is still a prerelease no client can elect, and the appcast
/// never went up. Returns the uploads the pass attempted.
fn first_pass_dies_on_the_signature(
    github: &mut FakeGithub,
    id: usize,
    files: &[PathBuf],
    local_appcast: &[u8],
    failure: UploadFailure,
) -> Vec<String> {
    github.fail_upload_once = Some((manifest_out::MANIFEST_SIG_ASSET.to_string(), failure));
    let mut channel = FakeChannel::new(github, id, local_appcast.to_vec(), files.to_vec());
    let err = channel::publish_on_channel(&mut channel).expect_err("the upload fails");
    assert!(
        err.to_string()
            .starts_with(&format!("upload {}", manifest_out::MANIFEST_SIG_ASSET)),
        "{failure:?}: {err}"
    );
    assert!(
        github.head_patches.is_empty(),
        "{failure:?}: no head PATCH before the pair is up"
    );
    assert_eq!(
        github.latest,
        Some(0),
        "{failure:?}: the previous app release keeps `latest`"
    );
    assert!(
        matches!(github.pointer(), PointerProbe::Tag { ref tag, .. } if tag == PREVIOUS),
        "{failure:?}: every updater still resolves the previous release"
    );
    assert!(
        github.releases[id].prerelease,
        "{failure:?}: still the source prerelease"
    );
    assert!(
        !github.releases[id]
            .assets
            .contains_key(manifest_out::MANIFEST_ASSET),
        "{failure:?}: the appcast never went up ahead of its signature"
    );
    assert!(github.exposures.is_empty(), "{:?}", github.exposures);
    let first_pass = github.uploads.clone();
    let ranked = ranked_upload_names(files);
    assert_eq!(
        first_pass[..],
        ranked[..first_pass.len()],
        "{failure:?}: the first pass is a prefix of the ranked sequence"
    );
    assert_eq!(
        first_pass.last().map(String::as_str),
        Some(manifest_out::MANIFEST_SIG_ASSET),
        "{failure:?}: the failure is the signature's POST, mid-sequence"
    );
    first_pass
}

/// A FAILURE BEFORE THE HEAD PATCH COSTS THE FLEET NOTHING, AND WHEN THE POST PROVABLY
/// CREATED NOTHING THE RESUME FLIPS ONCE. Two failures the cutter can prove created no
/// object — curl refusing its own arguments (exit 2, nothing sent) and an answered 4xx —
/// retract the durable upload intent (`publish::post_outcome`, the verdict the live
/// `upload_channel_asset` settles by). The resume re-binds the same release, re-sends
/// the whole ranked sequence (every earlier upload a same-bytes no-op, never a second
/// object), puts the pair up signature-first, and sends the head PATCH exactly once over
/// both passes.
///
/// NEGATIVE CONTROL: a 5xx can hide a stored object, so its intent stands and the resume
/// refuses a duplicate POST (`durable_post_decision(true, false)`) — the fake is capable
/// of the wedge, so the rows above are earned by the verdict, not by a fake that
/// re-POSTs whatever it is asked. The wedge is as harmless to the fleet as the rest: no
/// PATCH, `latest` and the pointer stay on the previous release, and `--abandon`
/// (`an_abandon_withdraws_the_cuts_assets_and_leaves_the_source_release`) is the exit.
#[test]
fn a_failure_before_the_head_patch_leaves_latest_and_a_provable_one_flips_once_on_resume() {
    for failure in [UploadFailure::NeverSent, UploadFailure::Refused] {
        let mut github = FakeGithub::with_previous_release();
        let id = github.source_release(true);
        let (_dir, files, local_appcast) = dist();
        let first_pass =
            first_pass_dies_on_the_signature(&mut github, id, &files, &local_appcast, failure);
        assert!(
            !github
                .upload_intents
                .iter()
                .any(|name| name == manifest_out::MANIFEST_SIG_ASSET),
            "{failure:?}: the intent of a POST that created nothing is retracted"
        );
        let after_first = github.releases[id].assets.clone();

        let mut channel = FakeChannel::new(&mut github, id, local_appcast, files.clone());
        channel::publish_on_channel(&mut channel).expect("the resume converges");
        assert_eq!(
            github.binds, 2,
            "{failure:?}: the resume re-binds the same release"
        );
        let second_pass = &github.uploads[first_pass.len()..];
        assert_eq!(
            second_pass,
            ranked_upload_names(&files),
            "{failure:?}: the resume re-sends the whole ranked sequence"
        );
        assert_eq!(
            second_pass[second_pass.len() - 2..],
            [
                manifest_out::MANIFEST_SIG_ASSET.to_string(),
                manifest_out::MANIFEST_ASSET.to_string()
            ],
            "{failure:?}: the signature, then the appcast, last"
        );
        assert_eq!(
            github.head_patches,
            vec![(true, true)],
            "{failure:?}: exactly one head PATCH across both passes, with the pair up"
        );
        for (name, bytes) in &after_first {
            assert_eq!(
                github.releases[id].assets.get(name),
                Some(bytes),
                "{failure:?}: {name}: a re-upload of the same bytes changes nothing"
            );
        }
        assert!(!github.releases[id].prerelease && !github.releases[id].draft);
        assert_eq!(github.latest, Some(id));
        assert!(github.exposures.is_empty(), "{:?}", github.exposures);
    }

    // The control: a 5xx keeps the intent, so no resume re-POSTs — and nothing moves.
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    first_pass_dies_on_the_signature(
        &mut github,
        id,
        &files,
        &local_appcast,
        UploadFailure::ServerError,
    );
    assert!(
        github
            .upload_intents
            .iter()
            .any(|name| name == manifest_out::MANIFEST_SIG_ASSET),
        "a 5xx keeps the conservative intent"
    );
    for resume in 1..=2 {
        let mut channel = FakeChannel::new(&mut github, id, local_appcast.clone(), files.clone());
        let err = channel::publish_on_channel(&mut channel)
            .expect_err("a resume never re-POSTs an issued intent");
        assert!(
            err.to_string().contains("refusing a duplicate POST"),
            "resume {resume}: {err}"
        );
        assert!(github.head_patches.is_empty(), "resume {resume}: no PATCH");
        assert_eq!(github.latest, Some(0), "resume {resume}: `latest` stays");
        assert!(github.releases[id].prerelease);
        assert!(github.exposures.is_empty(), "{:?}", github.exposures);
    }
}

/// AN UPLOAD THE SERVER REFUSED CAN STILL FIND ITS OBJECT THERE, AND THEN THE JOURNAL
/// MUST NAME IT. `422 already_exists` is a 4xx, so the cutter retracts the intent of a
/// refused POST before it probes (`CutCtx::settle_upload_intent`) — and an object that is
/// nevertheless on the release, one nobody journaled, would be an app asset no intent names:
/// `--abandon` (`channel::withdrawable_assets`, over the journal's intents) could not take it
/// back, and a recut would refuse the release for carrying it. `after_upload_post` — the
/// cutter's own decision, read by this fake — converges on it and has the journal name it
/// again. A 5xx that hid a stored object converges the same way with no journal write at
/// all: its intent was never retracted.
///
/// Whatever the failure, the cut then completes, and EVERY app asset on the release is one
/// `--abandon` withdraws — the property that makes an unnamed object a defect.
#[test]
fn a_refused_upload_whose_object_is_there_is_converged_and_still_named_by_the_journal() {
    for failure in [
        UploadFailure::RefusedButStored,
        UploadFailure::ServerErrorButStored,
    ] {
        let mut github = FakeGithub::with_previous_release();
        let id = github.source_release(true);
        let (_dir, files, local_appcast) = dist();
        github.fail_upload_once = Some((manifest_out::MANIFEST_SIG_ASSET.to_string(), failure));
        let mut channel = FakeChannel::new(&mut github, id, local_appcast, files.clone());
        channel::publish_on_channel(&mut channel)
            .unwrap_or_else(|err| panic!("{failure:?}: the cut converges on the object: {err}"));
        assert_eq!(
            github.uploads,
            ranked_upload_names(&files),
            "{failure:?}: one pass, no re-POST of anything"
        );
        assert_eq!(
            github.head_patches,
            vec![(true, true)],
            "{failure:?}: exactly one head PATCH, with the pair up"
        );
        let on_release: Vec<String> = github.releases[id].assets.keys().cloned().collect();
        let app: Vec<&String> = on_release
            .iter()
            .filter(|name| !channel::is_source_shape(name))
            .collect();
        let withdrawn =
            channel::withdrawable_assets(&on_release, VERSION, Some(&github.upload_intents))
                .expect("a journal holds the release's own assets");
        assert_eq!(
            withdrawn.len(),
            app.len(),
            "{failure:?}: every app asset on the release is one `--abandon` can take back \
             (journaled: {:?})",
            github.upload_intents
        );
        assert!(
            withdrawn
                .iter()
                .any(|name| name == manifest_out::MANIFEST_SIG_ASSET),
            "{failure:?}: the object the refusal met is among them"
        );
    }
}

/// GitHub recomputes `latest` behind a cache: on v0.93.0 the pointer still named the
/// previous release after the ten anonymous probes and named the cut a minute later, so a
/// release already live failed its own gate. The pointer gate waits on its own bound —
/// a lag past the old ten attempts converges — and a pointer that never moves is still
/// refused, after exactly that bound.
#[test]
fn a_pointer_github_recomputes_slowly_is_waited_for_within_its_own_bound() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast.clone(), files);
    channel::publish_on_channel(&mut channel).expect("the cut owns latest");
    let stale = || PointerProbe::Tag {
        tag: PREVIOUS.to_string(),
        location: download_url(PREVIOUS, manifest_out::MANIFEST_ASSET),
    };

    let lag = 12; // past the ten-probe budget this gate used to share
    let (mut asked, mut slept) = (0, Vec::new());
    publish::prove_pointer_serves(
        SLUG,
        TAG,
        &local_appcast,
        &mut || {
            asked += 1;
            Ok(if asked <= lag {
                stale()
            } else {
                github.pointer()
            })
        },
        &mut |url| github.fetch(url),
        &mut |d| slept.push(d),
    )
    .expect("a pointer that moves within the bound is proved");
    assert_eq!(slept, vec![publish::POINTER_PROBE_DELAY; lag]);

    let (mut asked, mut slept) = (0, Vec::new());
    let err = publish::prove_pointer_serves(
        SLUG,
        TAG,
        &local_appcast,
        &mut || {
            asked += 1;
            Ok(stale())
        },
        &mut |url| github.fetch(url),
        &mut |d| slept.push(d),
    )
    .expect_err("a pointer that never moves is refused");
    assert!(
        err.to_string()
            .contains(&format!("names {PREVIOUS}, not {TAG}")),
        "{err}"
    );
    assert_eq!(asked, publish::POINTER_PROBE_ATTEMPTS as usize);
    assert_eq!(slept.len(), publish::POINTER_PROBE_ATTEMPTS as usize - 1);
}

/// NEGATIVE CONTROL — the adopt path as it was: no head PATCH. The prerelease stays
/// a prerelease, the pointer stays on the previous release, and the cutter's gate
/// refuses. (This is also the break the 2026-09-10 decision to stop the steal would
/// have caused on its own: the adopt path passed only because of the steal.)
#[test]
fn without_the_head_patch_the_pointer_gate_refuses() {
    let mut github = FakeGithub::with_previous_release();
    github.drop_head_patch = true;
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast, files);
    let err = channel::publish_on_channel(&mut channel).expect_err("no PATCH, no latest, no pass");
    let msg = err.to_string();
    assert!(
        msg.contains(&format!("names {PREVIOUS}, not {TAG}")),
        "the gate names the release that still holds the pointer: {msg}"
    );
    assert!(github.head_patches.is_empty());
    assert!(github.releases[id].prerelease);
}

/// Why the engine creates the source release as a prerelease: as a full release it
/// takes `latest` at creation, before the cut has put anything on it — the window
/// every updater 302'd into. The exposure detector sees it (non-vacuity). The cutter
/// does not trust the engine to have read `BINARY_CUT_FOLLOWS_DEFAULT`: it REFUSES to
/// adopt a full source release (`channel::unclaimed_source_is_prerelease`), before the
/// bind — nothing bound, uploaded or patched. The remedy the refusal names (mark it a
/// prerelease, the pointer returns to the previous release) lets the resume converge.
#[test]
fn a_full_source_release_steals_latest_before_the_cut_and_the_cut_refuses_to_adopt_it() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(false);
    assert_eq!(
        github.exposures,
        vec![format!(
            "after create {TAG}: latest → {TAG} without its appcast pair"
        )],
        "the old engine's steal is exactly one exposure, at creation"
    );
    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast.clone(), files.clone());
    let err = channel::publish_on_channel(&mut channel).expect_err("a full source release");
    assert!(err.to_string().contains("is a FULL release"), "{err}");
    assert_eq!(github.binds, 0, "refused before the bind");
    assert_eq!(github.journal_release, None, "no adoption persisted");
    assert!(
        github.uploads.is_empty(),
        "nothing uploaded onto the live head"
    );
    assert!(github.head_patches.is_empty());

    // The remedy: mark it a prerelease; GitHub hands `latest` back to the newest full
    // release, the previous app release.
    github.releases[id].prerelease = true;
    github.latest = Some(0);
    let mut channel = FakeChannel::new(&mut github, id, local_appcast, files);
    channel::publish_on_channel(&mut channel).expect("the resume adopts the prerelease");
    assert_eq!(github.head_patches, vec![(true, true)]);
    assert_eq!(github.latest, Some(id));
}

/// WHAT THAT REFUSAL PROTECTS — the negative control, over the cutter's own classifier.
/// Adopted anyway, a FULL source release is the channel's PUBLISHED app release
/// ([`verify::ReleaseState::Published`]: not a prerelease, carrying an appcast) the moment
/// the appcast lands — with every upload of the cut done, and no proof of them, no second
/// floor read and no head PATCH yet. That is the state `--abandon` refuses (a floor
/// refusal after it can no longer be withdrawn) and the state a resume's `head_made`
/// reads as "the head is made" (it would skip the proof and the floor). The prerelease
/// the engine makes holds [`verify::ReleaseState::Unpublished`] through the same uploads,
/// until the PATCH.
#[test]
fn adopting_a_full_source_release_would_publish_it_before_any_proof() {
    for prerelease in [true, false] {
        let mut github = FakeGithub::with_previous_release();
        github.adopt_unchecked = true;
        let id = github.source_release(prerelease);
        let (_dir, files, local_appcast) = dist();
        let mut channel = FakeChannel::new(&mut github, id, local_appcast, files.clone());
        channel.ratchet().expect("the floors pass either way");
        channel.bind().expect("adopted unchecked");
        let ranked = ranked_upload_names(&files);
        for name in &ranked {
            let file = files
                .iter()
                .find(|file| file.file_name().unwrap().to_str() == Some(name.as_str()))
                .unwrap();
            channel.upload(file).unwrap();
        }
        assert!(github.head_patches.is_empty(), "no PATCH, no proof yet");
        assert_eq!(
            github.release_state(id),
            if prerelease {
                verify::ReleaseState::Unpublished
            } else {
                verify::ReleaseState::Published
            },
            "prerelease={prerelease}"
        );
    }
}

/// The live adopt path reads the flag as `gh api … --jq '.prerelease | tostring'`, and
/// only an exact `true`/`false` is a flag: nothing garbled reads as an answer.
#[test]
fn the_prerelease_flag_parses_exactly() {
    assert_eq!(publish::parse_prerelease_flag("true\n"), Ok(true));
    assert_eq!(publish::parse_prerelease_flag("false\n"), Ok(false));
    assert_eq!(publish::parse_prerelease_flag("false"), Ok(false));
    for garbled in ["", "\n", "null\n", "True\n", "true\n\n", " true\n"] {
        assert!(
            publish::parse_prerelease_flag(garbled).is_err(),
            "{garbled:?}"
        );
    }
}

/// NEGATIVE CONTROL for the ordering: a head PATCH sent before the appcast pair is up
/// exposes a head with nothing to install, and the pointer gate cannot fetch an
/// appcast that is not there.
#[test]
fn a_head_patch_before_the_appcast_pair_exposes_an_empty_head() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast, files.clone());
    channel.bind().unwrap();
    for file in files
        .iter()
        .filter(|f| channel::channel_upload_rank(f.file_name().unwrap().to_str().unwrap()) == 0)
    {
        channel.upload(file).unwrap();
    }
    channel.make_head().unwrap();
    let err = channel
        .prove_head()
        .expect_err("the pointer names this tag, but its appcast is not there");
    assert!(err.to_string().contains("404"), "{err}");
    assert_eq!(github.head_patches, vec![(false, false)]);
    assert_eq!(
        github.exposures,
        vec![format!(
            "after head PATCH: latest → {TAG} without its appcast pair"
        )]
    );
}

/// The draft path — a channel with no source release (a repo without `pub publish`)
/// — runs the same sequence and the same PATCH, and the draft is invisible until it.
#[test]
fn a_draft_this_cut_created_becomes_latest_through_the_same_patch() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.create(TAG, true, false, &[]);
    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast, files);
    channel::publish_on_channel(&mut channel).expect("draft path");
    assert_eq!(github.head_patches, vec![(true, true)]);
    assert!(!github.releases[id].draft);
    assert_eq!(github.latest, Some(id));
    assert!(github.exposures.is_empty(), "{:?}", github.exposures);
}

/// The head ratchet's table, over the fake: an older head passes, this cut passes
/// (an idempotent re-send), an empty channel passes; a newer tag, an equal tag
/// order, or an older tag carrying a build that is not lower all refuse. A cut OWNS
/// `latest`, so it must never take it from a NEWER release (`make_latest` obeys no
/// version order); the whole cut refusing there, before the bind, is
/// `a_newer_head_conforms_as_the_models_refusal`.
#[test]
fn the_head_ratchet_orders_by_tag_and_by_build() {
    let decide = |github: &FakeGithub| {
        publish::prove_channel_head_is_older(
            SLUG,
            TAG,
            BUILD,
            &mut || Ok(github.pointer()),
            &mut |url| Ok(github.get(url)),
        )
    };
    let older = FakeGithub::with_previous_release();
    decide(&older).expect("an older head is taken");

    let mut ours = FakeGithub::with_previous_release();
    ours.app_release(VERSION, BUILD);
    decide(&ours).expect("this cut already holding the head is an idempotent re-send");

    decide(&FakeGithub::default()).expect("an empty channel has no head to protect");

    let mut newer = FakeGithub::with_previous_release();
    newer.app_release("0.92.0", BUILD + 1);
    let err = decide(&newer).expect_err("a newer tag is never displaced");
    assert!(
        err.to_string()
            .contains("v0.92.0 holds alabsystems/aterm's `latest`"),
        "{err}"
    );

    let mut same_order = FakeGithub::with_previous_release();
    same_order.app_release("0.91.1", BUILD - 1);
    decide(&same_order).expect_err("a tag that sorts above this cut's is never displaced");

    let mut higher_build = FakeGithub::default();
    higher_build.app_release("0.90.0", BUILD);
    let err = decide(&higher_build).expect_err("an older tag with a build not below ours");
    assert!(err.to_string().contains("not below this cut's"), "{err}");

    // An older source-only FULL release left holding `latest` (a publish before source
    // releases were prereleases, with no cut after it) serves no appcast: no app head.
    let mut stranded = FakeGithub::with_previous_release();
    stranded.create("v0.90.5", false, false, &["SHA256SUMS", "SHA256SUMS.sig"]);
    decide(&stranded).expect("an older release with no appcast is no head to protect");
    // …but a NEWER one is refused on its tag before anything is fetched.
    let mut stranded_newer = FakeGithub::with_previous_release();
    stranded_newer.create("v0.92.0", false, false, &["SHA256SUMS", "SHA256SUMS.sig"]);
    decide(&stranded_newer).expect_err("a newer tag is refused whatever it carries");
}

/// A cut whose first pass fails after its head PATCH: the pointer gate's bound runs out
/// while GitHub's `releases/latest` cache still names the previous release (v0.93.0).
/// Returns the release's asset bytes as the PATCH left them.
fn first_pass_fails_at_the_pointer_gate(
    github: &mut FakeGithub,
    id: usize,
    files: &[PathBuf],
    local_appcast: &[u8],
) -> BTreeMap<String, Vec<u8>> {
    github.stale_pointer = github.latest;
    let mut channel = FakeChannel::new(github, id, local_appcast.to_vec(), files.to_vec());
    let err = channel::publish_on_channel(&mut channel).expect_err("the cache outlasts the gate");
    assert!(
        err.to_string()
            .contains(&format!("names {PREVIOUS}, not {TAG}")),
        "{err}"
    );
    assert_eq!(github.head_patches, vec![(true, true)], "the PATCH landed");
    assert_eq!(github.latest, Some(id));
    github.stale_pointer = None;
    github.releases[id].assets.clone()
}

/// A HEAD THE CUT MADE IS FINISHED, NEVER REFUSED. The first pass's PATCH landed and its
/// pointer gate ran out; a roster join since re-dressed the head with generation N+1,
/// and this cut's signed appcast carries N. The resume sees its head is made: no roster
/// floor read to refuse it, no upload to roll the join's roster back — only the PATCH,
/// re-sent (idempotent), and the read side — and it passes.
#[test]
fn a_resume_after_the_head_patch_uploads_nothing_and_reads_no_roster_floor() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    first_pass_fails_at_the_pointer_gate(&mut github, id, &files, &local_appcast);
    github.roster_join();
    let redressed = github.releases[id].assets.clone();
    let uploads = github.uploads.len();

    let mut channel = FakeChannel::new(&mut github, id, local_appcast, files);
    channel::publish_on_channel(&mut channel).expect("the resume finishes the head it made");
    assert_eq!(github.uploads.len(), uploads, "nothing uploaded");
    assert_eq!(
        github.releases[id].assets, redressed,
        "the join's roster stands"
    );
    assert_eq!(
        github.head_patches,
        vec![(true, true); 2],
        "the PATCH re-sent, the pair up"
    );
    assert_eq!(github.latest, Some(id));
    assert!(github.exposures.is_empty(), "{:?}", github.exposures);
}

/// NEGATIVE CONTROL — the resume before 2026-09-26, which re-entered the write half over
/// a head it made: the floors read again refuse a release the fleet already runs, and
/// with `--abandon` refusing a published release nothing could release the lease.
#[test]
fn a_resume_that_forgets_its_head_is_refused_by_the_join_since() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    first_pass_fails_at_the_pointer_gate(&mut github, id, &files, &local_appcast);
    github.roster_join();
    github.forget_head = true;
    let mut channel = FakeChannel::new(&mut github, id, local_appcast, files);
    let err = channel::publish_on_channel(&mut channel).expect_err("the floors refuse the head");
    assert!(
        err.to_string()
            .contains("this machine's roster is older than the channel's"),
        "{err}"
    );
    assert!(
        github.published(id),
        "…a release that IS the published head"
    );
}

/// A non-app release holding `latest` (an `atpkg-index-<n>` cut published as a full
/// release) leaves the channel with no app head: the head ratchet lets the cut take
/// `latest` from it, and the pointer gate — which demands the pointer name THIS cut —
/// refuses it until the cut has.
#[test]
fn a_non_app_release_holding_latest_is_taken_over() {
    let mut github = FakeGithub::with_previous_release();
    let index = github.create("atpkg-index-30", false, false, &["index.toml"]);
    assert_eq!(github.latest, Some(index));
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    {
        let channel = FakeChannel::new(&mut github, id, local_appcast.clone(), files.clone());
        let github = &*channel.github;
        let err = publish::prove_pointer_serves(
            SLUG,
            TAG,
            &local_appcast,
            &mut || Ok(github.pointer()),
            &mut |url| github.fetch(url),
            &mut |_| {},
        )
        .expect_err("the gate never calls a non-app head this cut");
        assert!(err.to_string().contains("not an app release"), "{err}");
    }
    let mut channel = FakeChannel::new(&mut github, id, local_appcast, files);
    channel::publish_on_channel(&mut channel).expect("no app head to protect");
    assert_eq!(github.latest, Some(id));
}

// ---------------------------------------------------------------------------
// The head PATCH body, as sent
// ---------------------------------------------------------------------------

/// The live PATCH's argv, byte for byte: `-F` for the two JSON booleans, `-f` for
/// `make_latest`, which GitHub types as a string enum.
#[test]
fn the_live_head_patch_sends_exactly_the_table() {
    assert_eq!(
        channel::head_patch_argv("repos/alabsystems/aterm/releases/7"),
        [
            "api",
            "--method",
            "PATCH",
            "repos/alabsystems/aterm/releases/7",
            "-F",
            "draft=false",
            "-F",
            "prerelease=false",
            "-f",
            "make_latest=true",
        ]
    );
}

/// NEGATIVE CONTROL for the PATCH BODY. The fake applies only the fields it is sent,
/// so the cutter's table minus `prerelease=false` leaves the adopted source release a
/// prerelease, GitHub moves nothing, and the cutter's own pointer gate refuses the cut.
/// A draft this cut created is no prerelease, so the same body still passes there: the
/// field exists for the adopt path, which is the path every aterm cut takes.
#[test]
fn without_prerelease_false_the_adopted_release_never_becomes_latest() {
    let without_prerelease: Vec<_> = channel::HEAD_PATCH
        .iter()
        .copied()
        .filter(|(name, _)| *name != "prerelease")
        .collect();
    assert_eq!(without_prerelease.len(), channel::HEAD_PATCH.len() - 1);

    let mut github = FakeGithub::with_previous_release();
    github.head_fields = Some(without_prerelease.clone());
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast.clone(), files.clone());
    let err =
        channel::publish_on_channel(&mut channel).expect_err("a prerelease cannot be made latest");
    assert!(
        err.to_string()
            .contains(&format!("names {PREVIOUS}, not {TAG}")),
        "{err}"
    );
    assert_eq!(github.head_patches.len(), 1, "the PATCH was sent");
    assert!(github.releases[id].prerelease);
    assert_ne!(github.latest, Some(id));

    let mut github = FakeGithub::with_previous_release();
    github.head_fields = Some(without_prerelease);
    let id = github.create(TAG, true, false, &[]);
    let mut channel = FakeChannel::new(&mut github, id, local_appcast, files);
    channel::publish_on_channel(&mut channel).expect("a draft needs no prerelease=false");
    assert_eq!(github.latest, Some(id));
}

/// THE OPT-IN TRAVELS WITH THE CUTTER. The engine creates the source release as a
/// prerelease only for a tree whose committed `publish/config.sh` declares
/// `BINARY_CUT_FOLLOWS_DEFAULT="true"`, read at the commit it publishes; what makes that
/// prerelease safe is this tree's head PATCH flipping it. So this tree holds both: the
/// declaration, in the one form the engine reads (`^KEY_DEFAULT="value"$`), and a PATCH
/// that sends `prerelease=false`. Dropping either half fails here.
#[test]
fn the_tree_that_declares_binary_cut_follows_flips_the_prerelease() {
    fn declared(config: &str) -> Vec<&str> {
        config
            .lines()
            .filter_map(|line| line.strip_prefix("BINARY_CUT_FOLLOWS_DEFAULT="))
            .collect()
    }
    assert_eq!(
        declared(include_str!("../../../../publish/config.sh")),
        ["\"true\""],
        "publish/config.sh declares the opt-in exactly once, as the engine parses it"
    );
    assert!(
        channel::HEAD_PATCH.contains(&("prerelease", HeadPatchValue::Bool(false))),
        "a tree that opts in must flip the prerelease it is given"
    );
    // The matcher is not vacuous: a config without the line declares nothing, and
    // any other value reads back as itself.
    assert!(
        declared("STAGING_REMOTE_DEFAULT=\"x\"\n# BINARY_CUT_FOLLOWS_DEFAULT=\"true\"\n")
            .is_empty()
    );
    assert_eq!(
        declared("BINARY_CUT_FOLLOWS_DEFAULT=\"false\"\n"),
        ["\"false\""]
    );
}

// ---------------------------------------------------------------------------
// Adopting a release the journal holds no intent for
// ---------------------------------------------------------------------------

/// The adoption table. A release under this cut's tag that the journal holds no intent
/// for is adopted ONLY as the engine's source release — with or without its roster pair,
/// or still empty. NEGATIVE rows, each another cut's release that must never be adopted:
/// a complete app release under this tag (every name this cut publishes, which no
/// journal of this cut claims), a partial one, another version's DMG, and a retired
/// container. Refused by name, before a byte is fetched.
#[test]
fn an_unclaimed_release_is_adopted_only_as_the_source_release() {
    let elected = channel::required_asset_names(VERSION, true, true);
    let source_pair = ["SHA256SUMS", "SHA256SUMS.sig"];
    let roster_pair = [
        aterm_update_core::roster::ROSTER_ASSET,
        aterm_update_core::roster::ROSTER_SIG_ASSET,
    ];
    let dmg = channel::dmg_asset_name(VERSION);
    let names = |parts: &[&[&str]]| -> Vec<String> {
        parts
            .iter()
            .flat_map(|part| part.iter().map(|name| (*name).to_string()))
            .collect()
    };
    let elected_refs: Vec<&str> = elected.iter().map(String::as_str).collect();
    // (label, release assets, `None` to adopt or the asset the refusal names)
    let rows: Vec<(&str, Vec<String>, Option<&str>)> = vec![
        (
            "the source release",
            names(&[&source_pair, &roster_pair]),
            None,
        ),
        (
            "a source release without its roster pair yet",
            names(&[&source_pair]),
            None,
        ),
        ("a source release with nothing on it yet", vec![], None),
        (
            "NEGATIVE: a complete app release no journal of this cut claims",
            names(&[&source_pair, &elected_refs]),
            Some("aterm-0.91.0-mac.zip"),
        ),
        (
            "NEGATIVE: a partial one, as a dead cut left it",
            names(&[&source_pair, &roster_pair, &[dmg.as_str()]]),
            Some("aterm-0.91.0.dmg"),
        ),
        (
            "NEGATIVE: another version's DMG",
            names(&[&source_pair, &["aterm-0.90.0.dmg"]]),
            Some("aterm-0.90.0.dmg"),
        ),
        (
            "NEGATIVE: a retired container",
            names(&[&source_pair, &["aterm-offline.dmg"]]),
            Some("aterm-offline.dmg"),
        ),
    ];
    for (label, assets, refused) in rows {
        match (channel::unclaimed_is_source_release(&assets), refused) {
            (Ok(()), None) => {}
            (Err(why), Some(app)) => assert_eq!(
                why,
                format!("carries {app}, which the engine's source release never does"),
                "{label}"
            ),
            (verdict, want) => panic!("{label}: {verdict:?}, wanted a refusal naming {want:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tier-1: the real sequence, stepped through the derived `ReleasePublishOnce`
// ---------------------------------------------------------------------------

/// [`FakeChannel`] with every call the real sequence makes stepped through the derived
/// model, and the fake GitHub's state projected onto the model's variables after every
/// step and every stutter. A call the model does not enable, or a projection that
/// disagrees with the model, is recorded and fails the call.
struct ModelBound<'g> {
    inner: FakeChannel<'g>,
    model: aterm_spec::derive::Model,
    state: aterm_spec::interp::State,
    actions: Vec<&'static str>,
    newer: Option<usize>,
    /// The environment's roster join, taken right after this model action.
    join_after: Option<&'static str>,
}

impl ModelBound<'_> {
    fn project(&self) -> [(&'static str, i64); 7] {
        let github = &*self.inner.github;
        let latest = match github.latest {
            Some(i) if i == self.inner.id => 1,
            Some(i) if Some(i) == self.newer => 2,
            _ => 0,
        };
        let joined = i64::from(github.roster_seq > CARRIED_ROSTER);
        let bound = i64::from(github.journal_release == Some(self.inner.id));
        let Some(ours) = github.releases.get(self.inner.id).filter(|r| !r.deleted) else {
            return [
                ("latest", latest),
                ("joined", joined),
                ("bound", bound),
                ("source", 0),
                ("prerelease", 0),
                ("sig_up", 0),
                ("toml_up", 0),
            ];
        };
        let has = |name: &str| i64::from(ours.assets.contains_key(name));
        [
            ("latest", latest),
            ("joined", joined),
            ("bound", bound),
            ("source", 1),
            ("prerelease", i64::from(ours.prerelease)),
            ("sig_up", has(manifest_out::MANIFEST_SIG_ASSET)),
            ("toml_up", has(manifest_out::MANIFEST_ASSET)),
        ]
    }

    fn agree(&self, after: &str) -> Result<()> {
        for (var, value) in self.project() {
            if self.state[var] != value {
                return Err(Error::new(format!(
                    "Tier-1: after {after}, the fake GitHub has {var}={value} but the model \
                     has {}",
                    self.state[var]
                )));
            }
        }
        Ok(())
    }

    fn step(&mut self, action: &'static str) -> Result<()> {
        if !self.model.action_enabled(action, &self.state) {
            return Err(Error::new(format!(
                "Tier-1: the real sequence took {action}, which the model does not enable \
                 at {:?}",
                self.state
            )));
        }
        let before = self.state.clone();
        self.state = self.model.successors(action, &before)[0].clone();
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &self.model,
            &[],
            &before,
            &self.state,
            Some(action),
            action,
        );
        if !admitted {
            return Err(Error::new(format!("Tier-1: {action} not admitted: {why}")));
        }
        self.actions.push(action);
        self.agree(action)?;
        if self.join_after == Some(action) {
            self.join_after = None;
            self.join()?;
        }
        Ok(())
    }

    /// The environment's `RosterJoin`, on the fake and the model together.
    fn join(&mut self) -> Result<()> {
        self.inner.github.roster_join();
        self.step("RosterJoin")
    }

    /// A new process resuming the journal: nothing is bound in memory, the model's state
    /// (and the fake's journal) carry over.
    fn restart(&mut self) {
        self.inner.bound = false;
    }
}

impl ChannelRelease for ModelBound<'_> {
    fn head_made(&mut self) -> Result<bool> {
        let made = self.inner.head_made()?;
        // The real predicate against the model's history, both ways: the resume finishes
        // exactly the heads the cut made.
        if made != (self.state["headed"] == 1) {
            return Err(Error::new(format!(
                "Tier-1: head_made answered {made}, but the model has headed={}",
                self.state["headed"]
            )));
        }
        self.agree("head_made")?;
        Ok(made)
    }

    fn artifacts(&mut self) -> Result<Vec<PathBuf>> {
        self.agree("artifacts")?;
        self.inner.artifacts()
    }

    fn bind(&mut self) -> Result<()> {
        self.inner.bind()?;
        self.step("Bind")
    }

    fn upload(&mut self, file: &Path) -> Result<()> {
        self.inner.upload(file)?;
        match file.file_name().and_then(|n| n.to_str()) {
            Some(name) if name == manifest_out::MANIFEST_SIG_ASSET => self.step("UploadSig"),
            Some(name) if name == manifest_out::MANIFEST_ASSET => self.step("UploadToml"),
            _ => self.agree("an unmodelled upload"),
        }
    }

    fn prove_assets(&mut self) -> Result<()> {
        self.inner.prove_assets()?;
        self.step("ProveAssets")
    }

    fn ratchet_head(&mut self) -> Result<()> {
        // The head half alone reads, and moves nothing the model tracks.
        if let Err(refused) = self.inner.head_half() {
            self.step("RefuseNewer")?;
            return Err(refused);
        }
        self.agree("ratchet_head")
    }

    fn ratchet(&mut self) -> Result<()> {
        // The real ratchet's two halves, each refusal stepped as the model's own.
        if let Err(refused) = self.inner.head_half() {
            self.step("RefuseNewer")?;
            return Err(refused);
        }
        if let Err(refused) = self.inner.roster_half() {
            self.step("RefuseFloor")?;
            return Err(refused);
        }
        self.step("Ratchet")
    }

    fn make_head(&mut self) -> Result<()> {
        self.inner.make_head()?;
        // One PATCH, two model actions: the head PATCH, or — over a head this cut
        // already made (`head_made` agreed with the model's `headed`) — its re-send.
        if self.state["headed"] == 1 {
            self.step("ReassertHead")
        } else {
            self.step("MakeHead")
        }
    }

    fn prove_head(&mut self) -> Result<()> {
        self.agree("prove_head")?;
        self.inner.prove_head()?;
        self.step("ProveHead")
    }
}

/// Bind `id` (the source release) to a model state that has taken the environment's
/// steps: `PublishSource`, optionally `NewerRelease`, and the cut's `AcquireLease`.
fn model_bound<'g>(
    github: &'g mut FakeGithub,
    id: usize,
    newer: Option<usize>,
    local_appcast: Vec<u8>,
    files: Vec<PathBuf>,
) -> ModelBound<'g> {
    let model = aterm_spec::derive::release_publish_once_model();
    let mut state = model.init_state();
    let mut environment = vec!["PublishSource"];
    if newer.is_some() {
        environment.push("NewerRelease");
    }
    environment.push("AcquireLease");
    for action in environment {
        assert!(model.fire(action, &mut state), "{action}");
    }
    let bound = ModelBound {
        inner: FakeChannel::new(github, id, local_appcast, files),
        model,
        state,
        actions: Vec::new(),
        newer,
        join_after: None,
    };
    bound
        .agree("binding")
        .expect("the environment projects onto the model");
    bound
}

/// TIER-1. The real `publish_on_channel`, against the fake, conforms to the derived
/// model step for step: the ratchet, the bind, the signature, the appcast, the proof,
/// the ratchet again, the head PATCH, the stranger's view — every one enabled where it
/// was taken, and the fake's `latest`, prerelease flag, release, journal, roster and
/// appcast pair equal to the model's after each.
#[test]
fn the_real_sequence_conforms_to_the_derived_head_model() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut bound = model_bound(&mut github, id, None, local_appcast, files);
    channel::publish_on_channel(&mut bound).expect("conforms");
    assert_eq!(
        bound.actions,
        [
            "Ratchet",
            "Bind",
            "UploadSig",
            "UploadToml",
            "ProveAssets",
            "Ratchet",
            "MakeHead",
            "ProveHead"
        ]
    );
    assert_eq!(bound.state["phase"], 6);
}

/// TIER-1, the refusal: a newer release holding `latest` is the model's
/// `RefuseNewer`, taken by the real ratchet on its first call.
#[test]
fn a_newer_head_conforms_as_the_models_refusal() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let newer = github.app_release("0.92.0", BUILD + 1);
    let (_dir, files, local_appcast) = dist();
    let mut bound = model_bound(&mut github, id, Some(newer), local_appcast, files);
    channel::publish_on_channel(&mut bound).expect_err("refused");
    assert_eq!(bound.actions, ["RefuseNewer"]);
    assert_eq!(bound.state["phase"], 4);
    assert_eq!(bound.state["bound"], 0);
    assert_eq!(github.binds, 0, "the refused cut bound nothing");
}

/// TIER-1, THE RESUME AFTER THE HEAD. The first pass is the model's up to `MakeHead`;
/// its pointer gate runs out (no model step: the cut stops, the journal stays at
/// `publish`); an `atpkg-index` full release takes `latest` (`OtherTakesLatest`) and a
/// join re-dresses the channel (`RosterJoin` in phase 3); and the resume — a new process,
/// the model's state carried over — answers `head_made` exactly as the model's `headed`
/// does, and takes `ReassertHead` and `ProveHead` alone. NEGATIVE CONTROL: the resume
/// that forgets its head disagrees with the model at the first question it asks.
#[test]
fn a_resume_after_the_head_conforms_as_the_models_proof_alone() {
    for forget_head in [false, true] {
        let mut github = FakeGithub::with_previous_release();
        let id = github.source_release(true);
        let (_dir, files, local_appcast) = dist();
        github.stale_pointer = github.latest;
        let mut bound = model_bound(&mut github, id, None, local_appcast, files);
        channel::publish_on_channel(&mut bound).expect_err("the pointer gate runs out");
        assert_eq!(bound.actions.last(), Some(&"MakeHead"));
        assert_eq!(bound.state["phase"], 3);
        bound.inner.github.stale_pointer = None;
        bound
            .inner
            .github
            .create("atpkg-index-31", false, false, &["index.toml"]);
        bound
            .step("OtherTakesLatest")
            .expect("a full release after the PATCH takes `latest`, in the model too");
        bound
            .join()
            .expect("a join after the PATCH is the model's too");
        bound.inner.github.forget_head = forget_head;
        bound.restart();
        let resumed = channel::publish_on_channel(&mut bound);
        if forget_head {
            let err = resumed.expect_err("the forgotten head is not the model's");
            assert!(
                err.to_string()
                    .contains("head_made answered false, but the model has headed=1"),
                "{err}"
            );
            continue;
        }
        resumed.expect("the resume conforms");
        assert_eq!(
            bound.actions[bound.actions.len() - 4..],
            [
                "OtherTakesLatest",
                "RosterJoin",
                "ReassertHead",
                "ProveHead"
            ]
        );
        assert_eq!(bound.state["phase"], 6);
        assert_eq!(bound.inner.github.latest, Some(id));
    }
}

/// TIER-1, THE SECOND FLOOR READ. A join lands while the cut uploads (after the
/// signature, before the appcast); the read after the proof is the model's
/// `RefuseFloor`, from a release carrying every upload but not yet the head. The
/// `--abandon` that follows withdraws exactly those uploads — the real
/// `channel::withdrawable_assets` over the fake's upload log — and conforms to the
/// model's `Abandon` from the refusal, the engine's release left as it made it.
#[test]
fn a_join_during_the_uploads_is_refused_by_the_second_read_and_withdrawn() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut bound = model_bound(&mut github, id, None, local_appcast, files);
    bound.join_after = Some("UploadSig");
    let err = channel::publish_on_channel(&mut bound).expect_err("the second read refuses");
    assert!(
        err.to_string()
            .contains("this machine's roster is older than the channel's"),
        "{err}"
    );
    assert_eq!(
        bound.actions,
        [
            "Ratchet",
            "Bind",
            "UploadSig",
            "RosterJoin",
            "UploadToml",
            "ProveAssets",
            "RefuseFloor"
        ]
    );
    assert!(bound.inner.github.head_patches.is_empty(), "never the head");
    assert_eq!(
        bound.inner.github.latest,
        Some(0),
        "the previous release keeps it"
    );

    let uploaded = bound.inner.github.uploads.clone();
    let release = &mut bound.inner.github.releases[id];
    let names: Vec<String> = release.assets.keys().cloned().collect();
    for name in channel::withdrawable_assets(&names, VERSION, Some(&uploaded)).unwrap() {
        release.assets.remove(&name);
    }
    bound.inner.github.journal_release = None;
    bound
        .step("Abandon")
        .expect("the withdrawal after a refusal conforms to the model's abandon");
    assert_eq!(bound.state["phase"], 5);
    assert!(bound.inner.github.releases[id].prerelease);
}

/// NEGATIVE CONTROLS: the binding catches what it exists to catch.
/// * the bind before the floors (the adopt path before 2026-09-23 persisted the
///   adoption and bound the release first) takes a `Bind` the model disables;
/// * the upload order the sequence would have without its rank sort (alphabetical:
///   the appcast before its signature) takes an `UploadToml` the model disables;
/// * a head PATCH over bytes nothing proved — what the origin's verify stood in front
///   of while a cut published twice — takes a `MakeHead` the model disables;
/// * a head PATCH on the floor read BEFORE the uploads, a join having landed since,
///   takes a `MakeHead` the model disables;
/// * the adopt path before 2026-09-23 (no head PATCH reaching GitHub) leaves the fake's
///   `latest` where the model says the head PATCH moved it.
#[test]
fn the_tier1_binding_refuses_the_pre_fix_orders() {
    let (_dir, files, local_appcast) = dist();
    let ranked = {
        let mut ranked = files.clone();
        ranked.sort_by_key(|file| {
            channel::channel_upload_rank(file.file_name().unwrap().to_str().unwrap())
        });
        ranked
    };
    let fresh = |github: &mut FakeGithub| github.source_release(true);

    let mut github = FakeGithub::with_previous_release();
    let id = fresh(&mut github);
    let mut bound = model_bound(&mut github, id, None, local_appcast.clone(), files.clone());
    let err = bound.bind().expect_err("the bind before the ratchet");
    assert!(
        err.to_string()
            .contains("took Bind, which the model does not enable"),
        "{err}"
    );

    let mut github = FakeGithub::with_previous_release();
    let id = fresh(&mut github);
    let mut bound = model_bound(&mut github, id, None, local_appcast.clone(), files.clone());
    bound.ratchet().unwrap();
    bound.bind().unwrap();
    let err = files
        .iter()
        .try_for_each(|file| bound.upload(file))
        .expect_err("alphabetical order uploads the appcast first");
    assert!(
        err.to_string()
            .contains("took UploadToml, which the model does not enable"),
        "{err}"
    );

    let mut github = FakeGithub::with_previous_release();
    let id = fresh(&mut github);
    let mut bound = model_bound(&mut github, id, None, local_appcast.clone(), files.clone());
    bound.ratchet().unwrap();
    bound.bind().unwrap();
    for file in &ranked {
        bound.upload(file).unwrap();
    }
    bound.ratchet().unwrap();
    let err = bound
        .make_head()
        .expect_err("a head PATCH over unproved bytes");
    assert!(
        err.to_string()
            .contains("took MakeHead, which the model does not enable"),
        "{err}"
    );

    let mut github = FakeGithub::with_previous_release();
    let id = fresh(&mut github);
    let mut bound = model_bound(&mut github, id, None, local_appcast.clone(), files.clone());
    bound.join_after = Some("UploadSig");
    bound.ratchet().unwrap();
    bound.bind().unwrap();
    for file in &ranked {
        bound.upload(file).unwrap();
    }
    bound.prove_assets().unwrap();
    let err = bound
        .make_head()
        .expect_err("a head PATCH on the floor read before the join");
    assert!(
        err.to_string()
            .contains("took MakeHead, which the model does not enable"),
        "{err}"
    );

    let mut github = FakeGithub::with_previous_release();
    github.drop_head_patch = true;
    let id = fresh(&mut github);
    let mut bound = model_bound(&mut github, id, None, local_appcast, files);
    let err = channel::publish_on_channel(&mut bound).expect_err("no PATCH");
    assert!(
        err.to_string()
            .contains("after MakeHead, the fake GitHub has latest=0 but the model has 1"),
        "{err}"
    );
}

/// TIER-1, the abandon. A cut that never made its release the head withdraws exactly
/// what it uploaded — the real `channel::withdrawable_assets` over its journal's upload
/// intents — and the fake then projects onto the model's `Abandon`: the engine's release
/// is still there, still a prerelease, and carries none of the cut's app assets (the
/// source shapes stay). NEGATIVE CONTROL: deleting the release instead — what abandon
/// did to the private origin's draft while a cut published twice — leaves the fake with
/// no release where the model keeps the engine's.
#[test]
fn an_abandon_withdraws_the_cuts_assets_and_leaves_the_source_release() {
    for delete_instead in [false, true] {
        let mut github = FakeGithub::with_previous_release();
        let id = github.source_release(true);
        let (_dir, files, local_appcast) = dist();
        let mut bound = model_bound(&mut github, id, None, local_appcast, files.clone());
        bound.ratchet().unwrap();
        bound.bind().unwrap();
        let mut ranked = files;
        ranked.sort_by_key(|file| {
            channel::channel_upload_rank(file.file_name().unwrap().to_str().unwrap())
        });
        // The cut dies with everything up but the appcast.
        for file in ranked
            .iter()
            .filter(|file| !file.ends_with(manifest_out::MANIFEST_ASSET))
        {
            bound.upload(file).unwrap();
        }
        let uploaded = bound.inner.github.uploads.clone();
        bound.inner.github.journal_release = None;
        let release = &mut bound.inner.github.releases[id];
        if delete_instead {
            release.deleted = true;
        } else {
            let names: Vec<String> = release.assets.keys().cloned().collect();
            for name in channel::withdrawable_assets(&names, VERSION, Some(&uploaded)).unwrap() {
                release.assets.remove(&name);
            }
        }
        let outcome = bound.step("Abandon");
        if delete_instead {
            let err = outcome.expect_err("a deleted release is not the model's abandon");
            assert!(
                err.to_string()
                    .contains("the fake GitHub has source=0 but the model has 1"),
                "{err}"
            );
            continue;
        }
        outcome.expect("the withdrawal conforms to the model's abandon");
        let release = &bound.inner.github.releases[id];
        assert!(release.prerelease && !release.draft);
        let mut left: Vec<&str> = release.assets.keys().map(String::as_str).collect();
        left.sort_unstable();
        assert_eq!(
            left,
            [
                "SHA256SUMS",
                "SHA256SUMS.sig",
                aterm_update_core::roster::ROSTER_ASSET,
                aterm_update_core::roster::ROSTER_SIG_ASSET,
            ],
            "exactly the engine's source shapes remain"
        );
        assert_eq!(bound.state["source"], 1);
    }
}
