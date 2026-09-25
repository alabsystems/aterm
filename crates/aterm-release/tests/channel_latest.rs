// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CUT OWNS `latest` — proved against a fake GitHub.
//!
//! `pub publish` creates aterm's vX.Y.0 release on the public channel first, as a
//! SOURCE release, and the cut's `mirror` step ADOPTS it and puts the app on it.
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
//! [`mirror::publish_on_channel`] sends one guarded head PATCH ([`mirror::HEAD_PATCH`]:
//! `draft=false`, `prerelease=false`, `make_latest=true`) on every path, after the
//! appcast pair is up. The fake below models the three GitHub rules that decide
//! `latest` — a newly published full release takes it, a prerelease never can,
//! `make_latest=true` on a full release takes it — applies exactly the PATCH fields
//! the cutter's own table sends, and records every moment at which `latest` named a
//! release a client could not install from. The pointer verdict is the cutter's own
//! gate, [`publish::prove_pointer_serves`], fed from the fake.

// The release crate is a binary on purpose (the spec's §9 file plan has no
// lib.rs), so the integration tests compile the modules under test directly.
#[path = "../src/apple.rs"]
#[allow(dead_code)]
mod apple;
#[path = "../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../src/dmg.rs"]
#[allow(dead_code)]
mod dmg;
#[path = "../src/gates.rs"]
#[allow(dead_code)]
mod gates;
#[path = "../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../src/machines.rs"]
#[allow(dead_code)]
mod machines;
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../src/mirror.rs"]
#[allow(dead_code)]
mod mirror;
#[path = "../src/provision.rs"]
#[allow(dead_code)]
mod provision;
#[path = "../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../src/verify.rs"]
#[allow(dead_code)]
mod verify;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ledger::{Error, Result};
use mirror::{ChannelRelease, HeadPatchValue, UnclaimedChannelRelease};
use publish::PointerProbe;

const OWNER: &str = "alabsystems";
const REPO: &str = "aterm";
const SLUG: &str = "alabsystems/aterm";
const PREVIOUS: &str = "v0.90.0";
const TAG: &str = "v0.91.0";
const VERSION: &str = "0.91.0";
const PREVIOUS_BUILD: u64 = 1_789_000_000;
const BUILD: u64 = 1_789_100_000;

fn download_url(tag: &str, name: &str) -> String {
    aterm_update_core::cdn::release_download_url(OWNER, REPO, tag, name)
        .expect("canonical names build a download URL")
}

