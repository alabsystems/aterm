// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The read contract (docs/DESIGN-agh-2026-10-19.md §10.3): one set of cases, run unchanged
//! against [`FakeHost`] and against [`AghHost`] over agh-fake, so the release host agh will
//! become answers every read the lanes make exactly as the host their tests drive. Then what
//! only AghHost does: its M2 refusals, its anonymous reads, the absent writer, one owner per
//! host and ambiguous drafts.
//!
//! Unix only: the AghHost world writes its token into a private file (`aterm_uds::private_file`).

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agh::fx::Fx;
use agh::identity::MapEnv;
use agh::policy::Router;
use agh::retry::RetryPolicy;
use agh::wire::Endpoints;
use agh::wire::http::HttpTransport;
use agh_fake::{AssetSpec, Change, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec};
use atpkg_publish::channel::{DOWNLOAD_ATTEMPTS, download_retried};
use atpkg_publish::host::agh::AghHost;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::host::{HostError, Probe, Release, ReleaseHost, Replace, Slug};

use super::scratch;

/// The repository every case reads.
const OWNER: &str = "fixture-dev";
const REPO: &str = "packs";
const SLUG: &str = "fixture-dev/packs";
/// The asset API's path prefix: an authenticated download is a GET under it.
const ASSETS: &str = "/repos/fixture-dev/packs/releases/assets/";
/// The simulated clock's start, in Unix seconds.
const SIM_START: u64 = 1_760_000_000;

fn slug() -> Slug {
    Slug::new(OWNER, REPO)
}

/// One host and the means to lay its state and its faults.
trait World {
    /// The host under test.
    fn host(&self) -> &dyn ReleaseHost;
    /// Lay release `tag` with these assets.
    fn put(&self, tag: &str, draft: bool, prerelease: bool, assets: &[(&str, &[u8])]);
    /// Every authenticated asset download answers an HTTP 500 from now on.
    fn fail_downloads(&self);
    /// The repository becomes invisible to this identity.
    fn hide(&self);
    /// A draft beside the published release `tag`.
    fn put_stray_draft(&self, tag: &str);
    /// The download attempts so far.
    fn download_attempts(&self) -> usize;
    /// The warning lines the host wrote, where it keeps them.
    fn warnings(&self) -> Option<Vec<String>>;
}

/// [`FakeHost`], with slug `fixture-dev/packs`.
struct FakeWorld {
    host: FakeHost,
}

impl FakeWorld {
    fn new() -> FakeWorld {
        FakeWorld {
            host: FakeHost::new(),
        }
    }
}

impl World for FakeWorld {
    fn host(&self) -> &dyn ReleaseHost {
        &self.host
    }

    fn put(&self, tag: &str, draft: bool, prerelease: bool, assets: &[(&str, &[u8])]) {
        self.host.put(
            &slug(),
            tag,
            atpkg_publish::host::fake::FakeRelease {
                draft,
                prerelease,
                ..Default::default()
            },
        );
        for (name, bytes) in assets {
            self.host.put_asset(&slug(), tag, name, bytes.to_vec());
        }
    }

    fn fail_downloads(&self) {
        self.host.set_unreachable(OWNER, true);
    }

    fn hide(&self) {
        self.host.set_not_visible(&slug(), true);
    }

    fn put_stray_draft(&self, tag: &str) {
        self.host.put_stray_draft(&slug(), tag);
    }

    fn download_attempts(&self) -> usize {
        self.host
            .calls()
            .iter()
            .filter(|line| line.starts_with("download "))
            .count()
    }

    fn warnings(&self) -> Option<Vec<String>> {
        None
    }
}

/// [`AghHost`] over agh-fake, routed by an agh.toml.
struct AghWorld {
    fake: FakeGithub,
    host: AghHost,
    /// The client's simulated effects: every pause agh takes is in its `slept()`.
    fx: Arc<Fx>,
    _dir: aterm_tempfile::TempDir,
}

