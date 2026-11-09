// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The release-host write contract (plan §15): one set of cases over `FakeHost` and over
//! `AghHost` on agh-fake's loopback server, the cases `AghHost` alone answers (open intents,
//! grants, the pre-claim push check), and the findings F1, F2, F5 and the bounded tag read.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use agh::config::Dirs;
use agh::fx::Fx;
use agh::git::HelperCommand;
use agh::identity::MapEnv;
use agh::policy::Router;
use agh::retry::RetryPolicy;
use agh::wire::Endpoints;
use agh::wire::http::HttpTransport;
use agh_fake::{AssetSpec, Change, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec};
use atpkg_publish::artifact::upload;
use atpkg_publish::ctx::HostChoice;
use atpkg_publish::error::Error;
use atpkg_publish::host::agh::AghHost;
use atpkg_publish::host::fake::{FakeFail, FakeHost, FakeOp, FakeRelease};
use atpkg_publish::host::{HostError, Hosts, Probe, Release, ReleaseHost, Replace, Slug};

use super::{Rig, scratch};
use crate::shared::{toml_path, write_config};

const OWNER: &str = "fixture-dev";
const REPO: &str = "packs";
const SLUG: &str = "fixture-dev/packs";
const RELEASE_SLUG: &str = "alabsystems/aterm";
const SIM_START: u64 = 1_760_000_000;
const AT: &str = "2026-10-01T00:00:00Z";

fn slug() -> Slug {
    Slug::new(OWNER, REPO)
}

fn slug_of(text: &str) -> Slug {
    let (owner, repo) = text.split_once('/').expect("owner/repo");
    Slug::new(owner, repo)
}

/// One release as a world reads it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Held {
    pub(super) draft: bool,
    pub(super) prerelease: bool,
    pub(super) assets: BTreeMap<String, Vec<u8>>,
}

/// A failure a shared case arms, which each world makes its own way (plan §15, Tests).
#[derive(Clone, Copy, Debug)]
pub(super) enum Arm {
    /// The create dies after its draft holds every file: FakeHost's `KilledCreate`, agh-fake's
    /// `Fault::Close` on the publish PATCH.
    KilledCreate,
    /// The create's POST lands and its answer is lost: FakeHost's `LostResponse`, agh-fake's
    /// `Fault::LandThenClose` on the POST, with the listing lag hiding the draft until
    /// `settle` reveals it.
    LostCreateAnswer,
    /// The publish lands and its answer is lost: FakeHost's `Create, LostResponse`, agh-fake's
    /// `Fault::LandThenClose` on the publish PATCH.
    LostPublishAnswer,
    /// An upload of `a.bin` then `b.bin` is cut after `a.bin`: FakeHost's `Partial`, agh-fake's
    /// `Fault::Close` on the second POST.
    CutUpload,
}

pub(super) trait WriteWorld {
    fn host(&self) -> &dyn ReleaseHost;
    fn slug(&self) -> Slug;
    /// Lay release `tag`, published or a draft, a prerelease, holding `assets`.
    fn lay(&self, tag: &str, draft: bool, assets: &[(&str, &[u8])]);
    /// Every release holding `tag`, as the world holds it.
    fn read(&self, tag: &str) -> Vec<Held>;
    fn arm(&self, arm: Arm);
    /// What must happen before a rerun after `arm` (agh-fake reveals what the lag hid).
    fn settle(&self, arm: Arm);
    fn make_immutable(&self, tag: &str);
    /// Make published release `tag` a full release, as someone else's edit would.
    fn make_full_release(&self, tag: &str);
    /// Whether a lost publish answer is settled by the create itself: agh's converging
    /// publish re-reads the release after a lost answer and finds it published.
    fn lost_publish_settles_itself(&self) -> bool;
    /// The writes the host has sent or taken so far.
    fn writes(&self) -> usize;
    /// Name `sha` as `remote()`'s main where the host's API reads it (agh-fake's branch;
    /// FakeHost reads `remote()` alone).
    fn set_main(&self, sha: &str);
    /// Where a case lays the bare repository `clone_repo` and `fetch_repo` read.
    fn remote(&self) -> PathBuf;
    /// A scratch directory of this world's.
    fn dir(&self) -> &Path;
}

static FILES: AtomicUsize = AtomicUsize::new(0);

/// `bytes` as `name`, in a fresh directory under `dir`: its path.
fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let n = FILES.fetch_add(1, Ordering::Relaxed);
    let sub = dir.join(format!("files-{n}"));
    std::fs::create_dir_all(&sub).expect("the files directory");
    let path = sub.join(name);
    std::fs::write(&path, bytes).expect("the file");
    path
}

fn held(draft: bool, assets: &[(&str, &[u8])]) -> Held {
    Held {
        draft,
        prerelease: true,
        assets: assets
            .iter()
            .map(|(n, b)| ((*n).to_string(), b.to_vec()))
            .collect(),
    }
}

struct FakeWorld {
    host: FakeHost,
    dir: aterm_tempfile::TempDir,
}

impl FakeWorld {
    fn new() -> FakeWorld {
        let dir = scratch("write-contract-fake");
        let host = FakeHost::new();
        host.add_clone_source(&slug(), &dir.path().join("remote.git"));
        FakeWorld { host, dir }
    }
}

impl WriteWorld for FakeWorld {
    fn host(&self) -> &dyn ReleaseHost {
        &self.host
    }

    fn slug(&self) -> Slug {
        slug()
    }

    fn lay(&self, tag: &str, draft: bool, assets: &[(&str, &[u8])]) {
        self.host.put(
            &slug(),
            tag,
            FakeRelease {
                draft,
                prerelease: true,
                assets: assets
                    .iter()
                    .map(|(n, b)| ((*n).to_string(), b.to_vec()))
                    .collect(),
            },
        );
    }

    fn read(&self, tag: &str) -> Vec<Held> {
        self.host
            .release_of(&slug(), tag)
            .map(|r| Held {
                draft: r.draft,
                prerelease: r.prerelease,
                assets: r.assets,
            })
            .into_iter()
            .collect()
    }

    fn arm(&self, arm: Arm) {
        let (op, fail) = match arm {
            Arm::KilledCreate => (FakeOp::Create, FakeFail::KilledCreate),
            Arm::LostCreateAnswer | Arm::LostPublishAnswer => {
                (FakeOp::Create, FakeFail::LostResponse)
            }
            Arm::CutUpload => (FakeOp::Upload, FakeFail::Partial),
        };
        self.host.fail_next(op, fail);
    }

    fn settle(&self, _arm: Arm) {}

    fn make_immutable(&self, tag: &str) {
        self.host.set_immutable(&slug(), tag, true);
    }

    fn make_full_release(&self, tag: &str) {
        let release = self.host.release_of(&slug(), tag).expect("the release");
        self.host.put(
            &slug(),
            tag,
            FakeRelease {
                prerelease: false,
                ..release
            },
        );
    }

    fn lost_publish_settles_itself(&self) -> bool {
        false
    }

    fn writes(&self) -> usize {
        self.host.changes()
    }

    fn set_main(&self, _sha: &str) {}

    fn remote(&self) -> PathBuf {
        self.dir.path().join("remote.git")
    }

    fn dir(&self) -> &Path {
        self.dir.path()
    }
}

/// How an `AghWorld` is built.
#[derive(Clone, Copy)]
pub(super) struct Setup {
    /// The repository the host serves (its owner is the host's home).
    slug: &'static str,
    /// Whether the slug's writer has push (else pull).
    push: bool,
    /// Whether the dev profile's token file is there.
    dev_token: bool,
    /// Whether fixture-dev's read chain falls back to anonymous.
    dev_anonymous: bool,
    /// Whether agh.toml grants atpkg-publish `alabsystems/aterm`.
    grant: bool,
}

impl Setup {
    const DEV: Setup = Setup {
        slug: SLUG,
        push: true,
        dev_token: true,
        dev_anonymous: false,
        grant: false,
    };
    pub(super) const RELEASE: Setup = Setup {
        slug: RELEASE_SLUG,
        push: true,
        dev_token: true,
        dev_anonymous: false,
        grant: true,
    };
}

pub(super) struct AghWorld {
    pub(super) fake: FakeGithub,
    pub(super) host: Arc<AghHost>,
    slug: &'static str,
    dirs: Dirs,
    endpoints: Endpoints,
    root: aterm_tempfile::TempDir,
    /// The simulated `Fx` the client runs on: `children_run` names every git run's spec.
    fx: Arc<Fx>,
}