/// A valid appcast for `version`, whose `url` binds it to its tag and DMG exactly as
/// the web-lane client demands.
fn appcast(version: &str, build: u64) -> Vec<u8> {
    let tag = format!("v{version}");
    let dmg = mirror::dmg_asset_name(version);
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
    /// [`mirror::HEAD_PATCH`] (a negative control).
    head_fields: Option<Vec<(&'static str, HeadPatchValue)>>,
    /// Every time the cut bound its channel release.
    binds: usize,
    /// Every upload name, in order.
    uploads: Vec<String>,
    /// For each head PATCH: were the appcast and its signature on the release?
    head_patches: Vec<(bool, bool)>,
    /// Every moment `latest` named a release a client cannot install from.
    exposures: Vec<String>,
}

impl FakeGithub {
    /// The channel as it stands when a release is cut: the previous app release is
    /// complete and holds `latest`.
    fn with_previous_release() -> Self {
        let mut github = Self::default();
        github.app_release("0.90.0", PREVIOUS_BUILD);
        github
    }

    /// A complete app release, published as a full release — so it takes `latest`.
    fn app_release(&mut self, version: &str, build: u64) -> usize {
        let mut assets = BTreeMap::new();
        for name in mirror::required_asset_names(version, true, true) {
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
    /// [`mirror::HEAD_PATCH`] unless a test replaced it. GitHub's rule: `make_latest=true`
    /// makes a FULL release `latest`, and moves nothing for a draft or a prerelease.
    fn head_patch(&mut self, id: usize) {
        let fields = self
            .head_fields
            .clone()
            .unwrap_or_else(|| mirror::HEAD_PATCH.to_vec());
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
        match self.latest {
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
            .filter(|release| !release.draft)
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
    bound: bool,
}

impl<'a> FakeChannel<'a> {
    fn new(github: &'a mut FakeGithub, id: usize, local_appcast: Vec<u8>) -> Self {
        Self {
            github,
            id,
            local_appcast,
            bound: false,
        }
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
    fn bind(&mut self) -> Result<()> {
        self.github.binds += 1;
        self.bound = true;
        Ok(())
    }

    fn upload(&mut self, file: &Path) -> Result<()> {
        self.bound()?;
        let name = file.file_name().unwrap().to_str().unwrap().to_string();
        let bytes = std::fs::read(file).map_err(|e| Error::new(e.to_string()))?;
        self.github.uploads.push(name.clone());
        let release = &mut self.github.releases[self.id];
        match release.assets.get(&name) {
            Some(existing) if *existing == bytes => {}
            // The source release's roster copy may be stale: the one permitted
            // replacement (delete by ID, upload this cut's bytes).
            Some(_)
                if name == aterm_update_core::roster::ROSTER_ASSET
                    || name == aterm_update_core::roster::ROSTER_SIG_ASSET =>
            {
                release.assets.insert(name.clone(), bytes);
            }
            Some(_) => return Err(Error::new(format!("{name} exists with other bytes"))),
            None => {
                release.assets.insert(name.clone(), bytes);
            }
        }
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
        mirror::validate_mirror_asset_set_with_linux(&names, VERSION, true, true, &[])
    }

    fn ratchet(&mut self) -> Result<()> {
        // The head half of the ratchet, decided by the cutter's own function over what
        // the fake serves a stranger. (The roster half reads a signed roster document;
        // `machine_roster.rs` owns its proofs.)
        let github = &*self.github;
        publish::prove_channel_head_is_older(
            SLUG,
            TAG,
            BUILD,
            &mut || Ok(github.pointer()),
            &mut |url| Ok(github.get(url)),
        )
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
fn dist() -> (tempdir::Dir, Vec<PathBuf>, Vec<u8>) {
    let dir = tempdir::Dir::new();
    let local_appcast = appcast(VERSION, BUILD);
    let mut files = Vec::new();
    for name in mirror::required_asset_names(VERSION, true, true) {
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

mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    pub struct Dir(PathBuf);

    impl Dir {
        pub fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "aterm-channel-latest-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
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
    let mut channel = FakeChannel::new(&mut github, id, local_appcast);
    mirror::publish_on_channel(&mut channel, files)
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
    let mut channel = FakeChannel::new(&mut github, id, local_appcast);
    let err =
        mirror::publish_on_channel(&mut channel, files).expect_err("no PATCH, no latest, no pass");
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
/// every updater 302'd into. The exposure detector sees it (non-vacuity), and the
/// cut still converges: its head PATCH is idempotent.
#[test]
fn a_full_source_release_steals_latest_before_the_cut_and_the_cut_still_converges() {
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
    let mut channel = FakeChannel::new(&mut github, id, local_appcast);
    mirror::publish_on_channel(&mut channel, files).expect("the cut converges either way");
    assert_eq!(github.latest, Some(id));
}

/// NEGATIVE CONTROL for the ordering: a head PATCH sent before the appcast pair is up
/// exposes a head with nothing to install, and the pointer gate cannot fetch an
/// appcast that is not there.
#[test]
fn a_head_patch_before_the_appcast_pair_exposes_an_empty_head() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast);
    channel.bind().unwrap();
    for file in files
        .iter()
        .filter(|f| mirror::channel_upload_rank(f.file_name().unwrap().to_str().unwrap()) == 0)
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
    let mut channel = FakeChannel::new(&mut github, id, local_appcast);
    mirror::publish_on_channel(&mut channel, files).expect("draft path");
    assert_eq!(github.head_patches, vec![(true, true)]);
    assert!(!github.releases[id].draft);
    assert_eq!(github.latest, Some(id));
    assert!(github.exposures.is_empty(), "{:?}", github.exposures);
}

/// The rank table itself: everything, then the signature, then the appcast.
#[test]
fn the_upload_rank_puts_the_appcast_pair_last_signature_first() {
    assert_eq!(mirror::channel_upload_rank("aterm-0.91.0-mac.zip"), 0);
    assert_eq!(
        mirror::channel_upload_rank(aterm_update_core::roster::ROSTER_ASSET),
        0
    );
    assert_eq!(
        mirror::channel_upload_rank(manifest_out::MANIFEST_SIG_ASSET),
        1
    );
    assert_eq!(mirror::channel_upload_rank(manifest_out::MANIFEST_ASSET), 2);
}

/// A cut OWNS `latest`, so it must never take it from a NEWER release: `make_latest`
/// obeys no version order, and a stale journal resumed at `mirror` after a newer
/// release shipped would hand the fleet an older build. Refused at the first ratchet —
/// before a single upload, and with no PATCH.
#[test]
fn a_newer_release_holding_latest_is_never_displaced() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let newer = github.app_release("0.92.0", BUILD + 1);
    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast);
    let err = mirror::publish_on_channel(&mut channel, files)
        .expect_err("a newer head is never displaced");
    assert!(
        err.to_string()
            .contains("v0.92.0 holds alabsystems/aterm's `latest`"),
        "{err}"
    );
    assert!(github.uploads.is_empty(), "refused before any upload");
    assert!(github.head_patches.is_empty(), "and before any PATCH");
    assert_eq!(
        github.binds, 0,
        "and before the bind: the journal names no channel release for retire to trip on"
    );
    assert_eq!(github.latest, Some(newer));
}

/// The head ratchet's table, over the fake: an older head passes, this cut passes
/// (an idempotent re-send), an empty channel passes; a newer tag, an equal tag
/// order, or an older tag carrying a build that is not lower all refuse.
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
    decide(&newer).expect_err("a newer tag is never displaced");

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

/// A resume after the head PATCH landed (the journal mark did not): the pointer already
/// names this cut, every asset is already up, and the whole sequence converges again
/// without re-uploading a byte — the PATCH re-sent is a no-op.
#[test]
fn a_resume_after_the_head_patch_converges_idempotently() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast.clone());
    mirror::publish_on_channel(&mut channel, files.clone()).expect("first pass");
    let uploaded_once = github.releases[id].assets.clone();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast);
    mirror::publish_on_channel(&mut channel, files).expect("the resume converges");
    assert_eq!(github.releases[id].assets, uploaded_once);
    assert_eq!(github.head_patches, vec![(true, true), (true, true)]);
    assert_eq!(github.latest, Some(id));
    assert!(github.exposures.is_empty(), "{:?}", github.exposures);
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
        let channel = FakeChannel::new(&mut github, id, local_appcast.clone());
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
    let mut channel = FakeChannel::new(&mut github, id, local_appcast);
    mirror::publish_on_channel(&mut channel, files).expect("no app head to protect");
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
        mirror::head_patch_argv("repos/alabsystems/aterm/releases/7"),
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
    let without_prerelease: Vec<_> = mirror::HEAD_PATCH
        .iter()
        .copied()
        .filter(|(name, _)| *name != "prerelease")
        .collect();
    assert_eq!(without_prerelease.len(), mirror::HEAD_PATCH.len() - 1);

    let mut github = FakeGithub::with_previous_release();
    github.head_fields = Some(without_prerelease.clone());
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut channel = FakeChannel::new(&mut github, id, local_appcast.clone());
    let err = mirror::publish_on_channel(&mut channel, files.clone())
        .expect_err("a prerelease cannot be made latest");
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
    let mut channel = FakeChannel::new(&mut github, id, local_appcast);
    mirror::publish_on_channel(&mut channel, files).expect("a draft needs no prerelease=false");
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
        declared(include_str!("../../../publish/config.sh")),
        ["\"true\""],
        "publish/config.sh declares the opt-in exactly once, as the engine parses it"
    );
    assert!(
        mirror::HEAD_PATCH.contains(&("prerelease", HeadPatchValue::Bool(false))),
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

/// The classifier's table. POSITIVE rows: the source release (with or without its
/// roster pair, or still empty), and this cut's own release — complete, or partial as a
/// dead machine left it — alongside the source shapes. NEGATIVE rows, each a foreign
/// release that must never be adopted: a name no cut publishes (another version's DMG, a
/// retired container), one mismatched app asset among matching ones, and a source
/// release carrying one binary that is not this cut's.
#[test]
fn an_unclaimed_release_is_adopted_only_as_the_source_release_or_by_its_bytes() {
    let elected = mirror::required_asset_names(VERSION, true, true);
    let source_pair = ["SHA256SUMS", "SHA256SUMS.sig"];
    let roster_pair = [
        aterm_update_core::roster::ROSTER_ASSET,
        aterm_update_core::roster::ROSTER_SIG_ASSET,
    ];
    let dmg = mirror::dmg_asset_name(VERSION);
    let names = |parts: &[&[&str]]| -> Vec<String> {
        parts
            .iter()
            .flat_map(|part| part.iter().map(|name| (*name).to_string()))
            .collect()
    };
    let elected_refs: Vec<&str> = elected.iter().map(String::as_str).collect();
    let app_only: Vec<&str> = elected_refs
        .iter()
        .copied()
        .filter(|name| !roster_pair.contains(name))
        .collect();
    // (label, release assets, app assets whose bytes are NOT this cut's, verdict)
    type Want = std::result::Result<UnclaimedChannelRelease, &'static str>;
    let rows: Vec<(&str, Vec<String>, Vec<&str>, Want)> = vec![
        (
            "the source release",
            names(&[&source_pair, &roster_pair]),
            vec![],
            Ok(UnclaimedChannelRelease::Source),
        ),
        (
            "a source release without its roster pair yet",
            names(&[&source_pair]),
            vec![],
            Ok(UnclaimedChannelRelease::Source),
        ),
        (
            "a source release with nothing on it yet",
            vec![],
            vec![],
            Ok(UnclaimedChannelRelease::Source),
        ),
        (
            "this cut's complete release beside the source pair",
            names(&[&source_pair, &elected_refs]),
            vec![],
            Ok(UnclaimedChannelRelease::Own),
        ),
        (
            "this cut's partial release, as a dead machine left it",
            names(&[&source_pair, &roster_pair, &[dmg.as_str()]]),
            vec![],
            Ok(UnclaimedChannelRelease::Own),
        ),
        (
            "NEGATIVE: another version's DMG",
            names(&[&source_pair, &["aterm-0.90.0.dmg"]]),
            vec![],
            Err("it carries aterm-0.90.0.dmg, which this cut does not publish"),
        ),
        (
            "NEGATIVE: a retired container beside this cut's own set",
            names(&[&source_pair, &elected_refs, &["aterm-offline.dmg"]]),
            vec![],
            Err("it carries aterm-offline.dmg, which this cut does not publish"),
        ),
        (
            "NEGATIVE: one mismatched app asset among this cut's",
            names(&[&source_pair, &app_only]),
            vec![manifest_out::MANIFEST_ASSET],
            Err("aterm-appcast.toml is not this cut's artifact"),
        ),
        (
            "NEGATIVE: the source release plus one binary that is not this cut's",
            names(&[&source_pair, &roster_pair, &[dmg.as_str()]]),
            vec![dmg.as_str()],
            Err("is not this cut's artifact"),
        ),
    ];
    for (label, assets, foreign_bytes, want) in rows {
        let mut asked = Vec::new();
        let verdict = mirror::classify_unclaimed_channel_release(&assets, &elected, |name| {
            asked.push(name.to_string());
            if foreign_bytes.contains(&name) {
                Err("sha256 differs".to_string())
            } else {
                Ok(())
            }
        });
        match (want, &verdict) {
            (Ok(want), got) => assert_eq!(got, &want, "{label}"),
            (Err(needle), UnclaimedChannelRelease::Foreign(why)) => {
                assert!(why.contains(needle), "{label}: {why}");
            }
            (Err(_), got) => panic!("{label}: a foreign release was classified {got:?}"),
        }
        match verdict {
            // A source release costs no download, and a foreign NAME is refused before
            // any byte is fetched.
            UnclaimedChannelRelease::Source => assert!(asked.is_empty(), "{label}: {asked:?}"),
            UnclaimedChannelRelease::Foreign(ref why) if why.contains("does not publish") => {
                assert!(asked.is_empty(), "{label}: {asked:?}");
            }
            // Never a roster or attestation asset: those are not this cut's to prove.
            _ => assert!(
                asked
                    .iter()
                    .all(|name| !roster_pair.contains(&name.as_str())
                        && !source_pair.contains(&name.as_str())),
                "{label}: {asked:?}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// --retire-unmirrored and the release its journal names
// ---------------------------------------------------------------------------

/// Retire's decision over the channel release its journal names. A cut refused at the
/// first ratchet names none (proved above: the floors run before the bind); one refused
/// later names either the draft it created — deleted — or the shared-tag release it
/// adopted, which is VISIBLE and left alone: before 2026-09-23 retire refused it as
/// "LIVE", and the documented exit was dead for exactly the refusal it exists for.
/// NEGATIVE rows: an ID that now names another tag, draft or visible, is never touched.
#[test]
fn retire_deletes_its_own_draft_leaves_the_adopted_release_and_touches_nothing_else() {
    let object = |tag: &str, draft: bool| publish::ReleaseObjectIdentity {
        id: 7,
        tag: tag.to_string(),
        draft,
        target_commitish: "main".to_string(),
    };
    use verify::RetireChannelRelease::{DeleteDraft, Gone, LeaveAdopted};
    assert_eq!(verify::retire_channel_disposition(None, TAG), Ok(Gone));
    assert_eq!(
        verify::retire_channel_disposition(Some(&object(TAG, true)), TAG),
        Ok(DeleteDraft)
    );
    assert_eq!(
        verify::retire_channel_disposition(Some(&object(TAG, false)), TAG),
        Ok(LeaveAdopted)
    );
    for draft in [true, false] {
        let err = verify::retire_channel_disposition(Some(&object(PREVIOUS, draft)), TAG)
            .expect_err("another tag's object is never touched");
        assert!(err.contains(&format!("under tag \"{PREVIOUS}\"")), "{err}");
    }
}

// ---------------------------------------------------------------------------
// Tier-1: the real sequence, stepped through the derived `ReleaseChannelHead`
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
}

impl ModelBound<'_> {
    fn project(&self) -> [(&'static str, i64); 4] {
        let github = &*self.inner.github;
        let ours = &github.releases[self.inner.id];
        let latest = match github.latest {
            Some(i) if i == self.inner.id => 1,
            Some(i) if Some(i) == self.newer => 2,
            _ => 0,
        };
        let has = |name: &str| i64::from(ours.assets.contains_key(name));
        [
            ("latest", latest),
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
        self.agree(action)
    }
}

impl ChannelRelease for ModelBound<'_> {
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
        self.agree("prove_assets")
    }

    fn ratchet(&mut self) -> Result<()> {
        match self.inner.ratchet() {
            Ok(()) => self.step("Ratchet"),
            Err(refused) => {
                self.step("RefuseNewer")?;
                Err(refused)
            }
        }
    }

    fn make_head(&mut self) -> Result<()> {
        self.inner.make_head()?;
        self.step("MakeHead")
    }

    fn prove_head(&mut self) -> Result<()> {
        self.agree("prove_head")?;
        self.inner.prove_head()
    }
}

/// Bind `id` (the source release) to a model state that has taken the environment's
/// steps: `PublishSource`, optionally `NewerRelease`, and the cut's `AcquireLease`.
fn model_bound<'g>(
    github: &'g mut FakeGithub,
    id: usize,
    newer: Option<usize>,
    local_appcast: Vec<u8>,
) -> ModelBound<'g> {
    let model = aterm_spec::derive::release_channel_head_model();
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
        inner: FakeChannel::new(github, id, local_appcast),
        model,
        state,
        actions: Vec::new(),
        newer,
    };
    bound
        .agree("binding")
        .expect("the environment projects onto the model");
    bound
}

/// TIER-1. The real `publish_on_channel`, against the fake, conforms to the derived
/// model step for step: the ratchet, the bind, the signature, the appcast, the ratchet
/// again, the head PATCH — every one enabled where it was taken, and the fake's
/// `latest`, prerelease flag and appcast pair equal to the model's after each.
#[test]
fn the_real_sequence_conforms_to_the_derived_head_model() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut bound = model_bound(&mut github, id, None, local_appcast);
    mirror::publish_on_channel(&mut bound, files).expect("conforms");
    assert_eq!(
        bound.actions,
        [
            "Ratchet",
            "Bind",
            "UploadSig",
            "UploadToml",
            "Ratchet",
            "MakeHead"
        ]
    );
    assert_eq!(bound.state["phase"], 3);
}

/// TIER-1, the refusal: a newer release holding `latest` is the model's
/// `RefuseNewer`, taken by the real ratchet on its first call.
#[test]
fn a_newer_head_conforms_as_the_models_refusal() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let newer = github.app_release("0.92.0", BUILD + 1);
    let (_dir, files, local_appcast) = dist();
    let mut bound = model_bound(&mut github, id, Some(newer), local_appcast);
    mirror::publish_on_channel(&mut bound, files).expect_err("refused");
    assert_eq!(bound.actions, ["RefuseNewer"]);
    assert_eq!(bound.state["phase"], 4);
    assert_eq!(bound.state["bound"], 0);
    assert_eq!(github.binds, 0, "the refused cut bound nothing");
}

/// NEGATIVE CONTROLS: the binding catches what it exists to catch.
/// * the bind before the floors (the adopt path before 2026-09-23 persisted the
///   adoption and bound the release first) takes a `Bind` the model disables;
/// * the upload order the sequence would have without its rank sort (alphabetical:
///   the appcast before its signature) takes an `UploadToml` the model disables;
/// * the adopt path before 2026-09-23 (no head PATCH reaching GitHub) leaves the fake's
///   `latest` where the model says the head PATCH moved it.
#[test]
fn the_tier1_binding_refuses_the_pre_fix_orders() {
    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut bound = model_bound(&mut github, id, None, local_appcast.clone());
    let err = bound.bind().expect_err("the bind before the ratchet");
    assert!(
        err.to_string()
            .contains("took Bind, which the model does not enable"),
        "{err}"
    );

    let mut github = FakeGithub::with_previous_release();
    let id = github.source_release(true);
    let mut bound = model_bound(&mut github, id, None, local_appcast.clone());
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
    github.drop_head_patch = true;
    let id = github.source_release(true);
    let (_dir, files, local_appcast) = dist();
    let mut bound = model_bound(&mut github, id, None, local_appcast);
    let err = mirror::publish_on_channel(&mut bound, files).expect_err("no PATCH");
    assert!(
        err.to_string()
            .contains("after MakeHead, the fake GitHub has latest=0 but the model has 1"),
        "{err}"
    );
}