/// How an [`AghWorld`] is built. [`Setup::PLAN`] is PLAN §12's world; every other field
/// widens it for one AghHost-alone test.
#[derive(Clone, Copy)]
struct Setup {
    /// The dev token file is written.
    token: bool,
    /// `fixture-dev`'s row names `dev` as its writer.
    writer: bool,
    /// The client's own retry policy: only a test about retries passes another than
    /// `RetryPolicy::NONE` (PLAN §1 rule 8).
    policy: RetryPolicy,
    /// `fixture-dev-bot` holds push on the repository (else only pull).
    push: bool,
    /// `fixture-dev` reads through its own `reader` profile (login `fixture-reader`, pull on
    /// the repository), so a call that went through the read chain instead of as the writer
    /// shows in the logins `seen()` records.
    reader: bool,
    /// A second owner, `fixture-ext`, with its own writer `ext` (`fixture-ext-bot`, push on
    /// `fixture-ext/packs`, which holds a published `v1` with `a.bin`): a call the host let
    /// through for that owner would succeed, as that owner's writer.
    ext: bool,
}

impl Setup {
    /// PLAN §12's world: the dev token present, `fixture-dev`'s writer row, no retry.
    const PLAN: Setup = Setup {
        token: true,
        writer: true,
        policy: RetryPolicy::NONE,
        push: true,
        reader: false,
        ext: false,
    };
}

/// The other owner's repository, which [`Setup::ext`] lays.
const EXT_SLUG: &str = "fixture-ext/packs";

impl AghWorld {
    /// PLAN §12's world.
    fn new() -> AghWorld {
        AghWorld::build(Setup::PLAN)
    }

    /// `token`: the dev token file is written. `writer`: `fixture-dev`'s row names `dev`
    /// as its writer. The client retries nothing (PLAN §1 rule 8).
    fn with(token: bool, writer: bool) -> AghWorld {
        AghWorld::build(Setup {
            token,
            writer,
            ..Setup::PLAN
        })
    }

    /// [`AghWorld::with`], with the client's own retry policy `policy`: only a test about
    /// retries passes another than `RetryPolicy::NONE`.
    fn with_policy(token: bool, writer: bool, policy: RetryPolicy) -> AghWorld {
        AghWorld::build(Setup {
            token,
            writer,
            policy,
            ..Setup::PLAN
        })
    }

    /// The world `setup` names.
    fn build(setup: Setup) -> AghWorld {
        let Setup {
            token,
            writer,
            policy,
            push,
            reader,
            ext,
        } = setup;
        let fake = FakeGithub::start();
        let dir = scratch("agh-contract");
        let tokens = dir.path().join("tokens");
        aterm_uds::private_file::create_private_dir(&tokens).expect("the token directory");
        // One profile: its token registered as `login` and, when `written`, in its file.
        let profile = |name: &str, login: &str, written: bool| {
            let secret = agh_fake::token(name);
            fake.add_token(&secret, login);
            let file = format!("{name}.token");
            if written {
                aterm_uds::private_file::write_private(
                    &tokens,
                    &file,
                    format!("{secret}\n").as_bytes(),
                )
                .expect("the token file");
            }
            format!(
                "[profiles.{name}]\ntoken_file = \"{}\"\ntier = \"dev\"\nlogin = \"{login}\"\n\n",
                tokens.join(&file).display()
            )
        };
        let mut text = profile("dev", "fixture-dev-bot", token);
        let read = if reader {
            text.push_str(&profile("reader", "fixture-reader", true));
            "reader"
        } else {
            "dev"
        };
        if ext {
            text.push_str(&profile("ext", "fixture-ext-bot", true));
        }
        let write = if writer { "write = \"dev\"\n" } else { "" };
        text.push_str(&format!(
            "[[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"{read}\"]\n{write}"
        ));
        if ext {
            text.push_str(
                "\n[[owners]]\nname = \"fixture-ext\"\ntier = \"dev\"\nread = [\"ext\"]\nwrite = \"ext\"\n",
            );
        }
        let router = router(&text, dir.path());
        let ports = fake.ports();
        let fx = Arc::new(Fx::simulated(SIM_START));
        let github = agh::GithubBuilder::new(router)
            .transport(Arc::new(HttpTransport::direct(Arc::new(MapEnv::new()))))
            .endpoints(Endpoints::loopback(
                ports.api,
                ports.uploads,
                ports.web,
                ports.cdn,
            ))
            .env(Arc::new(MapEnv::new()))
            .fx(Arc::clone(&fx))
            .retry(policy)
            .caller("atpkg-publish")
            .build();
        let home = agh::Slug::new(OWNER, REPO).expect("the home slug");
        let host = AghHost::new(github, home).with_retry_pause(Duration::ZERO);
        let spec = if push {
            RepoSpec::public().push("fixture-dev-bot")
        } else {
            RepoSpec::public().pull("fixture-dev-bot")
        };
        fake.add_repo(
            SLUG,
            if reader {
                spec.pull("fixture-reader")
            } else {
                spec
            },
        );
        if ext {
            fake.add_repo(EXT_SLUG, RepoSpec::public().push("fixture-ext-bot"));
            let id = fake.add_release(
                EXT_SLUG,
                ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"),
            );
            fake.add_asset(id, AssetSpec::uploaded("a.bin", b"the other owner's bytes"));
        }
        AghWorld {
            fake,
            host,
            fx,
            _dir: dir,
        }
    }

