// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cut publishes once and owns `latest`, proved against a fake GitHub.

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

/// A valid appcast for `version`, its `url` binding it to its tag and DMG as the web-lane
/// client demands.
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
    /// `DELETE /releases/{id}`, set only by a negative control.
    deleted: bool,
    assets: BTreeMap<String, Vec<u8>>,
}

/// GitHub, as far as `latest` is concerned.
#[derive(Default)]
struct FakeGithub {
    releases: Vec<Release>,
    /// Index into `releases` of the release GitHub calls `latest`.
    latest: Option<usize>,
    /// Negative control: the cutter sends no head PATCH at all.
    drop_head_patch: bool,
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
    /// GitHub's `releases/latest` cache: while set, the pointer names this release whatever
    /// `latest` is.
    stale_pointer: Option<usize>,
    /// Negative control: a resume that does not ask whether its head PATCH landed.
    forget_head: bool,
    /// The next upload POST of this name fails, once, the way [`UploadFailure`] says.
    fail_upload_once: Option<(String, UploadFailure)>,
    /// The journal's durable upload intents (`Journal::upload_intents`): persisted before every
    /// upload POST, retracted only when [`publish::post_outcome`] says it created nothing.
    upload_intents: Vec<String>,
    /// Negative control: a bind that adopts whatever release is under the tag, a full source
    /// release included.
    adopt_unchecked: bool,
}

/// How one upload POST fails, as curl reports it to the live cutter.
#[derive(Debug, Clone, Copy)]
enum UploadFailure {
    /// curl refused its own arguments (exit 2): nothing left the machine.
    NeverSent,
    /// The server answered 422: understood, refused, nothing created.
    Refused,
    /// A 502 from the edge: whether GitHub stored the object is unknowable (here it did not).
    ServerError,
    /// 422 `already_exists` for an unjournaled object the pre-POST probe could not see yet:
    /// the POST created nothing, and the object is there.
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

    /// `atpkg-keys join`: the fleet's roster moves to the next generation and the join
    /// re-dresses the channel head with the new pair, with no lease or re-signed manifest.
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

    /// Is this the channel's published app release, which only a head PATCH makes of an
    /// adopted prerelease or a draft (`verify::ReleaseState::Published`)?
    fn published(&self, id: usize) -> bool {
        let release = &self.releases[id];
        !release.draft
            && !release.prerelease
            && !release.deleted
            && release.assets.contains_key(manifest_out::MANIFEST_ASSET)
    }

    /// What [`verify::classify_release_state`] (read by `--abandon` and `head_made`) makes of
    /// release `id`, over the row `release_metadata_listing` would emit.
    fn release_state(&self, id: usize) -> verify::ReleaseState {
        let release = &self.releases[id];
        let appcast = usize::from(release.assets.contains_key(manifest_out::MANIFEST_ASSET));
        let row = format!(
            "{id}\t{}\t{}\t{}\t{appcast}\n",
            release.tag, release.draft, release.prerelease
        );
        verify::classify_release_state(&row, &release.tag).expect("a well-formed row")
    }

    /// A complete app release, published as a full release, so it takes `latest`.
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

    /// `POST /releases`.
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

    /// `pub publish`'s sign-release: the source release with its attestation and roster pairs.
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