/// The agh.toml of the write contract: a dev and a release profile, the two fixture owners,
/// and, when `grant`, atpkg-publish's `[[writers]]` row.
fn config_text(tokens: &Path, grant: bool, dev_anonymous: bool) -> String {
    let tokens = toml_path(tokens);
    let dev_read = if dev_anonymous {
        "[\"dev\", \"anonymous\"]"
    } else {
        "[\"dev\"]"
    };
    let mut text = format!(
        "[profiles.dev]\ntoken_file = \"{tokens}/dev.token\"\ntier = \"dev\"\n\
         login = \"fixture-dev-bot\"\n\n\
         [profiles.release]\ntoken_file = \"{tokens}/release.token\"\ntier = \"release\"\n\
         login = \"fixture-release-bot\"\n\n\
         [[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = {dev_read}\nwrite = \"dev\"\n\n\
         [[owners]]\nname = \"alabsystems\"\ntier = \"release\"\nread = [\"dev\", \"anonymous\"]\n\
         write = \"release\"\n"
    );
    if grant {
        text.push_str(
            "\n[[writers]]\ncaller = \"atpkg-publish\"\nrepos = [\"alabsystems/aterm\"]\n",
        );
    }
    text
}

/// agh.toml written under `dirs` and read back with `load_for_host`, so its origin is a file
/// and its grants count.
fn file_router(dirs: &Dirs, text: &str) -> Router {
    write_config(dirs, text);
    let validated = agh::config::load_for_host(dirs)
        .unwrap_or_else(|e| panic!("agh.toml did not load: {}", e.why))
        .unwrap_or_else(|| panic!("no agh.toml at {}", dirs.config.display()));
    Router::from_validated(validated)
}

impl AghWorld {
    fn new() -> AghWorld {
        AghWorld::build(Setup::DEV)
    }

    pub(super) fn build(setup: Setup) -> AghWorld {
        Self::build_with_concurrent_clone_destination(setup, None)
    }

    fn build_with_concurrent_clone_destination(
        setup: Setup,
        concurrent_clone_destination: Option<&'static str>,
    ) -> AghWorld {
        let fake = FakeGithub::start();
        let root = scratch("write-contract-agh");
        let dirs = Dirs::under(root.path());
        let tokens = root.path().join("tokens");
        aterm_uds::private_file::create_private_dir(&tokens).expect("the token directory");
        for (name, login, written) in [
            ("dev", "fixture-dev-bot", setup.dev_token),
            ("release", "fixture-release-bot", true),
        ] {
            let secret = agh_fake::token(name);
            fake.add_token(&secret, login);
            if written {
                aterm_uds::private_file::write_private(
                    &tokens,
                    &format!("{name}.token"),
                    format!("{secret}\n").as_bytes(),
                )
                .expect("the token file");
            }
        }
        let router = file_router(
            &dirs,
            &config_text(&tokens, setup.grant, setup.dev_anonymous),
        );
        let login = if setup.slug == RELEASE_SLUG {
            "fixture-release-bot"
        } else {
            "fixture-dev-bot"
        };
        let spec = if setup.push {
            RepoSpec::public().push(login)
        } else {
            RepoSpec::public().pull(login)
        };
        fake.add_repo(setup.slug, spec);
        let ports = fake.ports();
        let endpoints = Endpoints::loopback(ports.api, ports.uploads, ports.web, ports.cdn);
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).expect("the scratch home");
        let path = std::env::var("PATH").unwrap_or_default();
        let env = MapEnv::new()
            .with("PATH", &path)
            .with("HOME", &home.to_string_lossy());
        let mut fx = Fx::simulated(SIM_START);
        if let Some(relative) = concurrent_clone_destination {
            let destination = root.path().join(relative);
            fx = fx.with_git_child_observer_for_tests(move |_| {
                std::fs::create_dir_all(&destination).expect("concurrent clone destination");
                std::fs::write(destination.join("theirs"), b"keep")
                    .expect("concurrent destination file");
            });
        }
        let fx = Arc::new(fx);
        let github = agh::GithubBuilder::new(router)
            .transport(Arc::new(HttpTransport::direct(Arc::new(env.clone()))))
            .endpoints(endpoints.clone())
            .env(Arc::new(env))
            .fx(Arc::clone(&fx))
            .retry(RetryPolicy::NONE)
            .caller("atpkg-publish")
            .journal(dirs.clone())
            .git_helper_for_tests(HelperCommand {
                exe: "/usr/bin/false".into(),
                args: Vec::new(),
            })
            .git_remote_for_tests(setup.slug, &root.path().join("remote.git"))
            .build();
        let home_slug = agh::Slug::parse(setup.slug).expect("the home slug");
        let host = Arc::new(AghHost::new(github, home_slug).with_retry_pause(Duration::ZERO));
        AghWorld {
            fake,
            host,
            slug: setup.slug,
            dirs,
            endpoints,
            root,
            fx,
        }
    }

    fn id_of(&self, tag: &str) -> u64 {
        self.fake
            .releases_of(self.slug)
            .iter()
            .find(|r| r.tag == tag)
            .map(|r| r.id)
            .unwrap_or_else(|| panic!("no release {tag}"))
    }

    fn releases_path(&self) -> String {
        format!("/repos/{}/releases", self.slug)
    }

    /// The release POSTs agh-fake has seen.
    fn posts(&self) -> usize {
        let path = self.releases_path();
        self.fake
            .seen()
            .iter()
            .filter(|s| {
                s.origin == FakeOrigin::Api && s.method == "POST" && s.path_and_query == path
            })
            .count()
    }

    /// The release PATCHes agh-fake has seen.
    fn patches(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|s| s.method == "PATCH")
            .count()
    }

    /// `agh intent list REPO` over this world's state directory: each intent's id and
    /// whether it is open.
    fn intents(&self) -> Vec<(String, bool)> {
        let env = MapEnv::new();
        let transport = Arc::new(HttpTransport::direct(Arc::new(env.clone())));
        let (mut ctx, captured) =
            agh::cli::Ctx::for_tests(self.root.path(), transport, self.endpoints.clone(), env);
        let args = [
            "intent".to_string(),
            "list".to_string(),
            self.slug.to_string(),
        ];
        assert_eq!(agh::cli::run(&args, &mut ctx), 0, "agh intent list");
        let text = captured.stdout_text();
        let doc: aterm_json::Value =
            aterm_json::from_str(text.trim()).unwrap_or_else(|e| panic!("{e}: {text}"));
        doc.as_array()
            .expect("a list")
            .iter()
            .map(|item| {
                (
                    item.get("id")
                        .and_then(aterm_json::Value::as_str)
                        .expect("an id")
                        .to_string(),
                    item.get("open")
                        .and_then(aterm_json::Value::as_bool)
                        .expect("open"),
                )
            })
            .collect()
    }
}

impl WriteWorld for AghWorld {
    fn host(&self) -> &dyn ReleaseHost {
        &*self.host
    }

    fn slug(&self) -> Slug {
        slug_of(self.slug)
    }

    fn lay(&self, tag: &str, draft: bool, assets: &[(&str, &[u8])]) {
        let spec = if draft {
            ReleaseSpec::draft(tag, AT)
        } else {
            ReleaseSpec::published(tag, AT)
        };
        let id = self.fake.add_release(self.slug, spec.prerelease(true));
        for (name, bytes) in assets {
            self.fake.add_asset(id, AssetSpec::uploaded(name, bytes));
        }
    }

    fn read(&self, tag: &str) -> Vec<Held> {
        self.fake
            .releases_of(self.slug)
            .into_iter()
            .filter(|r| r.tag == tag)
            .map(|r| {
                let (_, bytes) = self.fake.release_state(r.id).expect("the release");
                Held {
                    draft: r.draft,
                    prerelease: r.prerelease,
                    assets: bytes.into_iter().collect(),
                }
            })
            .collect()
    }

    fn arm(&self, arm: Arm) {
        let publish = Match::api(&format!("{}/", self.releases_path())).method("PATCH");
        match arm {
            Arm::KilledCreate => self.fake.fault(publish, Fault::Close),
            Arm::LostCreateAnswer => {
                self.fake.set_listing_lag(self.slug, true);
                self.fake.fault(
                    Match::api(&self.releases_path()).method("POST"),
                    Fault::LandThenClose,
                );
            }
            Arm::LostPublishAnswer => self.fake.fault(publish, Fault::LandThenClose),
            Arm::CutUpload => {
                let id = self.id_of("v1");
                self.fake.fault(
                    Match::uploads(&format!("{}/{id}/assets?name=b.bin", self.releases_path()))
                        .method("POST"),
                    Fault::Close,
                );
            }
        }
    }