    /// Every request the fake has seen so far.
    fn requests(&self) -> usize {
        self.fake.seen().len()
    }

    /// The listing GETs so far: the releases path itself, with or without a query.
    fn listing_gets(&self) -> usize {
        let path = "/repos/fixture-dev/packs/releases";
        self.fake
            .seen()
            .iter()
            .filter(|s| {
                s.origin == FakeOrigin::Api
                    && s.method == "GET"
                    && s.path_and_query
                        .strip_prefix(path)
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with('?'))
            })
            .count()
    }

    /// Every API request so far went as `fixture-dev`'s writer: every draft-inclusive read
    /// is WriterRead, never the read chain. Meaningful on a [`Setup::reader`] world, where
    /// the read chain is another login.
    fn assert_all_as_writer(&self) {
        for s in self
            .fake
            .seen()
            .iter()
            .filter(|s| s.origin == FakeOrigin::Api)
        {
            assert_eq!(
                s.as_login.as_deref(),
                Some("fixture-dev-bot"),
                "{} {}",
                s.method,
                s.path_and_query
            );
        }
    }
}

/// `config::parse`, `config::validate` (with `config::file_id`), then the Router.
fn router(text: &str, home: &Path) -> Router {
    let parsed = agh::config::parse(text).unwrap_or_else(|f| panic!("{}: {}", f.rule, f.what));
    let validated =
        agh::config::validate(&parsed, home, &agh::config::file_id).unwrap_or_else(|faults| {
            let faults: Vec<String> = faults
                .iter()
                .map(|f| format!("{}: {}", f.rule, f.what))
                .collect();
            panic!("{}", faults.join("; "))
        });
    Router::from_validated(validated)
}

impl World for AghWorld {
    fn host(&self) -> &dyn ReleaseHost {
        &self.host
    }

    fn put(&self, tag: &str, draft: bool, prerelease: bool, assets: &[(&str, &[u8])]) {
        let at = "2026-10-01T00:00:00Z";
        let spec = if draft {
            ReleaseSpec::draft(tag, at)
        } else {
            ReleaseSpec::published(tag, at)
        };
        let id = self.fake.add_release(SLUG, spec.prerelease(prerelease));
        for (name, bytes) in assets {
            self.fake.add_asset(id, AssetSpec::uploaded(name, bytes));
        }
    }

    fn fail_downloads(&self) {
        self.fake.fault_always(
            Match::api(ASSETS).method("GET"),
            Fault::Status {
                status: 500,
                body: String::new(),
                headers: vec![],
            },
        );
    }

    fn hide(&self) {
        self.fake.apply(Change::Hide { slug: SLUG.into() });
    }

    /// A prerelease, as the race loser's create always makes it (`gh release create
    /// --prerelease`) and as FakeHost lists its stray drafts.
    fn put_stray_draft(&self, tag: &str) {
        self.fake.add_release(
            SLUG,
            ReleaseSpec::draft(tag, "2026-10-02T00:00:00Z").prerelease(true),
        );
    }

    fn download_attempts(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|s| {
                s.origin == FakeOrigin::Api
                    && s.method == "GET"
                    && s.path_and_query.starts_with(ASSETS)
            })
            .count()
    }

    fn warnings(&self) -> Option<Vec<String>> {
        Some(self.host.warnings())
    }
}

/// `case` against a fresh [`AghWorld`], then the fake's own violations (a credential at the
/// web or CDN origin among them), which must be none. Then once more on a world whose read
/// chain is another login, where every API request must have gone as the writer.
fn on_agh(case: fn(&dyn World)) {
    let w = AghWorld::new();
    case(&w);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
    let w = AghWorld::build(Setup {
        reader: true,
        ..Setup::PLAN
    });
    case(&w);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
    w.assert_all_as_writer();
}

/// A release as compared across hosts: its asset names sorted.
fn normal(mut release: Release) -> Release {
    release.assets.sort();
    release
}