    /// `PATCH /releases/{id}` with exactly the fields sent, the cutter's own
    /// [`channel::HEAD_PATCH`].
    fn head_patch(&mut self, id: usize) {
        let fields = channel::HEAD_PATCH.to_vec();
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

    /// What `releases/latest/download/aterm-appcast.toml` answers a stranger, read as the
    /// deployed client reads it (a tag outside its grammar is not an app).
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

/// The cut's view of its channel release, through the trait the real step drives.
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

    /// The head half of the ratchet, by the cutter's own function over what the fake serves.
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

    /// The roster half, by the cutter's own floor: this cut's generation against the fleet's.
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
            let stored = self.github.releases[self.id].assets.contains_key(&name);
            let intents = &mut self.github.upload_intents;
            let settle = || {
                if outcome != publish::PostOutcome::Unknown {
                    intents.retain(|issued| *issued != name);
                }
                Ok(outcome)
            };
            match publish::after_upload_post(settle, || Ok(stored))? {
                publish::AfterUploadPost::Converge { journal } => {
                    // Its bytes are ours by construction; the journal names it again if the
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

/// The engine's prerelease source release never takes `latest`; the cut adopts it, uploads the
/// appcast signature and then the appcast last, sends the head PATCH once both are up, and the
/// pointer gate passes, with `latest` never naming a release a client could not install from.
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

/// This cut's upload names in the order the sequence sends them (the stable rank sort).
fn ranked_upload_names(files: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = files
        .iter()
        .map(|f| f.file_name().unwrap().to_str().unwrap().to_string())
        .collect();
    names.sort_by_key(|name| channel::channel_upload_rank(name));
    names
}

/// The adopt path's first pass, dying on the appcast signature's upload POST as `failure`
/// says, with every binary already up.
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

/// A failure before the head PATCH costs the fleet nothing, and when the POST provably created
/// nothing (curl's exit 2, an answered 4xx) its intent is retracted and the resume flips once:
/// it re-binds the same release, re-sends the ranked sequence (same-bytes no-ops), puts the
/// pair up signature-first and sends one head PATCH over both passes.
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

/// A refused upload can still find its object there, and the journal must then name it:
/// `422 already_exists` is a 4xx, so the refused POST's intent is retracted before the probe,
/// and an unjournaled object could not be withdrawn by `--abandon`.
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

/// GitHub recomputes `latest` behind a cache, so the pointer gate waits on its own bound
/// (`POINTER_PROBE_ATTEMPTS`): a lag past ten probes converges, and a pointer that never moves
/// is refused after exactly that bound.
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

    let lag = 12;
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

/// Negative control: an adopt path with no head PATCH leaves the prerelease a prerelease and
/// the pointer on the previous release, and the pointer gate refuses.
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

/// A full source release takes `latest` at creation, before the cut has put anything on it
/// (hence the engine's prerelease).
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

    github.releases[id].prerelease = true;
    github.latest = Some(0);
    let mut channel = FakeChannel::new(&mut github, id, local_appcast, files);
    channel::publish_on_channel(&mut channel).expect("the resume adopts the prerelease");
    assert_eq!(github.head_patches, vec![(true, true)]);
    assert_eq!(github.latest, Some(id));
}

/// What that refusal protects: adopted anyway, a full source release is
/// [`verify::ReleaseState::Published`] the moment the appcast lands, before any proof, second
/// floor read or head PATCH, a state `--abandon` refuses and `head_made` reads as made.
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

/// The live adopt path reads the flag as `jq`'s `.prerelease | tostring` spells it: only an exact `true`/`false` is a flag.
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

/// The draft path (no source release, a repo without `pub publish`) runs the same sequence and
/// PATCH, and the draft is invisible until it.
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

/// The head ratchet's table: an older head, this cut (an idempotent re-send) and an empty
/// channel pass; a newer tag, an equal tag order, or an older tag with a build not lower
/// refuse.
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
    assert!(err.to_string().contains("v0.92.0"), "{err}");

    let mut same_order = FakeGithub::with_previous_release();
    same_order.app_release("0.91.1", BUILD - 1);
    decide(&same_order).expect_err("a tag that sorts above this cut's is never displaced");

    let mut higher_build = FakeGithub::default();
    higher_build.app_release("0.90.0", BUILD);
    let err = decide(&higher_build).expect_err("an older tag with a build not below ours");
    assert!(err.to_string().contains("not below this cut's"), "{err}");

    let mut stranded = FakeGithub::with_previous_release();
    stranded.create("v0.90.5", false, false, &["SHA256SUMS", "SHA256SUMS.sig"]);
    decide(&stranded).expect("an older release with no appcast is no head to protect");
    let mut stranded_newer = FakeGithub::with_previous_release();
    stranded_newer.create("v0.92.0", false, false, &["SHA256SUMS", "SHA256SUMS.sig"]);
    decide(&stranded_newer).expect_err("a newer tag is refused whatever it carries");
}

/// A first pass that fails after its head PATCH: the pointer gate's bound runs out while
/// GitHub's `releases/latest` cache still names the previous release.
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

/// A head the cut made is finished, never refused: after its PATCH landed and the pointer gate
/// ran out, a roster join re-dressed the head with a newer generation.
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

/// Negative control: a resume that re-enters the write half over a head it made has the floors
/// refuse a release the fleet already runs, and `--abandon` refuses a published release, so
/// nothing could release the lease.
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
    assert!(err.to_string().contains("roster"), "{err}");
    assert!(
        github.published(id),
        "…a release that IS the published head"
    );
}

/// A non-app release holding `latest` (an `atpkg-index-<n>` full release) is no app head: the
/// head ratchet lets the cut take `latest` from it, and the pointer gate, which demands this
/// cut, refuses until it has.
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

/// The live PATCH's JSON body, byte for byte: two JSON booleans and the string `make_latest`.
#[test]
fn the_live_head_patch_sends_exactly_the_table() {
    assert_eq!(
        channel::head_patch_body().to_string(),
        r#"{"draft":false,"make_latest":"true","prerelease":false}"#
    );
}

/// The adoption table: a release under this cut's tag that the journal holds no intent for is
/// adopted only as the engine's source release (with or without its roster pair, or empty).
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
            (Err(why), Some(app)) => assert!(why.contains(app), "{label}: {why}"),
            (verdict, want) => panic!("{label}: {verdict:?}, wanted a refusal naming {want:?}"),
        }
    }
}