    fn settle(&self, arm: Arm) {
        if let Arm::LostCreateAnswer = arm {
            for release in self.fake.releases_of(self.slug) {
                if release.hidden {
                    self.fake.apply(Change::Reveal {
                        release_id: release.id,
                    });
                }
            }
            self.fake.set_listing_lag(self.slug, false);
        }
    }

    fn make_immutable(&self, tag: &str) {
        self.fake.set_immutable(self.id_of(tag), true);
    }

    /// agh-fake has no `Change` for the flag, so the edit is a PATCH of the world's own to its
    /// loopback API origin, as the slug's writer: one request on one connection, which the
    /// fake closes after its answer.
    fn make_full_release(&self, tag: &str) {
        use std::io::{Read, Write};
        let id = self.id_of(tag);
        let secret = agh_fake::token(if self.slug == RELEASE_SLUG {
            "release"
        } else {
            "dev"
        });
        let body = r#"{"prerelease":false}"#;
        let head = format!(
            "PATCH {}/{id} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {secret}\r\n\
             User-Agent: atpkg-publish-write-contract\r\nX-GitHub-Api-Version: {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.releases_path(),
            agh::API_VERSION,
            body.len()
        );
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.fake.ports().api))
            .expect("the loopback API origin");
        // A hang detector, never a latency budget.
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("the read timeout");
        stream
            .write_all(format!("{head}{body}").as_bytes())
            .expect("the PATCH");
        let mut answer = String::new();
        stream.read_to_string(&mut answer).expect("the answer");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(
            self.fake
                .releases_of(self.slug)
                .iter()
                .any(|r| r.id == id && !r.prerelease),
            "{answer}"
        );
    }

    fn lost_publish_settles_itself(&self) -> bool {
        true
    }

    fn writes(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|s| s.method != "GET" && s.method != "HEAD")
            .count()
    }

    fn set_main(&self, sha: &str) {
        self.fake.set_branch(self.slug, "main", sha);
    }

    fn remote(&self) -> PathBuf {
        self.root.path().join("remote.git")
    }

    fn dir(&self) -> &Path {
        self.root.path()
    }
}

fn on_fake(case: fn(&dyn WriteWorld)) {
    case(&FakeWorld::new());
}