fn release(tag: &str, draft: bool, prerelease: bool, assets: &[&str]) -> Release {
    Release {
        tag: tag.to_string(),
        draft,
        prerelease,
        assets: assets.iter().map(|a| (*a).to_string()).collect(),
    }
}

// --- 1. releases ---------------------------------------------------------------------

fn case_releases(w: &dyn World) {
    w.put("v1", false, true, &[("b.bin", b"bee"), ("a.bin", b"ay")]);
    w.put("v2", true, true, &[("c.bin", b"sea")]);
    w.put("v3", false, false, &[]);
    let mut got: Vec<Release> = w
        .host()
        .releases(&slug())
        .expect("the listing")
        .into_iter()
        .map(normal)
        .collect();
    got.sort_by(|a, b| a.tag.cmp(&b.tag));
    assert_eq!(
        got,
        vec![
            release("v1", false, true, &["a.bin", "b.bin"]),
            release("v2", true, true, &["c.bin"]),
            release("v3", false, false, &[]),
        ]
    );
}

#[test]
fn releases_fake() {
    case_releases(&FakeWorld::new());
}

#[test]
fn releases_agh() {
    on_agh(case_releases);
}

// --- 2. release ----------------------------------------------------------------------

fn case_release(w: &dyn World) {
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    let got = w.host().release(&slug(), "v1").expect("the release");
    assert_eq!(normal(got), release("v1", false, true, &["a.bin"]));
    assert_eq!(w.host().release(&slug(), "v9"), Err(HostError::NotFound));
}

#[test]
fn release_fake() {
    case_release(&FakeWorld::new());
}

#[test]
fn release_agh() {
    on_agh(case_release);
}

// --- 3. download ---------------------------------------------------------------------

fn case_download(w: &dyn World) {
    w.put(
        "v1",
        false,
        true,
        &[("a.bin", b"ay bytes"), ("b.bin", b"bee bytes")],
    );
    let dir = scratch("contract-download");
    let out = dir.path().join("out");
    w.host()
        .download(&slug(), "v1", &["a.bin", "b.bin"], &out)
        .expect("the download");
    assert_eq!(
        std::fs::read(out.join("a.bin")).expect("a.bin"),
        b"ay bytes"
    );
    assert_eq!(
        std::fs::read(out.join("b.bin")).expect("b.bin"),
        b"bee bytes"
    );
    assert_eq!(
        w.host().download(&slug(), "v1", &["missing.bin"], &out),
        Err(HostError::NotFound)
    );
    assert!(!out.join("missing.bin").exists());
}

#[test]
fn download_fake() {
    case_download(&FakeWorld::new());
}

#[test]
fn download_agh() {
    on_agh(case_download);
}

// --- 4. not visible ------------------------------------------------------------------

fn case_not_visible(w: &dyn World) {
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    w.hide();
    let dir = scratch("contract-hidden");
    let releases = w.host().releases(&slug());
    assert!(
        matches!(releases, Err(HostError::Unreachable(_))),
        "{releases:?}"
    );
    let release = w.host().release(&slug(), "v1");
    assert!(
        matches!(release, Err(HostError::Unreachable(_))),
        "{release:?}"
    );
    let download = w.host().download(&slug(), "v1", &["a.bin"], dir.path());
    assert!(
        matches!(download, Err(HostError::Unreachable(_))),
        "{download:?}"
    );
    // A repository this identity cannot see is private or gone, so the download host serves
    // nothing for it either.
    assert_eq!(w.host().probe_public(&slug(), "v1", "a.bin"), Probe::Absent);
}

#[test]
fn not_visible_fake() {
    case_not_visible(&FakeWorld::new());
}

#[test]
fn not_visible_agh() {
    on_agh(case_not_visible);
}

// --- 5. attempts ---------------------------------------------------------------------

fn case_attempts(w: &dyn World) {
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    w.fail_downloads();
    let dir = scratch("contract-attempts");
    let retried = download_retried(w.host(), &slug(), "v1", &["a.bin"], dir.path());
    assert!(
        matches!(retried, Err(HostError::Unreachable(_))),
        "{retried:?}"
    );
    assert_eq!(w.download_attempts(), DOWNLOAD_ATTEMPTS);
    let once = w.host().download(&slug(), "v1", &["a.bin"], dir.path());
    assert!(matches!(once, Err(HostError::Unreachable(_))), "{once:?}");
    assert_eq!(w.download_attempts(), DOWNLOAD_ATTEMPTS + 1);
}