/// [`FakeChannel`] with every call stepped through the derived model and the fake's state
/// projected onto the model's variables after each step and stutter; a call the model does not
/// enable, or a projection that disagrees, fails the call.
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
    let model = aterm_models_release::release_publish_once_model();
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

/// Tier-1: the real `publish_on_channel` against the fake conforms to the derived model step
/// for step, each step enabled where taken and the fake's `latest`, prerelease flag, release,
/// journal, roster and appcast pair equal to the model's after each.
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

/// Tier-1, the refusal: a newer release holding `latest` is the model's `RefuseNewer`, taken by
/// the real ratchet on its first call.
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

/// Tier-1, the resume after the head: the first pass runs to `MakeHead` and its pointer gate
/// runs out; a full release takes `latest` (`OtherTakesLatest`) and a join re-dresses the
/// channel (`RosterJoin`); the resume answers `head_made` as the model's `headed` and takes
/// `ReassertHead` and `ProveHead` alone.
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

/// Tier-1, the second floor read: a join lands while the cut uploads (after the signature,
/// before the appcast), so the read after the proof is the model's `RefuseFloor`.
#[test]
fn a_join_during_the_uploads_is_refused_by_the_second_read_and_withdrawn() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut bound = model_bound(&mut github, id, None, local_appcast, files);
    bound.join_after = Some("UploadSig");
    let err = channel::publish_on_channel(&mut bound).expect_err("the second read refuses");
    assert!(err.to_string().contains("roster"), "{err}");
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

/// Negative controls: the binding refuses the bind before the floors (`Bind`), the appcast
/// uploaded before its signature (`UploadToml`), a head PATCH over unproved bytes or on a floor
/// read a join has since overtaken (`MakeHead`), and a head PATCH that never reaches GitHub
/// (the fake's `latest` disagrees with the model's).
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

/// Tier-1, the abandon: a cut that never made its release the head withdraws exactly what it
/// uploaded (`channel::withdrawable_assets` over its journal's upload intents), and the fake
/// projects onto the model's `Abandon`: the engine's release is still there, still a
/// prerelease, with only its source shapes.
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