fn on_agh(case: fn(&dyn WriteWorld)) {
    let w = AghWorld::new();
    case(&w);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

fn create(w: &dyn WriteWorld, tag: &str, files: &[PathBuf]) -> Result<(), HostError> {
    w.host().create(&w.slug(), tag, "title", "notes", files)
}

fn two_files(w: &dyn WriteWorld) -> Vec<PathBuf> {
    vec![
        file(w.dir(), "a.bin", b"ay"),
        file(w.dir(), "b.bin", b"bee"),
    ]
}

fn unreachable(r: Result<(), HostError>) {
    assert!(matches!(r, Err(HostError::Unreachable(_))), "{r:?}");
}

fn refused(r: Result<(), HostError>) {
    assert!(matches!(r, Err(HostError::Refused(_))), "{r:?}");
}

fn case_create_makes_one_prerelease_never_latest_with_every_file(w: &dyn WriteWorld) {
    create(w, "v1", &two_files(w)).expect("the create");
    assert_eq!(
        w.read("v1"),
        vec![held(false, &[("a.bin", b"ay"), ("b.bin", b"bee")])]
    );
}

#[test]
fn create_makes_one_prerelease_never_latest_with_every_file_fake() {
    on_fake(case_create_makes_one_prerelease_never_latest_with_every_file);
}

#[test]
fn create_makes_one_prerelease_never_latest_with_every_file_agh() {
    let w = AghWorld::new();
    case_create_makes_one_prerelease_never_latest_with_every_file(&w);
    let releases = w.fake.releases_of(SLUG);
    assert_eq!(releases.len(), 1, "{releases:?}");
    assert_eq!(releases[0].make_latest.as_deref(), Some("false"));
    assert_eq!(releases[0].target_commitish, None);
    assert_eq!(w.posts(), 1);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

fn case_a_create_over_an_existing_release_is_refused(w: &dyn WriteWorld) {
    w.lay("v1", false, &[("a.bin", b"ay")]);
    let before = w.read("v1");
    refused(create(w, "v1", &two_files(w)));
    assert_eq!(w.read("v1"), before);
}

#[test]
fn a_create_over_an_existing_release_is_refused_fake() {
    on_fake(case_a_create_over_an_existing_release_is_refused);
}

#[test]
fn a_create_over_an_existing_release_is_refused_agh() {
    on_agh(case_a_create_over_an_existing_release_is_refused);
}

/// An armed write failure never lays a release over one the host did not make: the create is
/// refused first, as unarmed, writing nothing, and the arming waits for the next create that
/// writes, as agh-fake's fault on a request waits for that request.
fn case_an_armed_create_over_another_release_changes_nothing(w: &dyn WriteWorld) {
    w.lay("v1", false, &[("a.bin", b"ay")]);
    let before = w.read("v1");
    let files = two_files(w);
    let whole = |draft| held(draft, &[("a.bin", b"ay"), ("b.bin", b"bee")]);

    w.arm(Arm::KilledCreate);
    refused(create(w, "v1", &files));
    assert_eq!(w.read("v1"), before);
    assert_eq!(w.writes(), 0, "a refused create writes nothing");
    unreachable(create(w, "v2", &files));
    assert_eq!(w.read("v2"), vec![whole(true)], "the arming fired on v2");

    w.arm(Arm::LostPublishAnswer);
    refused(create(w, "v1", &files));
    assert_eq!(w.read("v1"), before);
    let lost = create(w, "v3", &files);
    if w.lost_publish_settles_itself() {
        lost.expect("the publish's own guard read settles its lost answer");
    } else {
        unreachable(lost);
    }
    assert_eq!(w.read("v3"), vec![whole(false)], "the arming fired on v3");
    assert_eq!(w.read("v1"), before);
}

#[test]
fn an_armed_create_over_another_release_changes_nothing_fake() {
    on_fake(case_an_armed_create_over_another_release_changes_nothing);

    // An armed `Refuse` fires on the next create, whatever is there: over another's release
    // it is spent, so the create after it is not refused.
    let w = FakeWorld::new();
    w.lay("v1", false, &[("a.bin", b"ay")]);
    let before = w.read("v1");
    w.host.fail_next(FakeOp::Create, FakeFail::Refuse);
    refused(create(&w, "v1", &two_files(&w)));
    assert_eq!(w.read("v1"), before);
    create(&w, "v2", &two_files(&w)).expect("the arming is spent");
}

#[test]
fn an_armed_create_over_another_release_changes_nothing_agh() {
    on_agh(case_an_armed_create_over_another_release_changes_nothing);
}

/// A rerun of a killed create that brings fewer files than its draft holds is refused, as
/// agh's exact-set publish guard refuses it, and the draft stays as the killed create left it.
fn case_a_killed_create_rerun_with_fewer_files_is_refused(w: &dyn WriteWorld) {
    let files = two_files(w);
    let draft = vec![held(true, &[("a.bin", b"ay"), ("b.bin", b"bee")])];
    w.arm(Arm::KilledCreate);
    unreachable(create(w, "v1", &files));
    assert_eq!(w.read("v1"), draft);
    w.settle(Arm::KilledCreate);
    let writes = w.writes();
    refused(create(w, "v1", &files[..1]));
    assert_eq!(w.read("v1"), draft);
    assert_eq!(w.writes(), writes, "the refused rerun writes nothing");
}

#[test]
fn a_killed_create_rerun_with_fewer_files_is_refused_fake() {
    on_fake(case_a_killed_create_rerun_with_fewer_files_is_refused);
}

#[test]
fn a_killed_create_rerun_with_fewer_files_is_refused_agh() {
    on_agh(case_a_killed_create_rerun_with_fewer_files_is_refused);
}

fn case_a_killed_create_is_finished_by_the_rerun(w: &dyn WriteWorld) {
    let files = two_files(w);
    w.arm(Arm::KilledCreate);
    unreachable(create(w, "v1", &files));
    assert_eq!(
        w.read("v1"),
        vec![held(true, &[("a.bin", b"ay"), ("b.bin", b"bee")])]
    );
    w.settle(Arm::KilledCreate);
    create(w, "v1", &files).expect("the rerun");
    assert_eq!(
        w.read("v1"),
        vec![held(false, &[("a.bin", b"ay"), ("b.bin", b"bee")])]
    );
}

#[test]
fn a_killed_create_is_finished_by_the_rerun_fake() {
    on_fake(case_a_killed_create_is_finished_by_the_rerun);
}

#[test]
fn a_killed_create_is_finished_by_the_rerun_agh() {
    on_agh(case_a_killed_create_is_finished_by_the_rerun);
}

fn case_a_lost_create_answer_never_makes_a_second_release(w: &dyn WriteWorld) {
    let files = two_files(w);
    w.arm(Arm::LostCreateAnswer);
    unreachable(create(w, "v1", &files));
    w.settle(Arm::LostCreateAnswer);
    create(w, "v1", &files).expect("the rerun");
    assert_eq!(
        w.read("v1"),
        vec![held(false, &[("a.bin", b"ay"), ("b.bin", b"bee")])]
    );
}

#[test]
fn a_lost_create_answer_never_makes_a_second_release_fake() {
    on_fake(case_a_lost_create_answer_never_makes_a_second_release);
}

#[test]
fn a_lost_create_answer_never_makes_a_second_release_agh() {
    let w = AghWorld::new();
    case_a_lost_create_answer_never_makes_a_second_release(&w);
    assert_eq!(w.posts(), 1);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

fn case_a_lost_publish_answer_on_an_immutable_release_converges(w: &dyn WriteWorld) {
    let files = two_files(w);
    let whole = vec![held(false, &[("a.bin", b"ay"), ("b.bin", b"bee")])];
    w.arm(Arm::LostPublishAnswer);
    let first = create(w, "v1", &files);
    if w.lost_publish_settles_itself() {
        first.expect("the publish's own guard read settles its lost answer");
    } else {
        unreachable(first);
    }
    assert_eq!(w.read("v1"), whole);
    w.make_immutable("v1");
    let writes = w.writes();
    create(w, "v1", &files).expect("the rerun converges");
    assert_eq!(w.read("v1"), whole);
    assert_eq!(w.writes(), writes, "the rerun writes nothing");
}

#[test]
fn a_lost_publish_answer_on_an_immutable_release_converges_fake() {
    on_fake(case_a_lost_publish_answer_on_an_immutable_release_converges);
}

#[test]
fn a_lost_publish_answer_on_an_immutable_release_converges_agh() {
    let w = AghWorld::new();
    case_a_lost_publish_answer_on_an_immutable_release_converges(&w);
    assert_eq!(w.posts(), 1);
    assert_eq!(w.patches(), 1);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

/// A second create of a release this host made converges with nothing written, as agh's
/// created-intent rule converges it; other bytes or other notes are refused, and nothing
/// changes.
fn case_a_second_create_of_a_made_release_converges(w: &dyn WriteWorld) {
    let files = two_files(w);
    let whole = vec![held(false, &[("a.bin", b"ay"), ("b.bin", b"bee")])];
    create(w, "v1", &files).expect("the create");
    let writes = w.writes();
    create(w, "v1", &files).expect("the second create converges");
    assert_eq!(w.read("v1"), whole);
    assert_eq!(w.writes(), writes, "the second create writes nothing");

    let other = vec![
        file(w.dir(), "a.bin", b"ay"),
        file(w.dir(), "b.bin", b"other"),
    ];
    refused(create(w, "v1", &other));
    refused(
        w.host()
            .create(&w.slug(), "v1", "title", "other notes", &files),
    );
    assert_eq!(w.read("v1"), whole);
    assert_eq!(w.writes(), writes, "a refused create writes nothing");
}

#[test]
fn a_second_create_of_a_made_release_converges_fake() {
    on_fake(case_a_second_create_of_a_made_release_converges);
}

#[test]
fn a_second_create_of_a_made_release_converges_agh() {
    on_agh(case_a_second_create_of_a_made_release_converges);
}

/// A rerun over a release this host made, which someone has since made a full release, is
/// refused with nothing written: the trait's create leaves a prerelease, never latest.
fn case_a_made_release_now_a_full_release_is_refused(w: &dyn WriteWorld) {
    let files = two_files(w);
    w.arm(Arm::LostPublishAnswer);
    let first = create(w, "v1", &files);
    if w.lost_publish_settles_itself() {
        first.expect("the publish's own guard read settles its lost answer");
    } else {
        unreachable(first);
    }
    w.make_full_release("v1");
    let full = vec![Held {
        prerelease: false,
        ..held(false, &[("a.bin", b"ay"), ("b.bin", b"bee")])
    }];
    assert_eq!(w.read("v1"), full);
    let writes = w.writes();
    refused(create(w, "v1", &files));
    assert_eq!(w.read("v1"), full);
    assert_eq!(w.writes(), writes, "the refused rerun writes nothing");
}

#[test]
fn a_made_release_now_a_full_release_is_refused_fake() {
    on_fake(case_a_made_release_now_a_full_release_is_refused);
}

#[test]
fn a_made_release_now_a_full_release_is_refused_agh() {
    on_agh(case_a_made_release_now_a_full_release_is_refused);
}

fn case_upload_never_converges_on_identical_bytes_and_refuses_others(w: &dyn WriteWorld) {
    w.lay("v1", false, &[("a.bin", b"ay")]);
    let s = w.slug();
    w.host()
        .upload(&s, "v1", &[file(w.dir(), "a.bin", b"ay")], Replace::Never)
        .expect("the same bytes converge");
    refused(w.host().upload(
        &s,
        "v1",
        &[file(w.dir(), "a.bin", b"other")],
        Replace::Never,
    ));
    assert_eq!(w.read("v1"), vec![held(false, &[("a.bin", b"ay")])]);
}

#[test]
fn upload_never_converges_on_identical_bytes_and_refuses_others_fake() {
    on_fake(case_upload_never_converges_on_identical_bytes_and_refuses_others);
}

#[test]
fn upload_never_converges_on_identical_bytes_and_refuses_others_agh() {
    on_agh(case_upload_never_converges_on_identical_bytes_and_refuses_others);
}

fn case_clobber_replaces_and_identical_bytes_are_unchanged(w: &dyn WriteWorld) {
    w.lay("v1", false, &[("a.bin", b"ay")]);
    let s = w.slug();
    let other = file(w.dir(), "a.bin", b"other");
    w.host()
        .upload(&s, "v1", std::slice::from_ref(&other), Replace::Clobber)
        .expect("the clobber");
    assert_eq!(w.read("v1"), vec![held(false, &[("a.bin", b"other")])]);
    w.host()
        .upload(&s, "v1", &[other], Replace::Clobber)
        .expect("the clobber again");
    assert_eq!(w.read("v1"), vec![held(false, &[("a.bin", b"other")])]);
}

#[test]
fn clobber_replaces_and_identical_bytes_are_unchanged_fake() {
    on_fake(case_clobber_replaces_and_identical_bytes_are_unchanged);
}

#[test]
fn clobber_replaces_and_identical_bytes_are_unchanged_agh() {
    let w = AghWorld::new();
    w.lay("v1", false, &[("a.bin", b"ay")]);
    let other = file(w.dir(), "a.bin", b"other");
    w.host
        .upload(
            &slug(),
            "v1",
            std::slice::from_ref(&other),
            Replace::Clobber,
        )
        .expect("the clobber");
    let writes = w.writes();
    w.host
        .upload(&slug(), "v1", &[other], Replace::Clobber)
        .expect("the clobber again");
    assert_eq!(w.writes(), writes, "identical bytes are unchanged");
    assert_eq!(w.read("v1"), vec![held(false, &[("a.bin", b"other")])]);
    on_agh(case_clobber_replaces_and_identical_bytes_are_unchanged);
}

fn case_an_immutable_release_refuses_every_upload(w: &dyn WriteWorld) {
    w.lay("v1", false, &[("a.bin", b"ay")]);
    w.make_immutable("v1");
    let s = w.slug();
    let before = w.read("v1");
    for replace in [Replace::Never, Replace::Clobber] {
        refused(
            w.host()
                .upload(&s, "v1", &[file(w.dir(), "a.bin", b"other")], replace),
        );
    }
    assert_eq!(w.read("v1"), before);
}

#[test]
fn an_immutable_release_refuses_every_upload_fake() {
    on_fake(case_an_immutable_release_refuses_every_upload);
}

#[test]
fn an_immutable_release_refuses_every_upload_agh() {
    on_agh(case_an_immutable_release_refuses_every_upload);
}

/// An armed upload failure never lands other bytes over an asset under `Never`: the upload is
/// refused as unarmed, the asset keeps its bytes, and the arming waits for the next upload
/// that writes.
fn case_an_armed_upload_never_lands_other_bytes(w: &dyn WriteWorld) {
    w.lay("v1", false, &[("a.bin", b"ay")]);
    let s = w.slug();
    let only_a = vec![held(false, &[("a.bin", b"ay")])];
    w.arm(Arm::CutUpload);
    refused(w.host().upload(
        &s,
        "v1",
        &[file(w.dir(), "a.bin", b"other")],
        Replace::Never,
    ));
    assert_eq!(w.read("v1"), only_a);
    let files = two_files(w);
    unreachable(w.host().upload(&s, "v1", &files, Replace::Never));
    assert_eq!(
        w.read("v1"),
        only_a,
        "the arming fired on the upload that writes"
    );
    w.settle(Arm::CutUpload);
    w.host()
        .upload(&s, "v1", &files, Replace::Never)
        .expect("the rerun");
    assert_eq!(
        w.read("v1"),
        vec![held(false, &[("a.bin", b"ay"), ("b.bin", b"bee")])]
    );
}

#[test]
fn an_armed_upload_never_lands_other_bytes_fake() {
    on_fake(case_an_armed_upload_never_lands_other_bytes);
}

#[test]
fn an_armed_upload_never_lands_other_bytes_agh() {
    on_agh(case_an_armed_upload_never_lands_other_bytes);
}

fn case_a_cut_upload_is_converged_by_the_rerun(w: &dyn WriteWorld) {
    w.lay("v1", false, &[]);
    let s = w.slug();
    let files = two_files(w);
    w.arm(Arm::CutUpload);
    unreachable(w.host().upload(&s, "v1", &files, Replace::Never));
    assert_eq!(w.read("v1"), vec![held(false, &[("a.bin", b"ay")])]);
    w.settle(Arm::CutUpload);
    w.host()
        .upload(&s, "v1", &files, Replace::Never)
        .expect("the rerun");
    assert_eq!(
        w.read("v1"),
        vec![held(false, &[("a.bin", b"ay"), ("b.bin", b"bee")])]
    );
}

#[test]
fn a_cut_upload_is_converged_by_the_rerun_fake() {
    on_fake(case_a_cut_upload_is_converged_by_the_rerun);
}

#[test]
fn a_cut_upload_is_converged_by_the_rerun_agh() {
    on_agh(case_a_cut_upload_is_converged_by_the_rerun);
}

fn case_publish_draft_keeps_a_prerelease_and_a_missing_tag_is_not_found(w: &dyn WriteWorld) {
    w.lay("v1", true, &[("a.bin", b"ay")]);
    let s = w.slug();
    w.host().publish_draft(&s, "v1").expect("the publish");
    assert_eq!(w.read("v1"), vec![held(false, &[("a.bin", b"ay")])]);
    assert_eq!(w.host().publish_draft(&s, "v9"), Err(HostError::NotFound));
}

#[test]
fn publish_draft_keeps_a_prerelease_and_a_missing_tag_is_not_found_fake() {
    on_fake(case_publish_draft_keeps_a_prerelease_and_a_missing_tag_is_not_found);
}

#[test]
fn publish_draft_keeps_a_prerelease_and_a_missing_tag_is_not_found_agh() {
    on_agh(case_publish_draft_keeps_a_prerelease_and_a_missing_tag_is_not_found);
}

/// A bare repository at `at` holding `work`'s commits: its head.
fn bare_repo(work: &Path, at: &Path) -> String {
    let head = super::repo(work, 2);
    let parent = at.parent().expect("a parent");
    super::git(
        parent,
        &[
            "clone",
            "--quiet",
            "--bare",
            &work.to_string_lossy(),
            &at.to_string_lossy(),
        ],
    );
    head
}

fn case_clone_then_fetch(w: &dyn WriteWorld) {
    let head = bare_repo(&w.dir().join("work"), &w.remote());
    let dest = w.dir().join("clone");
    w.host().clone_repo(&w.slug(), &dest).expect("the clone");
    assert_eq!(super::git(&dest, &["rev-parse", "HEAD"]), head);
    w.host().fetch_repo(&w.slug(), &dest).expect("the fetch");
}

#[test]
fn clone_then_fetch_fake() {
    on_fake(case_clone_then_fetch);
}

#[test]
fn clone_then_fetch_agh() {
    on_agh(case_clone_then_fetch);
}

/// A lane's anonymous clone and fetch through agh use a progress-inactivity bound. The lane's
/// four-hour wall budget remains in the inert field that authenticated reads use, but cannot kill
/// a large anonymous clone whose transfer keeps advancing.
#[test]
fn a_lane_anonymous_clone_and_fetch_run_while_progress_continues() {
    use atpkg_publish::host::agh::LANE_GIT_BUDGET;
    let w = AghWorld::build(Setup {
        dev_token: false,
        dev_anonymous: true,
        ..Setup::DEV
    });
    bare_repo(&w.dir().join("work"), &w.remote());
    let dest = w.dir().join("clone");
    w.host.clone_repo(&w.slug(), &dest).expect("the clone");
    w.host.fetch_repo(&w.slug(), &dest).expect("the fetch");
    let runs: Vec<_> =
        w.fx.children_run()
            .into_iter()
            .filter(|spec| spec.args.iter().any(|arg| arg == "clone" || arg == "fetch"))
            .collect();
    assert_eq!(runs.len(), 2, "{runs:?}");
    for spec in &runs {
        assert_eq!(spec.budget, LANE_GIT_BUDGET, "{:?}", spec.args);
        let progress = spec.progress.as_ref().expect("anonymous progress bound");
        assert_eq!(progress.window, Duration::from_secs(60), "{:?}", spec.args);
    }
}

/// A clone that fails into a directory it made leaves no directory behind, so the next run
/// clones again rather than fetching into a half-made `.git`; a directory that was there
/// before stays.
#[test]
fn a_failed_lane_clone_leaves_no_directory_it_made() {
    let w = AghWorld::new();
    let fresh = w.dir().join("fresh");
    assert!(
        w.host.clone_repo(&w.slug(), &fresh).is_err(),
        "no remote laid"
    );
    assert!(!fresh.exists());
    let kept = w.dir().join("kept");
    std::fs::create_dir(&kept).expect("an empty directory");
    assert!(
        w.host.clone_repo(&w.slug(), &kept).is_err(),
        "no remote laid"
    );
    assert!(kept.is_dir());
}

/// Agh owns a private sibling stage, not the caller's destination. If another actor creates the
/// destination after clone admission, publication refuses and the host wrapper must not revive
/// its obsolete partial-clone cleanup by deleting that actor's files.
#[test]
fn a_failed_lane_clone_preserves_a_concurrent_destination() {
    let w = AghWorld::build_with_concurrent_clone_destination(Setup::DEV, Some("raced"));
    bare_repo(&w.dir().join("work"), &w.remote());
    let destination = w.dir().join("raced");
    let error = w
        .host
        .clone_repo(&w.slug(), &destination)
        .expect_err("the concurrent nonempty destination refuses publication");
    assert!(
        error.to_string().contains("without replacing caller files"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(destination.join("theirs")).expect("concurrent file preserved"),
        b"keep"
    );
}

/// During the swap both hosts write the same staging releases, so a clobber drops the other
/// host's leftover too: a cut `RestHost` replace's `{name}.atpkg-staged` beside a cut agh
/// clobber's `{name}.agh-{8 hex}`. The release ends holding the file alone, so the mirror
/// finds no extra asset. `RestHost`'s half is `channel::rest`'s
/// `a_clobber_drops_either_host_s_leftover_temporary`.
#[test]
fn a_clobber_drops_either_host_s_leftover_temporary() {
    let w = AghWorld::new();
    w.lay(
        "v1",
        true,
        &[
            ("a.toml.agh-00000000", b"old manifest"),
            ("a.toml.atpkg-staged", b"a stale stage"),
        ],
    );
    let bytes: &[u8] = b"new manifest";
    w.host()
        .upload(
            &slug(),
            "v1",
            &[file(w.dir(), "a.toml", bytes)],
            Replace::Clobber,
        )
        .expect("the clobber");
    assert_eq!(w.read("v1"), vec![held(true, &[("a.toml", bytes)])]);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

/// A person's source checkout of `w.remote()` at `{dir}/checkout`, its origin the GitHub URL
/// of `w.slug()` (never contacted: `check_on_main` reads only its name), and its refs.
fn source_checkout(w: &dyn WriteWorld) -> PathBuf {
    let checkout = w.dir().join("checkout");
    super::git(
        w.dir(),
        &[
            "clone",
            "--quiet",
            &w.remote().to_string_lossy(),
            &checkout.to_string_lossy(),
        ],
    );
    let slug = w.slug();
    super::git(
        &checkout,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("https://github.com/{}/{}.git", slug.owner, slug.repo),
        ],
    );
    checkout
}

/// `fetch_main` reads the host's main into the record ref alone: no tag, no remote-tracking
/// ref and no branch of the checkout moves. `check_on_main`, which every harness pack and
/// package source check runs, passes a commit main holds and refuses a local one, and leaves
/// no record ref behind.
fn case_fetch_main_reads_main_into_the_record_ref_alone(w: &dyn WriteWorld) {
    use atpkg_publish::artifact::package::{MAIN_OF_RECORD, check_on_main};
    let first = bare_repo(&w.dir().join("work"), &w.remote());
    let checkout = source_checkout(w);
    let remote = w.remote();
    let tree = super::git(&remote, &["rev-parse", "main^{tree}"]);
    let head = super::git(
        &remote,
        &["commit-tree", &tree, "-p", &first, "-m", "three"],
    );
    super::git(&remote, &["update-ref", "refs/heads/main", &head]);
    super::git(&remote, &["tag", "v9", &head]);
    w.set_main(&head);
    let refs = || super::git(&checkout, &["for-each-ref"]);
    let before = refs();
    w.host()
        .fetch_main(&w.slug(), &checkout, MAIN_OF_RECORD)
        .expect("the main of record");
    assert_eq!(
        super::git(&checkout, &["rev-parse", "--verify", MAIN_OF_RECORD]),
        head
    );
    super::git(&checkout, &["update-ref", "-d", MAIN_OF_RECORD]);
    assert_eq!(refs(), before, "fetch_main wrote another ref");
    let slug = w.slug();
    check_on_main(
        w.host(),
        &checkout,
        &slug.owner,
        &slug.repo,
        "harness",
        &head,
    )
    .expect("main holds the commit");
    super::git(
        &checkout,
        &["commit", "--quiet", "--allow-empty", "-m", "local"],
    );
    let local = super::git(&checkout, &["rev-parse", "HEAD"]);
    let refused = check_on_main(
        w.host(),
        &checkout,
        &slug.owner,
        &slug.repo,
        "harness",
        &local,
    )
    .expect_err("main does not hold a local commit");
    assert!(matches!(refused, Error::Refused(_)), "{refused:?}");
    assert!(
        super::git(&checkout, &["for-each-ref", "refs/atpkg-publish/"]).is_empty(),
        "no record ref is left behind"
    );
}

#[test]
fn fetch_main_reads_main_into_the_record_ref_alone_fake() {
    on_fake(case_fetch_main_reads_main_into_the_record_ref_alone);
}

#[test]
fn fetch_main_reads_main_into_the_record_ref_alone_agh() {
    on_agh(case_fetch_main_reads_main_into_the_record_ref_alone);
}

/// The API names the main of record: a fetch that brings another main (the API read before
/// main moved, or git transported another) is `Unreachable`, and the record ref is removed.
#[test]
fn a_fetched_main_the_api_does_not_name_is_unreachable_and_dropped() {
    use atpkg_publish::artifact::package::MAIN_OF_RECORD;
    let w = AghWorld::new();
    let first = bare_repo(&w.dir().join("work"), &w.remote());
    let checkout = source_checkout(&w);
    let tree = super::git(&w.remote(), &["rev-parse", "main^{tree}"]);
    let stale = super::git(&w.remote(), &["commit-tree", &tree, "-m", "stale"]);
    w.set_main(&stale);
    let err = w
        .host()
        .fetch_main(&w.slug(), &checkout, MAIN_OF_RECORD)
        .expect_err("the fetched main is not the API's");
    assert!(matches!(err, HostError::Unreachable(_)), "{err:?}");
    assert!(
        super::git(&checkout, &["for-each-ref", "refs/atpkg-publish/"]).is_empty(),
        "the record ref is removed"
    );
    w.set_main(&first);
    w.host()
        .fetch_main(&w.slug(), &checkout, MAIN_OF_RECORD)
        .expect("the API's main");
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn an_open_intent_never_posts_again_and_a_later_read_observes_it() {
    let w = AghWorld::new();
    let files = [file(w.dir(), "a.bin", b"ay")];
    w.fake.set_listing_lag(SLUG, true);
    w.fake.fault(
        Match::api(&w.releases_path()).method("POST"),
        Fault::LandThenClose,
    );
    unreachable(create(&w, "v1", &files));
    let intents = w.intents();
    let [(id, true)] = intents.as_slice() else {
        panic!("one open intent: {intents:?}");
    };
    assert_eq!(
        create(&w, "v1", &files),
        Err(HostError::Unreachable(format!(
            "outcome unknown; intent {id} open"
        )))
    );
    assert_eq!(w.posts(), 1);
    w.fake.apply(Change::Reveal {
        release_id: w.id_of("v1"),
    });
    let read = w.host.release(&slug(), "v1").expect("the draft");
    assert!(read.draft, "{read:?}");
    assert_eq!(w.intents(), vec![(id.clone(), false)]);
    create(&w, "v1", &files).expect("the rerun");
    assert_eq!(w.posts(), 1);
    assert_eq!(w.read("v1"), vec![held(false, &[("a.bin", b"ay")])]);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn release_tier_writes_need_the_atpkg_publish_grant() {
    let w = AghWorld::build(Setup {
        grant: false,
        ..Setup::RELEASE
    });
    let files = [file(w.dir(), "a.bin", b"ay")];
    match create(&w, "v1", &files) {
        Err(HostError::Refused(why)) => assert!(
            why.contains("agh policy grant atpkg-publish alabsystems/aterm"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());

    let w = AghWorld::build(Setup::RELEASE);
    let files = [file(w.dir(), "a.bin", b"ay")];
    create(&w, "v1", &files).expect("the granted create");
    let path = w.releases_path();
    let post = w
        .fake
        .seen()
        .into_iter()
        .find(|s| s.method == "POST" && s.path_and_query == path)
        .expect("the POST");
    assert_eq!(post.as_login.as_deref(), Some("fixture-release-bot"));
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn write_preflight_checks_push() {
    let w = AghWorld::build(Setup {
        push: false,
        ..Setup::DEV
    });
    match w.host.write_preflight(&slug()) {
        Err(HostError::Refused(why)) => {
            assert!(why.contains("fixture-dev-bot"), "{why}");
            assert!(why.contains(SLUG), "{why}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(w.writes(), 0);

    let w = AghWorld::new();
    assert_eq!(w.host.write_preflight(&slug()), Ok(()));
    assert_eq!(w.writes(), 0);

    let w = AghWorld::build(Setup {
        dev_token: false,
        ..Setup::DEV
    });
    match w.host.write_preflight(&slug()) {
        Err(HostError::Refused(why)) => {
            // The owner by itself, not inside the login or the slug the push refusal names.
            let rest = why.replace("fixture-dev-bot", "").replace(SLUG, "");
            assert!(rest.contains(OWNER), "{why}");
        }
        other => panic!("{other:?}"),
    }
    // Refused before anything is read: no push check, no anonymous permissions read.
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
}

/// The pre-claim check opens the write journal `create` needs first, so a state directory
/// agh refuses (open to other users) fails it before a build, with nothing sent.
#[test]
fn write_preflight_checks_the_write_journal() {
    use std::os::unix::fs::PermissionsExt;
    let w = AghWorld::new();
    std::fs::create_dir_all(&w.dirs.state).expect("the state directory");
    std::fs::set_permissions(&w.dirs.state, std::fs::Permissions::from_mode(0o755))
        .expect("the state directory's mode");
    let r = w.host.write_preflight(&slug());
    assert!(r.is_err(), "{r:?}");
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
}

#[test]
fn a_rate_limited_create_is_unreachable_and_posts_once() {
    let w = AghWorld::new();
    w.fake.fault(
        Match::api(&w.releases_path()).method("POST"),
        Fault::SecondaryLimit { retry_after: 1 },
    );
    unreachable(create(&w, "v1", &[file(w.dir(), "a.bin", b"ay")]));
    assert_eq!(w.posts(), 1);
    assert!(w.read("v1").is_empty(), "{:?}", w.read("v1"));
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

/// A draft `v1` holding `a.bin` as an upload in progress leaves it: state `starter`, no bytes.
fn lay_starter(w: &AghWorld) {
    let id = w
        .fake
        .add_release(SLUG, ReleaseSpec::draft("v1", AT).prerelease(true));
    w.fake.add_asset(
        id,
        AssetSpec {
            state: "starter".to_string(),
            bytes: Vec::new(),
            ..AssetSpec::uploaded("a.bin", b"")
        },
    );
}

#[test]
fn a_failed_guard_keeps_its_own_next_and_drops_the_generic_one() {
    let w = AghWorld::new();
    lay_starter(&w);
    match w.host.upload(
        &slug(),
        "v1",
        &[file(w.dir(), "a.bin", b"ay")],
        Replace::Never,
    ) {
        Err(HostError::Refused(why)) => {
            let (_, next) = why
                .split_once("; next: ")
                .unwrap_or_else(|| panic!("{why}"));
            assert!(
                next.contains("agh asset delete fixture-dev/packs v1 a.bin"),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
    match w.host.publish_draft(&slug(), "v1") {
        Err(HostError::Refused(why)) => {
            assert!(why.contains("a.bin"), "{why}");
            assert!(!why.contains("; next: "), "{why}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(w.writes(), 0, "{:?}", w.fake.seen());
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn a_release_tier_clone_without_a_grant_is_refused() {
    let w = AghWorld::build(Setup {
        grant: false,
        ..Setup::RELEASE
    });
    bare_repo(&w.dir().join("work"), &w.remote());
    let dest = w.dir().join("clone");
    let release = slug_of(RELEASE_SLUG);
    match w.host.clone_repo(&release, &dest) {
        Err(HostError::Refused(why)) => assert!(
            why.contains("agh policy grant atpkg-publish alabsystems/aterm"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    let grants: Vec<_> = std::fs::read_dir(w.dirs.grants())
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(grants.is_empty(), "{grants:?}");
    assert!(!dest.join(".git").exists());

    let w = AghWorld::build(Setup::RELEASE);
    let head = bare_repo(&w.dir().join("work"), &w.remote());
    let dest = w.dir().join("clone");
    w.host
        .clone_repo(&release, &dest)
        .expect("the granted clone");
    assert_eq!(super::git(&dest, &["rev-parse", "HEAD"]), head);
}

#[test]
fn a_refused_create_with_nothing_there_is_refused() {
    let rig = Rig::new("write-contract-f1");
    rig.host.fail_next(FakeOp::Create, FakeFail::Refuse);
    let dir = rig.path("up");
    std::fs::create_dir_all(&dir).expect("the pack dir");
    let manifest = dir.join("pkg-ay-9.toml");
    std::fs::write(
        &manifest,
        "schema = 1\nprogram = \"ay\"\nversion = \"0.1.0+gaaaaaaaaa\"\nbuild_number = 9\n",
    )
    .expect("the manifest");
    let archive = dir.join("ay-9-aarch64-apple-darwin.tar.zst");
    std::fs::write(&archive, b"the archive").expect("the archive");
    let tag = "atpkg-ay-9";
    let staging = rig.ctx.staging_slug("ay");
    let e = upload::publish(
        &rig.ctx,
        "ay",
        tag,
        &manifest,
        &[archive, manifest.clone()],
        "notes",
    )
    .expect_err("a refused create with nothing there");
    assert!(matches!(e, Error::Refused(_)), "{e}");
    let why = e.to_string();
    assert!(why.contains(tag), "{why}");
    assert!(why.contains(&staging.to_string()), "{why}");
    assert!(why.contains("refused by the test"), "{why}");
    let calls = rig.host.calls();
    assert!(!calls.iter().any(|c| c.starts_with("upload ")), "{calls:?}");
}

#[test]
fn a_draft_seed_is_not_found() {
    let host = FakeHost::new();
    let seed = Slug::new("alabsystems", "trust");
    host.put(
        &seed,
        "seed-1",
        FakeRelease {
            draft: true,
            prerelease: true,
            assets: BTreeMap::new(),
        },
    );
    assert_eq!(
        host.published_by_tag(&seed, "seed-1"),
        Err(HostError::NotFound)
    );
    let draft = host.release(&seed, "seed-1").expect("the draft");
    assert!(draft.draft, "{draft:?}");
}

/// F5's call site: `channel::seed::publish_refusals` reads the seed release as published only,
/// through the host of the owner the ledger names. A trust origin whose main carries the re-pin
/// and whose ledger names `{ledger_owner}/trust` `SEED_TAG`, cloned into `trust`, with a seal
/// receipt for it.
struct SeedFixture {
    rig: Rig,
    trust: PathBuf,
    seal: PathBuf,
}

const SEED_TAG: &str = "trust-stage0-2026-11-01-fixture";
const SEED_COMMIT: &str = "1234567890123456789012345678901234567890";

fn seed_fixture(label: &str, ledger_owner: &str) -> SeedFixture {
    use atpkg_publish::channel::seed::{GENERATED_MARKER, LEDGER};
    let rig = Rig::new(label);
    let origin = rig.path("trust-origin");
    std::fs::create_dir_all(origin.join("src")).expect("mkdir src");
    std::fs::create_dir_all(origin.join(Path::new(LEDGER).parent().expect("a parent")))
        .expect("mkdir ledger");
    super::git(&origin, &["init", "-q", "-b", "main"]);
    std::fs::write(
        origin.join("src/stage0"),
        format!(
            "compiler_git_commit_hash={SEED_COMMIT}\n{GENERATED_MARKER}\ndist/minted.tar.xz=abc\n"
        ),
    )
    .expect("stage0");
    std::fs::write(
        origin.join(LEDGER),
        format!("[release]\nrepo = \"{ledger_owner}/trust\"\ntag = \"{SEED_TAG}\"\n"),
    )
    .expect("ledger");
    super::git(&origin, &["add", "-A"]);
    super::git(&origin, &["commit", "-q", "-m", "re-pin"]);
    let repin = super::git(&origin, &["rev-parse", "HEAD"]);
    let trust = rig.path("trust");
    super::git(
        rig.tmp.path(),
        &[
            "clone",
            "-q",
            &origin.to_string_lossy(),
            &trust.to_string_lossy(),
        ],
    );
    let seal = rig.path("seal");
    std::fs::create_dir_all(&seal).expect("seal");
    std::fs::write(
        seal.join(atpkg::lineage::SEED_MINT_FILE),
        format!(
            "repin_commit={repin}\nrepin_release_tag={SEED_TAG}\n\
             repin_compiler_git_commit_hash={SEED_COMMIT}\n"
        ),
    )
    .expect("receipt");
    SeedFixture { rig, trust, seal }
}

fn seed_release(draft: bool) -> FakeRelease {
    FakeRelease {
        draft,
        prerelease: true,
        assets: BTreeMap::from([("stage0.tar.xz".to_string(), b"x".to_vec())]),
    }
}

#[test]
fn a_draft_seed_refuses_the_publish_through_its_owners_host() {
    let mut f = seed_fixture("write-contract-f5", "fixture-ext");
    f.rig.ctx.accounts.staging = "fixture-dev".to_string();
    f.rig.ctx.accounts.public = "fixture-ext".to_string();
    let staging = Arc::new(FakeHost::new());
    let channel = Arc::new(FakeHost::new());
    let seed = Slug::new("fixture-ext", "trust");
    channel.put(&seed, SEED_TAG, seed_release(true));
    f.rig.ctx.hosts = Hosts {
        staging: staging.clone(),
        channel: channel.clone(),
    };
    let refusals = atpkg_publish::channel::seed::publish_refusals(&f.rig.ctx, &f.trust, &f.seal);
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(refusals[0].contains(SEED_TAG), "{refusals:?}");
    let read = format!("release {seed} {SEED_TAG}");
    assert!(channel.calls().contains(&read), "{:?}", channel.calls());
    assert!(staging.calls().is_empty(), "{:?}", staging.calls());

    channel.put(&seed, SEED_TAG, seed_release(false));
    let refusals = atpkg_publish::channel::seed::publish_refusals(&f.rig.ctx, &f.trust, &f.seal);
    assert_eq!(refusals, Vec::<String>::new());
}

#[test]
fn a_long_staging_listing_still_resolves() {
    let w = AghWorld::new();
    for n in 0..2100 {
        w.fake.add_release(
            SLUG,
            ReleaseSpec::published(&format!("r{n}"), "2025-01-01T00:00:00Z"),
        );
    }
    w.fake
        .add_release(SLUG, ReleaseSpec::draft("v1", AT).prerelease(true));
    w.host
        .upload(
            &slug(),
            "v1",
            &[file(w.dir(), "a.bin", b"ay")],
            Replace::Never,
        )
        .expect("the upload");
    let past: Vec<String> = w
        .fake
        .seen()
        .into_iter()
        .map(|s| s.path_and_query)
        .filter(|path| {
            path.split(['?', '&'])
                .filter_map(|part| part.strip_prefix("page="))
                .any(|page| page.parse::<u32>().is_ok_and(|n| n > 5))
        })
        .collect();
    assert!(past.is_empty(), "{past:?}");
    assert_eq!(w.read("v1"), vec![held(true, &[("a.bin", b"ay")])]);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn pre_claim_checks_ask_the_host_under_agh() {
    let rig = Rig::new("write-contract-f2");
    let staging = rig.ctx.staging_slug(atpkg::manifest::INDEX_REPO);
    let ctx = rig.ctx.clone().with_host_choice(HostChoice::Agh);
    rig.host.fail_next(FakeOp::Preflight, FakeFail::Refuse);
    let e = upload::token_preflight(&ctx).expect_err("the armed preflight");
    let why = e.to_string();
    assert!(why.contains(&staging.to_string()), "{why}");
    assert!(why.contains("refused by the test"), "{why}");
    assert!(
        rig.host.calls().contains(&format!("preflight {staging}")),
        "{:?}",
        rig.host.calls()
    );
    upload::token_preflight(&ctx).expect("unarmed");

    // A pack's check asks the program repository its upload goes to: under agh push and a
    // grant are per repository, so the index repository's answer says nothing of it.
    let trust = rig.ctx.staging_slug("trust");
    rig.host.fail_next(FakeOp::Preflight, FakeFail::Refuse);
    let e = upload::token_preflight_for(&ctx, "trust").expect_err("the armed preflight");
    assert!(matches!(e, Error::Refused(_)), "{e}");
    let why = e.to_string();
    assert!(why.contains(&trust.to_string()), "{why}");
    assert!(why.contains("refused by the test"), "{why}");
    assert_eq!(rig.host.calls().last(), Some(&format!("preflight {trust}")));

    let rig = Rig::new("write-contract-f2-gh");
    let e = upload::token_preflight(&rig.ctx).expect_err("no token file");
    assert!(e.to_string().contains("~/.secrets/gh_access_token"), "{e}");
    assert!(
        !rig.host.calls().iter().any(|c| c.starts_with("preflight ")),
        "{:?}",
        rig.host.calls()
    );
}

/// The four pack verbs' `--upload` check asks the repository each uploads to (F2 site 1), once,
/// before anything is built: a refusal of that repository alone stops the verb, names its
/// staging slug, and leaves no output directory.
#[test]
fn each_pack_verb_asks_its_programs_repository_before_building_under_agh() {
    use atpkg_publish::artifact::{bottle, bundle, pack, package};
    type Verb = fn(&atpkg_publish::ctx::Ctx, &[String]) -> atpkg_publish::error::Result<()>;
    let rig = Rig::new("write-contract-f2-verbs");
    let ctx = rig.ctx.clone().with_host_choice(HostChoice::AghStaging);
    let dir = rig.tmp.path();
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let words = |args: &[&str]| args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>();
    let cases: [(&str, Verb, Vec<String>, String); 4] = [
        (
            "muse-alab",
            pack::run,
            words(&[
                "muse-alab",
                "--checkout",
                &path("checkout"),
                "--triple",
                "aarch64-apple-darwin",
                "--out",
                &path("out-pack"),
                "--upload",
            ]),
            path("out-pack"),
        ),
        (
            "trust",
            bundle::run,
            words(&[
                "trust",
                "--sysroot",
                &path("sysroot"),
                "--src-repo",
                &path("src"),
                "--out",
                &path("out-bundle"),
                "--upload",
            ]),
            path("out-bundle"),
        ),
        (
            "gh",
            bottle::run,
            words(&[
                "gh",
                "--archive",
                &path("bottle.tar.gz"),
                "--version",
                "2.0.0",
                "--build",
                "1",
                "--triple",
                "aarch64-apple-darwin",
                "--out",
                &path("out-bottle"),
                "--upload",
            ]),
            path("out-bottle"),
        ),
        (
            "codex",
            package::run,
            words(&[
                &path("package"),
                "--program",
                "codex",
                "--build",
                "2026110201",
                "--out",
                &path("out-package"),
                "--upload",
            ]),
            path("out-package"),
        ),
    ];
    for (program, run, args, out) in cases {
        let repo = atpkg_publish::programs::get(program)
            .expect("a pinned program")
            .repo;
        let slug = rig.ctx.staging_slug(repo);
        let before = rig.host.calls().len();
        rig.host.fail_next(FakeOp::Preflight, FakeFail::Refuse);
        let e = run(&ctx, &args).expect_err(program);
        assert!(matches!(e, Error::Refused(_)), "{program}: {e}");
        assert!(e.to_string().contains(&slug.to_string()), "{program}: {e}");
        assert_eq!(
            rig.host.calls()[before..],
            [format!("preflight {slug}")],
            "{program}"
        );
        assert!(
            !Path::new(&out).exists(),
            "{program} built before its check"
        );
    }
}

/// A channel host whose draft-inclusive reads panic: only its published reads answer.
struct PublishedOnly(FakeHost);

impl ReleaseHost for PublishedOnly {
    fn releases(&self, _: &Slug) -> Result<Vec<Release>, HostError> {
        panic!("a published-only site read the draft-inclusive listing")
    }

    fn release(&self, _: &Slug, _: &str) -> Result<Release, HostError> {
        panic!("a published-only site read a release as the writer")
    }

    fn published_releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
        self.0.published_releases(slug)
    }

    fn download(&self, _: &Slug, _: &str, _: &[&str], _: &Path) -> Result<(), HostError> {
        panic!("download")
    }

    fn create(&self, _: &Slug, _: &str, _: &str, _: &str, _: &[PathBuf]) -> Result<(), HostError> {
        panic!("create")
    }

    fn upload(&self, _: &Slug, _: &str, _: &[PathBuf], _: Replace) -> Result<(), HostError> {
        panic!("upload")
    }

    fn publish_draft(&self, _: &Slug, _: &str) -> Result<(), HostError> {
        panic!("publish_draft")
    }

    fn probe_public(&self, _: &Slug, _: &str, _: &str) -> Probe {
        panic!("probe_public")
    }

    fn fetch_public(&self, _: &Slug, _: &str, _: &str, _: &Path, _: u64) -> Probe {
        panic!("fetch_public")
    }

    fn clone_repo(&self, _: &Slug, _: &Path) -> Result<(), HostError> {
        panic!("clone_repo")
    }

    fn fetch_repo(&self, _: &Slug, _: &Path) -> Result<(), HostError> {
        panic!("fetch_repo")
    }
}

#[test]
fn published_only_sites_read_as_read() {
    let mut rig = Rig::new("write-contract-published");
    let index = rig.index_slug();
    let fake = FakeHost::new();
    for (n, draft) in [(1, false), (2, false), (3, true)] {
        fake.put(
            &index,
            &atpkg_publish::quad::index_tag(n),
            FakeRelease {
                draft,
                prerelease: true,
                assets: BTreeMap::new(),
            },
        );
    }
    rig.ctx.hosts = Hosts::single(Arc::new(PublishedOnly(fake)));
    assert_eq!(
        atpkg_publish::quad::head_build(&rig.ctx, &index),
        Ok(Some(2))
    );
}