#[test]
fn attempts_fake() {
    case_attempts(&FakeWorld::new());
}

#[test]
fn attempts_agh() {
    on_agh(case_attempts);
}

// --- 6. probe_public -----------------------------------------------------------------

fn case_probe_public(w: &dyn World) {
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    w.put("v2", true, true, &[("b.bin", b"bee")]);
    assert_eq!(
        w.host().probe_public(&slug(), "v1", "a.bin"),
        Probe::Present
    );
    assert_eq!(w.host().probe_public(&slug(), "v2", "b.bin"), Probe::Absent);
    assert_eq!(
        w.host().probe_public(&slug(), "v1", "missing.bin"),
        Probe::Absent
    );
}

#[test]
fn probe_public_fake() {
    case_probe_public(&FakeWorld::new());
}

#[test]
fn probe_public_agh() {
    on_agh(case_probe_public);
}

// --- 7. fetch_public -----------------------------------------------------------------

fn case_fetch_public(w: &dyn World) {
    let bytes: &[u8] = b"the published asset's bytes";
    w.put("v1", false, true, &[("a.bin", bytes)]);
    let dir = scratch("contract-fetch");
    let dest = dir.path().join("got").join("a.bin");
    let cap = u64::try_from(bytes.len()).expect("a small size");
    assert_eq!(
        w.host().fetch_public(&slug(), "v1", "a.bin", &dest, cap),
        Probe::Present
    );
    assert_eq!(std::fs::read(&dest).expect("the fetched file"), bytes);
    // On `dest` itself, which still holds the published bytes: an answer that is not present
    // leaves no file there, whichever answer it is.
    let capped = w
        .host()
        .fetch_public(&slug(), "v1", "a.bin", &dest, cap - 1);
    assert!(matches!(capped, Probe::Unknown(_)), "{capped:?}");
    assert!(!dest.exists());
    // A file an earlier call left at `dest` does not outlive an answer that is not present.
    let stale = dir.path().join("stale").join("missing.bin");
    std::fs::create_dir_all(stale.parent().expect("a parent")).expect("the stale dir");
    std::fs::write(&stale, b"stale bytes").expect("the stale file");
    assert_eq!(
        w.host()
            .fetch_public(&slug(), "v1", "missing.bin", &stale, cap),
        Probe::Absent
    );
    assert!(!stale.exists());
}

#[test]
fn fetch_public_fake() {
    case_fetch_public(&FakeWorld::new());
}

#[test]
fn fetch_public_agh() {
    on_agh(case_fetch_public);
}

// --- 8. the race ---------------------------------------------------------------------

fn case_race(w: &dyn World) {
    w.put("v1", false, true, &[("a.bin", b"the winner's bytes")]);
    w.put_stray_draft("v1");
    let got = w
        .host()
        .release(&slug(), "v1")
        .expect("the published release");
    assert_eq!(normal(got), release("v1", false, true, &["a.bin"]));
    if let Some(lines) = w.warnings() {
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("v1"), "{lines:?}");
        assert!(lines[0].contains("1 draft(s)"), "{lines:?}");
    }
    // The listing shows both: the winner, and the loser's draft with no asset.
    let mut listed: Vec<Release> = w
        .host()
        .releases(&slug())
        .expect("the listing")
        .into_iter()
        .map(normal)
        .collect();
    listed.sort_by(|a, b| (&a.tag, a.draft).cmp(&(&b.tag, b.draft)));
    assert_eq!(
        listed,
        vec![
            release("v1", false, true, &["a.bin"]),
            release("v1", true, true, &[]),
        ]
    );
    let dir = scratch("contract-race");
    w.host()
        .download(&slug(), "v1", &["a.bin"], dir.path())
        .expect("the published release's download");
    assert_eq!(
        std::fs::read(dir.path().join("a.bin")).expect("a.bin"),
        b"the winner's bytes"
    );
}

#[test]
fn race_fake() {
    case_race(&FakeWorld::new());
}

#[test]
fn race_agh() {
    on_agh(case_race);
}

// --- FakeHost alone ------------------------------------------------------------------

#[test]
#[should_panic(expected = "put_stray_draft")]
fn fake_stray_draft_needs_a_published_release() {
    // A stray beside a draft (or beside nothing) is a state FakeHost would answer otherwise
    // than GitHub, so laying one fails at its own line.
    let w = FakeWorld::new();
    w.put("v1", true, true, &[]);
    w.put_stray_draft("v1");
}

// --- AghHost alone -------------------------------------------------------------------

#[test]
fn agh_writes_and_git_are_refused() {
    let w = AghWorld::new();
    let dir = scratch("contract-writes");
    let host = &w.host;
    let refused =
        |r: Result<(), HostError>| assert!(matches!(r, Err(HostError::Refused(_))), "{r:?}");
    refused(host.create(&slug(), "v1", "title", "notes", &[]));
    refused(host.upload(&slug(), "v1", &[], Replace::Never));
    refused(host.upload(&slug(), "v1", &[], Replace::Clobber));
    refused(host.publish_draft(&slug(), "v1"));
    refused(host.clone_repo(&slug(), &dir.path().join("clone")));
    refused(host.fetch_repo(&slug(), dir.path()));
    assert_eq!(w.requests(), 0);
}

#[test]
fn agh_refuses_a_listed_asset_name_outside_dir() {
    // The names come from the reply's own listing (the mirror lane passes them back), so a
    // name `dir.join` would carry out of `dir`, or a part file's name, is refused before any
    // byte moves.
    let w = AghWorld::new();
    w.put(
        "v1",
        false,
        true,
        &[
            ("../escape", b"out"),
            ("x.agh-part", b"part"),
            ("a.bin", b"ay"),
        ],
    );
    let dir = scratch("contract-escape");
    let out = dir.path().join("out");
    for name in ["../escape", "x.agh-part"] {
        let answer = w.host.download(&slug(), "v1", &["a.bin", name], &out);
        assert!(matches!(answer, Err(HostError::Refused(_))), "{answer:?}");
    }
    assert!(!dir.path().join("escape").exists());
    assert!(!out.join("a.bin").exists());
    assert_eq!(w.download_attempts(), 0);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn agh_reads_anonymously_without_a_present_writer() {
    assert!(!AghWorld::new().host.reads_anonymously());
    assert!(AghWorld::with(true, false).host.reads_anonymously());
    assert!(AghWorld::with(false, true).host.reads_anonymously());
}

#[test]
fn agh_absent_writer_is_unreachable_and_names_the_owner() {
    let w = AghWorld::with(false, true);
    match w.host.releases(&slug()) {
        Err(HostError::Unreachable(why)) => assert!(why.contains(OWNER), "{why}"),
        other => panic!("{other:?}"),
    }
    match w.host.release(&slug(), "v1") {
        Err(HostError::Unreachable(why)) => assert!(why.contains(OWNER), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(w.requests(), 0);
}

#[test]
fn agh_one_owner_per_host() {
    // `fixture-ext` has its own writer row and a repository that writer can read: without
    // the owner check, each read below would succeed as `fixture-ext-bot`.
    let w = AghWorld::build(Setup {
        ext: true,
        ..Setup::PLAN
    });
    let other = Slug::new("fixture-ext", REPO);
    let dir = scratch("contract-owner");
    // Every answer names the other slug, which no refusal of the Router's or of M2's does.
    let names_other = |r: Result<(), HostError>| match r {
        Err(HostError::Refused(why)) => assert!(why.contains(EXT_SLUG), "{why}"),
        other => panic!("{other:?}"),
    };
    names_other(w.host.releases(&other).map(drop));
    names_other(w.host.release(&other, "v1").map(drop));
    names_other(w.host.download(&other, "v1", &["a.bin"], dir.path()));
    assert!(!dir.path().join("a.bin").exists());
    let probe = w.host.probe_public(&other, "v1", "a.bin");
    assert!(
        matches!(&probe, Probe::Unknown(why) if why.contains(EXT_SLUG)),
        "{probe:?}"
    );
    // A file an earlier call left at `dest` does not outlive the refusal either.
    let dest = dir.path().join("fetched").join("a.bin");
    std::fs::create_dir_all(dest.parent().expect("a parent")).expect("the fetch dir");
    std::fs::write(&dest, b"an earlier fetch's bytes").expect("the earlier file");
    let fetch = w.host.fetch_public(&other, "v1", "a.bin", &dest, 1024);
    assert!(
        matches!(&fetch, Probe::Unknown(why) if why.contains(EXT_SLUG)),
        "{fetch:?}"
    );
    assert!(!dest.exists());
    // The operations M2 fills in refuse the other owner too, before their own refusal.
    names_other(w.host.create(&other, "v1", "title", "notes", &[]));
    names_other(w.host.upload(&other, "v1", &[], Replace::Never));
    names_other(w.host.publish_draft(&other, "v1"));
    names_other(w.host.clone_repo(&other, &dir.path().join("clone")));
    names_other(w.host.fetch_repo(&other, dir.path()));
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
}

#[test]
fn agh_download_never_compounds_the_lanes_retry() {
    // A client that retries a 500 itself (3 attempts, 2 s then 5 s): each of the lane's
    // attempts is still one asset GET, and agh takes none of its own pauses.
    let w = AghWorld::with_policy(true, true, RetryPolicy::STANDARD);
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    w.fail_downloads();
    let dir = scratch("contract-compound");
    let retried = download_retried(&w.host, &slug(), "v1", &["a.bin"], dir.path());
    assert!(
        matches!(retried, Err(HostError::Unreachable(_))),
        "{retried:?}"
    );
    assert_eq!(w.download_attempts(), DOWNLOAD_ATTEMPTS);
    let once = w.host.download(&slug(), "v1", &["a.bin"], dir.path());
    assert!(matches!(once, Err(HostError::Unreachable(_))), "{once:?}");
    assert_eq!(w.download_attempts(), DOWNLOAD_ATTEMPTS + 1);
    let pauses = RetryPolicy::STANDARD.pauses;
    let slept = w.fx.slept();
    assert!(!slept.iter().any(|d| pauses.contains(d)), "{slept:?}");
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn agh_download_resolution_never_compounds_the_lanes_retry() {
    // The same client, failing the tag's resolution instead: the listing answers 500. Each
    // of the lane's attempts is one listing GET, and agh takes none of its own pauses.
    let w = AghWorld::with_policy(true, true, RetryPolicy::STANDARD);
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    w.fake.fault_always(
        Match::api("/repos/fixture-dev/packs/releases").method("GET"),
        Fault::Status {
            status: 500,
            body: String::new(),
            headers: vec![],
        },
    );
    let dir = scratch("contract-compound-resolve");
    let retried = download_retried(&w.host, &slug(), "v1", &["a.bin"], dir.path());
    assert!(
        matches!(retried, Err(HostError::Unreachable(_))),
        "{retried:?}"
    );
    assert_eq!(w.listing_gets(), DOWNLOAD_ATTEMPTS);
    assert_eq!(w.download_attempts(), 0);
    let pauses = RetryPolicy::STANDARD.pauses;
    let slept = w.fx.slept();
    assert!(!slept.iter().any(|d| pauses.contains(d)), "{slept:?}");
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn agh_unauthenticated_download_is_refused_after_one_attempt() {
    // A 401 is the credential's answer, not the network's: the lane does not try again.
    let w = AghWorld::new();
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    w.fake.fault_always(
        Match::api(ASSETS).method("GET"),
        Fault::Status {
            status: 401,
            body: String::new(),
            headers: vec![],
        },
    );
    let dir = scratch("contract-unauthenticated");
    let retried = download_retried(&w.host, &slug(), "v1", &["a.bin"], dir.path());
    assert!(matches!(retried, Err(HostError::Refused(_))), "{retried:?}");
    assert_eq!(w.download_attempts(), 1);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

/// A 500 whose body's `message` echoes the dev token: agh keeps GitHub's message in `why`
/// as sent, so the host's error map is what keeps the credential out of a lane's log.
fn echoing_the_token() -> Fault {
    Fault::Status {
        status: 500,
        body: format!("{{\"message\":\"echo marker {}\"}}", agh_fake::token("dev")),
        headers: vec![],
    }
}

/// `why` carries the server's message (its marker) and not the credential in it.
fn assert_redacted(why: &str) {
    assert!(why.contains("echo marker"), "{why}");
    assert!(!why.contains(&agh_fake::token("dev")), "{why}");
}

#[test]
fn agh_redacts_the_texts_it_answers() {
    let w = AghWorld::new();
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    w.fake
        .fault_always(Match::api(ASSETS).method("GET"), echoing_the_token());
    let dir = scratch("contract-redacted");
    match w.host.download(&slug(), "v1", &["a.bin"], dir.path()) {
        Err(HostError::Unreachable(why)) => assert_redacted(&why),
        other => panic!("{other:?}"),
    }
    w.fake.fault_always(
        Match::web("/fixture-dev/packs/releases/download/"),
        echoing_the_token(),
    );
    let dest = dir.path().join("fetched").join("a.bin");
    match w.host.fetch_public(&slug(), "v1", "a.bin", &dest, 1024) {
        Probe::Unknown(why) => assert_redacted(&why),
        other => panic!("{other:?}"),
    }
}

#[test]
fn agh_writer_without_push_is_refused() {
    // The writer reads the repository but holds no push: drafts are not visible to it, so a
    // draft-inclusive read is refused (Forbidden), never answered without the drafts.
    let w = AghWorld::build(Setup {
        push: false,
        ..Setup::PLAN
    });
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    let releases = w.host.releases(&slug());
    assert!(
        matches!(releases, Err(HostError::Refused(_))),
        "{releases:?}"
    );
    let release = w.host.release(&slug(), "v1");
    assert!(matches!(release, Err(HostError::Refused(_))), "{release:?}");
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn agh_rate_limited_listing_is_unreachable() {
    // An exhausted rate limit passes: it is never a terminal refusal (design §7.4).
    let w = AghWorld::new();
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    w.fake.fault_always(
        Match::api("/repos/fixture-dev/packs/releases").method("GET"),
        Fault::PrimaryExhausted {
            reset: SIM_START + 3600,
        },
    );
    let releases = w.host.releases(&slug());
    assert!(
        matches!(releases, Err(HostError::Unreachable(_))),
        "{releases:?}"
    );
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn agh_draft_gone_between_listing_and_read_leaves_the_published_release() {
    let w = AghWorld::new();
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    let draft = w.fake.add_release(
        SLUG,
        ReleaseSpec::draft("v1", "2026-10-02T00:00:00Z").prerelease(true),
    );
    // Deleted once the listing (the first GET under the releases path) is answered, so its
    // read by id finds nothing.
    w.fake.after(
        Match::api("/repos/fixture-dev/packs/releases").method("GET"),
        1,
        Change::DeleteRelease { release_id: draft },
    );
    let got = w
        .host
        .release(&slug(), "v1")
        .expect("the published release");
    assert_eq!(normal(got), release("v1", false, true, &["a.bin"]));
    assert!(w.host.warnings().is_empty(), "{:?}", w.host.warnings());
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn agh_download_checks_every_name_before_any_byte_moves() {
    let w = AghWorld::new();
    w.put("v1", false, true, &[("a.bin", b"ay")]);
    let dir = scratch("contract-names-first");
    assert_eq!(
        w.host
            .download(&slug(), "v1", &["a.bin", "missing.bin"], dir.path()),
        Err(HostError::NotFound)
    );
    assert!(!dir.path().join("a.bin").exists());
    assert_eq!(w.download_attempts(), 0);
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn agh_reads_a_two_page_listing() {
    // 150 releases: two pages of 100, so the listing and the tag's resolution both walk past
    // the first page and revalidate it.
    let w = AghWorld::new();
    let tags: Vec<String> = (0..150).map(|n| format!("v{n:03}")).collect();
    for tag in &tags {
        w.put(tag, false, true, &[]);
    }
    let mut listed: Vec<String> = w
        .host
        .releases(&slug())
        .expect("the listing")
        .into_iter()
        .map(|r| r.tag)
        .collect();
    listed.sort();
    assert_eq!(listed, tags);
    for tag in [&tags[0], &tags[149]] {
        let got = w.host.release(&slug(), tag).expect("the release");
        assert_eq!(&got.tag, tag);
    }
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn agh_owner_compare_ignores_ascii_case() {
    let w = AghWorld::new();
    w.put("v1", false, true, &[]);
    let upper = Slug::new("Fixture-Dev", REPO);
    let got = w.host.release(&upper, "v1").expect("the same owner");
    assert_eq!(got.tag, "v1");
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}

#[test]
fn agh_ambiguous_drafts() {
    let w = AghWorld::new();
    w.put("d1", true, true, &[]);
    w.put("d1", true, true, &[]);
    match w.host.release(&slug(), "d1") {
        Err(HostError::Refused(why)) => assert!(why.contains("2 drafts"), "{why}"),
        other => panic!("{other:?}"),
    }
    w.put("d2", true, false, &[("a.bin", b"ay")]);
    let got = w.host.release(&slug(), "d2").expect("the one draft");
    assert_eq!(got, release("d2", true, false, &["a.bin"]));
    assert!(w.host.warnings().is_empty());
    assert_eq!(w.fake.violations(), Vec::<String>::new());
}
