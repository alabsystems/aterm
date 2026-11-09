// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration tests for [`RestHost`] against `agh-fake` over `/usr/bin/curl`.

#![cfg(unix)]

use std::ffi::OsStr;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agh_fake::{
    AssetSpec, CommitSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec,
};
use aterm_types::http_retry::Policy;
use aterm_uds::spawnfd::Command;
use aterm_update_core::github_rest::{Direct, Origins, Spawn};
use atpkg_publish::host::rest::{RestHost, STAGED, probe_of};
use atpkg_publish::host::{HostError, Probe, ReleaseHost, Replace, Slug};

use super::scratch;

const SLUG: &str = "fixture-dev/packs";
const LOGIN: &str = "fixture-dev-bot";

fn slug() -> Slug {
    Slug::new("fixture-dev", "packs")
}

/// Recorded `curl` invocation and stdin payload.
#[derive(Clone, Debug)]
struct Spawned {
    program: String,
    args: Vec<String>,
    env: Vec<(String, Option<String>)>,
    stdin: Option<Vec<u8>>,
}

/// [`Spawn`] wrapper that records child invocations or plants a secret for negative controls.
#[derive(Default)]
struct Recorder {
    spawned: Mutex<Vec<Spawned>>,
    plant: Option<(Plant, String)>,
}

#[derive(Clone, Copy)]
enum Plant {
    Env,
    Argv,
}

impl Recorder {
    fn spawned(&self) -> Vec<Spawned> {
        self.spawned.lock().unwrap().clone()
    }
}

impl Spawn for Recorder {
    fn run(
        &self,
        command: &mut Command,
        stdin: Option<&[u8]>,
    ) -> std::io::Result<std::process::Output> {
        match &self.plant {
            Some((Plant::Env, text)) => {
                command.env("ATERM_PLANTED", text);
            }
            Some((Plant::Argv, text)) => {
                command.arg(text);
            }
            None => {}
        }
        let text = |s: &std::ffi::OsStr| s.to_string_lossy().into_owned();
        self.spawned.lock().unwrap().push(Spawned {
            program: text(command.get_program()),
            args: command.get_args().map(text).collect(),
            env: command
                .get_envs()
                .map(|(k, v)| (text(k), v.map(text)))
                .collect(),
            stdin: stdin.map(<[u8]>::to_vec),
        });
        if self.plant.is_some() {
            return Err(std::io::Error::other("a planted child is never started"));
        }
        Direct.run(command, stdin)
    }
}

/// Where `secret` shows in what a process listing or an environment read would show.
fn exposed(spawned: &Spawned, secret: &str) -> Vec<String> {
    let mut found: Vec<String> = spawned
        .args
        .iter()
        .filter(|a| a.contains(secret))
        .map(|a| format!("argv: {a}"))
        .collect();
    found.extend(
        spawned
            .env
            .iter()
            .filter(|(_, v)| v.as_deref().is_some_and(|v| v.contains(secret)))
            .map(|(k, _)| format!("env: {k}")),
    );
    found
}

/// What makes the host's git read no config of this machine's.
fn hermetic_git() -> [(&'static str, &'static OsStr); 3] {
    [
        ("GIT_CONFIG_NOSYSTEM", OsStr::new("1")),
        ("GIT_CONFIG_GLOBAL", OsStr::new("/dev/null")),
        ("XDG_CONFIG_HOME", OsStr::new("/dev/null")),
    ]
}

/// `agh-fake` test rig with `fixture-dev-bot` credentials and scratch storage.
struct Rig {
    fake: FakeGithub,
    secret: String,
    token_file: PathBuf,
    origins: Origins,
    dir: aterm_tempfile::TempDir,
}

impl Rig {
    fn new() -> Rig {
        let fake = FakeGithub::start();
        let dir = scratch("rest-host");
        let tokens = dir.path().join("tokens");
        aterm_uds::private_file::create_private_dir(&tokens).expect("the token directory");
        let secret = agh_fake::token("dev");
        fake.add_token(&secret, LOGIN);
        aterm_uds::private_file::write_private(
            &tokens,
            "dev.token",
            format!("{secret}\n").as_bytes(),
        )
        .expect("the token file");
        fake.add_repo(SLUG, RepoSpec::public().push(LOGIN));
        let ports = fake.ports();
        Rig {
            origins: Origins::loopback(ports.api, ports.uploads, ports.web, ports.cdn),
            fake,
            secret,
            token_file: tokens.join("dev.token"),
            dir,
        }
    }

    /// Authenticated [`RestHost`] pointed at `git_base` with hermetic git config.
    fn host_at(&self, git_base: &str) -> RestHost {
        RestHost::at(
            Some(self.token_file.clone()),
            self.origins.clone(),
            git_base.to_string(),
        )
        .with_git_env(&hermetic_git())
    }

    fn host(&self) -> RestHost {
        self.host_at(&self.origins.web)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let dir = self.dir.path().join("files");
        std::fs::create_dir_all(&dir).expect("the files directory");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("the file");
        path
    }

    /// The release of `tag` on `SLUG`: its spec and its assets' names and bytes.
    fn state(&self, tag: &str) -> (ReleaseSpec, Vec<(String, Vec<u8>)>) {
        let mut found: Vec<_> = self
            .fake
            .release_ids(SLUG)
            .into_iter()
            .filter_map(|id| self.fake.release_state(id))
            .filter(|(spec, _)| spec.tag == tag)
            .collect();
        assert_eq!(found.len(), 1, "one release of {tag}");
        let (spec, mut assets) = found.remove(0);
        assets.sort();
        (spec, assets)
    }

    fn uploads_seen(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|s| s.origin == FakeOrigin::Uploads)
            .count()
    }
}

#[test]
fn the_download_host_status_maps_to_a_probe() {
    assert_eq!(probe_of(302), Probe::Present);
    assert_eq!(probe_of(200), Probe::Present);
    assert_eq!(probe_of(404), Probe::Absent);
    assert!(matches!(probe_of(500), Probe::Unknown(_)));
    assert!(matches!(probe_of(403), Probe::Unknown(_)));
}

#[test]
fn a_create_is_a_draft_uploaded_to_then_published_as_a_prerelease() {
    let rig = Rig::new();
    let host = rig.host();
    let files = [
        rig.file("index.toml", b"the index"),
        rig.file("index.toml.sig", b"its signature"),
    ];
    host.create(&slug(), "atpkg-index-9", "atpkg index 9", "notes", &files)
        .expect("the create");
    let (spec, assets) = rig.state("atpkg-index-9");
    assert!(!spec.draft, "published once every file is up");
    assert!(
        spec.prerelease,
        "and still a prerelease, which never takes latest"
    );
    assert_eq!(spec.name.as_deref(), Some("atpkg index 9"));
    assert_eq!(spec.body.as_deref(), Some("notes"));
    assert_eq!(
        assets,
        [
            ("index.toml".to_string(), b"the index".to_vec()),
            ("index.toml.sig".to_string(), b"its signature".to_vec()),
        ]
    );
    let writes: Vec<String> = rig
        .fake
        .seen()
        .iter()
        .filter(|s| s.method != "GET")
        .map(|s| format!("{:?} {}", s.origin, s.method))
        .collect();
    assert_eq!(
        writes,
        ["Api POST", "Uploads POST", "Uploads POST", "Api PATCH"]
    );
    let again = host.create(&slug(), "atpkg-index-9", "t", "n", &[]);
    assert!(matches!(again, Err(HostError::Refused(_))), "{again:?}");
    assert_eq!(
        host.release(&slug(), "atpkg-index-9")
            .expect("the release")
            .assets
            .len(),
        2
    );
    rig.fake.assert_clean();
}

#[test]
fn an_upload_replaces_only_when_asked_and_a_draft_is_published_by_tag() {
    let rig = Rig::new();
    let host = rig.host();
    let id = rig.fake.add_release(
        SLUG,
        ReleaseSpec::published("atpkg-ay-1", "2026-10-01T00:00:00Z").prerelease(true),
    );
    rig.fake
        .add_asset(id, AssetSpec::uploaded("ay.toml", b"old manifest"));
    let manifest = rig.file("ay.toml", b"new manifest");
    let before = rig.uploads_seen();
    let never = host.upload(
        &slug(),
        "atpkg-ay-1",
        std::slice::from_ref(&manifest),
        Replace::Never,
    );
    assert!(matches!(never, Err(HostError::Refused(_))), "{never:?}");
    assert_eq!(rig.uploads_seen(), before, "a refused name uploads nothing");
    assert_eq!(rig.state("atpkg-ay-1").1[0].1, b"old manifest");
    let sig = rig.file("ay.toml.sig", b"its signature");
    host.upload(&slug(), "atpkg-ay-1", &[manifest, sig], Replace::Clobber)
        .expect("the clobber");
    assert_eq!(
        rig.state("atpkg-ay-1").1,
        [
            ("ay.toml".to_string(), b"new manifest".to_vec()),
            ("ay.toml.sig".to_string(), b"its signature".to_vec()),
        ]
    );
    rig.fake.add_release(
        SLUG,
        ReleaseSpec::draft("atpkg-ay-2", "2026-10-02T00:00:00Z").prerelease(true),
    );
    host.publish_draft(&slug(), "atpkg-ay-2")
        .expect("the publish");
    let (spec, _) = rig.state("atpkg-ay-2");
    assert!(!spec.draft && spec.prerelease, "{spec:?}");
    assert_eq!(
        host.publish_draft(&slug(), "atpkg-ay-9"),
        Err(HostError::NotFound)
    );
    rig.fake.assert_clean();
}

/// `repo_seen` answers `false` only when `/repos/<slug>` is 404 both to the identity and to an
/// anonymous reader. This missing-repository control does not establish nonexistence from
/// those answers; the private-hidden control below has the same result.
#[test]
fn a_repository_is_unseen_only_when_the_identity_and_an_anonymous_reader_both_get_404() {
    let rig = Rig::new();
    let host = rig.host();
    let absent = Slug::new("fixture-pub", "absent");
    assert!(
        matches!(host.release(&absent, "atpkg-x-1"), Err(HostError::Unreachable(m)) if m.contains("not visible")),
        "the upload's own read of a repository nobody created"
    );
    let from = rig.fake.seen().len();
    assert!(matches!(host.repo_seen(&absent), Ok(false)));
    let asked: Vec<(String, bool)> = rig.fake.seen()[from..]
        .iter()
        .map(|s| (s.path_and_query.clone(), s.had_authorization))
        .collect();
    assert_eq!(
        asked,
        [
            ("/repos/fixture-pub/absent".to_string(), true),
            ("/repos/fixture-pub/absent".to_string(), false),
        ],
        "as the identity, then anonymously"
    );

    assert!(
        matches!(host.repo_seen(&slug()), Ok(true)),
        "a public repository"
    );
    rig.fake
        .add_repo("fixture-pub/hidden", RepoSpec::private().push(LOGIN));
    assert!(
        matches!(
            host.repo_seen(&Slug::new("fixture-pub", "hidden")),
            Ok(true)
        ),
        "a private repository the identity can see"
    );
    rig.fake.fault_always(
        Match::api("/repos/fixture-pub/down"),
        Fault::Status {
            status: 500,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    assert!(
        host.repo_seen(&Slug::new("fixture-pub", "down")).is_err(),
        "an owner that does not answer is not an answer"
    );
}

#[test]
fn the_anonymous_repository_fallback_retries_only_transient_answers_with_one_bound() {
    let rig = Rig::new();
    let host = rig.host();
    let path = "/repos/fixture-pub/retry-public";
    rig.fake
        .add_repo("fixture-pub/retry-public", RepoSpec::public());
    for status in [404, 504, 504] {
        rig.fake.fault(
            Match::api(path),
            Fault::Status {
                status,
                body: String::new(),
                headers: Vec::new(),
            },
        );
    }
    let from = rig.fake.seen().len();
    assert_eq!(
        host.repo_seen(&Slug::new("fixture-pub", "retry-public")),
        Ok(true)
    );
    assert_eq!(rig.fake.seen().len() - from, 4, "404, 504, 504, then 200");

    let from = rig.fake.seen().len();
    assert_eq!(
        host.repo_seen(&Slug::new("fixture-pub", "retry-absent")),
        Ok(false)
    );
    assert_eq!(
        rig.fake.seen().len() - from,
        2,
        "the authenticated and anonymous 404 each answer once"
    );

    rig.fake.fault(
        Match::api("/repos/fixture-pub/retry-down"),
        Fault::Status {
            status: 404,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    rig.fake.fault_always(
        Match::api("/repos/fixture-pub/retry-down"),
        Fault::Status {
            status: 503,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    let from = rig.fake.seen().len();
    let refused = host
        .repo_seen(&Slug::new("fixture-pub", "retry-down"))
        .unwrap_err()
        .to_string();
    assert_eq!(
        rig.fake.seen().len() - from,
        1 + usize::try_from(Policy::STANDARD.attempts()).unwrap()
    );
    assert!(refused.contains(&Policy::STANDARD.bound()), "{refused}");
}

#[test]
fn an_already_anonymous_repository_read_uses_one_budget_and_one_404_request() {
    let rig = Rig::new();
    let host = RestHost::at(None, rig.origins.clone(), rig.origins.web.clone());
    let path = "/repos/fixture-pub/anonymous-retry";
    rig.fake
        .add_repo("fixture-pub/anonymous-retry", RepoSpec::public());
    for _ in 0..2 {
        rig.fake.fault(
            Match::api(path),
            Fault::Status {
                status: 504,
                body: String::new(),
                headers: Vec::new(),
            },
        );
    }
    let from = rig.fake.seen().len();
    assert_eq!(
        host.repo_seen(&Slug::new("fixture-pub", "anonymous-retry")),
        Ok(true)
    );
    assert_eq!(rig.fake.seen().len() - from, 3, "504, 504, then 200");

    let from = rig.fake.seen().len();
    assert_eq!(
        host.repo_seen(&Slug::new("fixture-pub", "anonymous-absent")),
        Ok(false)
    );
    assert_eq!(
        rig.fake.seen().len() - from,
        1,
        "one anonymous 404 is final"
    );

    let down = "/repos/fixture-pub/anonymous-down";
    rig.fake.fault_always(
        Match::api(down),
        Fault::Status {
            status: 503,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    let from = rig.fake.seen().len();
    let refused = host
        .repo_seen(&Slug::new("fixture-pub", "anonymous-down"))
        .unwrap_err()
        .to_string();
    assert_eq!(
        rig.fake.seen().len() - from,
        usize::try_from(Policy::STANDARD.attempts()).unwrap()
    );
    assert!(refused.contains(&Policy::STANDARD.bound()), "{refused}");
}

#[test]
fn a_private_repository_hidden_from_both_readers_is_not_seen() {
    let rig = Rig::new();
    let hidden = Slug::new("fixture-pub", "private-hidden");
    rig.fake
        .add_repo("fixture-pub/private-hidden", RepoSpec::private());
    let host = rig.host();
    assert!(
        matches!(
            host.release(&hidden, "atpkg-x-1"),
            Err(HostError::Unreachable(_))
        ),
        "the existing private repository is unreadable to LOGIN"
    );
    let from = rig.fake.seen().len();
    assert_eq!(host.repo_seen(&hidden), Ok(false));
    let asked: Vec<(String, bool)> = rig.fake.seen()[from..]
        .iter()
        .map(|seen| (seen.path_and_query.clone(), seen.had_authorization))
        .collect();
    assert_eq!(
        asked,
        [
            ("/repos/fixture-pub/private-hidden".to_string(), true),
            ("/repos/fixture-pub/private-hidden".to_string(), false),
        ],
        "an existing private repository returns the same two 404s as a missing one"
    );
    rig.fake.assert_clean();
}

#[test]
fn a_write_without_a_token_is_refused_before_anything_is_sent() {
    let rig = Rig::new();
    let empty = rig.file("empty.token", b"\n");
    let file = rig.file("a.bin", b"ay");
    for (host, says) in [
        (
            RestHost::at(None, rig.origins.clone(), rig.origins.web.clone()),
            "never goes anonymously",
        ),
        (
            RestHost::at(Some(empty), rig.origins.clone(), rig.origins.web.clone()),
            "never goes anonymously",
        ),
        (
            RestHost::at(
                Some(rig.file("bad.token", b"two words\n")),
                rig.origins.clone(),
                rig.origins.web.clone(),
            ),
            "not visible ASCII",
        ),
    ] {
        for r in [
            host.create(&slug(), "atpkg-ay-1", "t", "n", std::slice::from_ref(&file)),
            host.upload(
                &slug(),
                "atpkg-ay-1",
                std::slice::from_ref(&file),
                Replace::Never,
            ),
            host.publish_draft(&slug(), "atpkg-ay-1"),
        ] {
            assert!(
                matches!(&r, Err(HostError::Refused(m)) if m.contains(says)),
                "{r:?}"
            );
        }
    }
    assert_eq!(rig.fake.seen().len(), 0, "nothing was sent");
    rig.host()
        .create(&slug(), "atpkg-ay-1", "t", "n", &[file])
        .expect("the create with the token");
    assert!(!rig.fake.seen().is_empty());
}

#[test]
fn the_token_rides_curls_stdin_and_never_its_argv_or_environment() {
    let rig = Rig::new();
    let recorder = Arc::new(Recorder::default());
    let host = rig
        .host()
        .with_spawn(Arc::clone(&recorder) as Arc<dyn Spawn>);
    let file = rig.file("a.bin", &[7_u8; 3000]);
    host.create(&slug(), "atpkg-ay-1", "t", "n", std::slice::from_ref(&file))
        .expect("the create");
    host.upload(&slug(), "atpkg-ay-1", &[file], Replace::Clobber)
        .expect("the upload");
    host.releases(&slug()).expect("the listing");
    let out = rig.dir.path().join("out");
    host.download(&slug(), "atpkg-ay-1", &["a.bin"], &out)
        .expect("the download");
    assert_eq!(
        std::fs::read(out.join("a.bin")).expect("a.bin"),
        [7_u8; 3000]
    );
    let spawned = recorder.spawned();
    let header = format!("Authorization: Bearer {}\n", rig.secret);
    let (mut credentialed, mut anonymous) = (0, 0);
    for s in &spawned {
        assert_eq!(s.program, "/usr/bin/curl");
        assert_eq!(exposed(s, &rig.secret), Vec::<String>::new(), "{s:?}");
        match &s.stdin {
            Some(stdin) => {
                assert_eq!(stdin, header.as_bytes());
                assert!(s.args.windows(2).any(|w| w == ["-H", "@-"]), "{s:?}");
                credentialed += 1;
            }
            None => {
                assert!(!s.args.iter().any(|a| a == "@-"), "{s:?}");
                anonymous += 1;
            }
        }
    }
    assert!(credentialed >= 5, "{spawned:#?}");
    assert_eq!(anonymous, 1, "the one download's CDN hop: {spawned:#?}");
    rig.fake.assert_clean();
    for plant in [Plant::Env, Plant::Argv] {
        let planted = Arc::new(Recorder {
            plant: Some((plant, rig.secret.clone())),
            ..Recorder::default()
        });
        let host = rig
            .host()
            .with_spawn(Arc::clone(&planted) as Arc<dyn Spawn>);
        let _ = host.releases(&slug());
        let spawned = planted.spawned();
        assert!(!spawned.is_empty());
        assert!(
            spawned.iter().all(|s| !exposed(s, &rig.secret).is_empty()),
            "{spawned:#?}"
        );
    }
}

/// What a loopback git server saw: each request's `Authorization`, if it carried one.
#[derive(Default)]
struct Seen {
    authorizations: Mutex<Vec<Option<String>>>,
}

/// Loopback HTTP server returning 401 when unauthenticated and 403 when authenticated.
fn git_server(seen: Arc<Seen>, stop: Arc<AtomicBool>) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("the address").port();
    let thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
            let Ok((stream, _)) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Blocking,
            ) else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .expect("a read timeout");
            let mut reader = BufReader::new(stream.try_clone().expect("the stream"));
            let mut authorization = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("authorization")
                {
                    authorization = Some(value.trim().to_string());
                }
            }
            let reply = if authorization.is_some() {
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"fixture\"\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            };
            seen.authorizations.lock().unwrap().push(authorization);
            let mut stream = stream;
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (port, thread)
}

/// `run` against a fresh [`git_server`]: what it saw.
fn against_git_server(run: impl FnOnce(&str)) -> Vec<Option<String>> {
    let (seen, stop) = (Arc::new(Seen::default()), Arc::new(AtomicBool::new(false)));
    let (port, thread) = git_server(Arc::clone(&seen), Arc::clone(&stop));
    run(&format!("http://127.0.0.1:{port}"));
    stop.store(true, Ordering::SeqCst);
    thread.join().expect("the server thread");
    seen.authorizations.lock().unwrap().clone()
}

/// `x-access-token` and `secret` as git's Basic credential.
fn basic(user: &str, secret: &str) -> String {
    format!(
        "Basic {}",
        aterm_codec::base64::encode(format!("{user}:{secret}").as_bytes()).expect("base64")
    )
}

#[test]
fn git_takes_the_token_through_its_scoped_helper_and_only_with_one() {
    let rig = Rig::new();
    let expected = basic("x-access-token", &rig.secret);
    let dest = rig.dir.path().join("clone");
    let mut git_env = hermetic_git().to_vec();
    git_env.push(("GIT_ALLOW_PROTOCOL", OsStr::new("http")));
    let seen = against_git_server(|base| {
        let host = rig.host_at(base).with_git_env(&git_env);
        let clone = host.clone_repo(&slug(), &dest);
        assert!(matches!(clone, Err(HostError::Unreachable(_))), "{clone:?}");
        std::fs::create_dir_all(&dest).expect("a directory to fetch into");
        aterm_uds::fixture_git::git()
            .args(["init", "--quiet"])
            .current_dir(&dest)
            .status()
            .expect("git init");
        let fetch = host.fetch_repo(&slug(), &dest);
        assert!(matches!(fetch, Err(HostError::Unreachable(_))), "{fetch:?}");
    });
    assert_eq!(
        seen,
        [None, Some(expected.clone()), None, Some(expected.clone())]
    );
    let seen = against_git_server(|base| {
        let host = RestHost::at(None, rig.origins.clone(), base.to_string()).with_git_env(&git_env);
        let clone = host.clone_repo(&slug(), &rig.dir.path().join("anonymous"));
        assert!(matches!(clone, Err(HostError::Unreachable(_))), "{clone:?}");
    });
    assert_eq!(seen, [None]);
}

#[test]
fn a_file_remote_clones_and_fetches_with_no_credential_asked() {
    let rig = Rig::new();
    let remotes = rig.dir.path().join("remotes");
    let bare = remotes.join("fixture-dev").join("packs.git");
    std::fs::create_dir_all(&bare).expect("the remote directory");
    let git = |dir: &Path, args: &[&str]| {
        let out = aterm_uds::fixture_git::git()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(
        &bare,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    let work = rig.dir.path().join("work");
    std::fs::create_dir_all(&work).expect("the work tree");
    git(&work, &["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(work.join("README"), "one\n").expect("a file");
    git(&work, &["add", "README"]);
    git(
        &work,
        &["-c", "user.name=Fixture", "commit", "--quiet", "-m", "one"],
    );
    git(
        &work,
        &["push", "--quiet", bare.to_str().expect("utf-8"), "main"],
    );
    let host = rig.host_at(&format!("file://{}", remotes.display()));
    let dest = rig.dir.path().join("clone");
    host.clone_repo(&slug(), &dest).expect("the clone");
    assert_eq!(
        std::fs::read_to_string(dest.join("README")).expect("README"),
        "one\n"
    );
    std::fs::write(work.join("README"), "two\n").expect("a change");
    git(
        &work,
        &["-c", "user.name=Fixture", "commit", "--quiet", "-am", "two"],
    );
    git(
        &work,
        &["push", "--quiet", bare.to_str().expect("utf-8"), "main"],
    );
    host.fetch_repo(&slug(), &dest).expect("the fetch");
    let out = aterm_uds::fixture_git::git()
        .args(["log", "--format=%s", "-1", "origin/main"])
        .current_dir(&dest)
        .output()
        .expect("git log");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "two");
    assert_eq!(rig.fake.seen().len(), 0, "git never asked the API");
}

fn main_record_git(dir: &Path, args: &[&str]) -> String {
    let out = aterm_uds::fixture_git::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout)
        .expect("git's text")
        .trim()
        .into()
}

/// A real Git mirror whose initial main agrees with the independently served API. The
/// checkout's URL rewrite routes the host's HTTPS URL to this mirror without contacting GitHub.
fn main_record_fixture(rig: &Rig) -> (RestHost, PathBuf, String) {
    let remotes = rig.dir.path().join("mirror");
    let bare = remotes.join("fixture-dev/packs.git");
    std::fs::create_dir_all(&bare).expect("the mirror directory");
    main_record_git(
        &bare,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    let checkout = rig.dir.path().join("checkout");
    std::fs::create_dir(&checkout).expect("the checkout");
    main_record_git(&checkout, &["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(checkout.join("README"), "API main\n").expect("the source");
    main_record_git(&checkout, &["add", "README"]);
    main_record_git(
        &checkout,
        &[
            "-c",
            "user.name=Fixture",
            "commit",
            "--quiet",
            "-m",
            "API main",
        ],
    );
    main_record_git(
        &checkout,
        &["remote", "add", "origin", bare.to_str().expect("utf-8")],
    );
    main_record_git(&checkout, &["push", "--quiet", "-u", "origin", "main"]);
    let sha = main_record_git(&checkout, &["rev-parse", "HEAD"]);
    rig.fake.add_commit(
        SLUG,
        CommitSpec {
            sha: sha.clone(),
            message: "API main".into(),
            author_login: Some(LOGIN.into()),
            author_name: "Fixture".into(),
            date: "2026-10-31T00:00:00Z".into(),
            paths: vec!["README".into()],
        },
    );
    rig.fake.set_branch(SLUG, "refs/heads/main", &sha);
    main_record_git(
        &checkout,
        &[
            "config",
            &format!("url.file://{}/.insteadOf", remotes.display()),
            "https://github.com/",
        ],
    );
    (rig.host_at("https://github.com"), checkout, sha)
}

#[test]
fn a_main_of_record_is_bound_to_the_authenticated_hosts_commit() {
    use atpkg_publish::artifact::package::MAIN_OF_RECORD;

    let rig = Rig::new();
    let (host, checkout, api_main) = main_record_fixture(&rig);
    host.fetch_main(&slug(), &checkout, MAIN_OF_RECORD)
        .expect("the rewritten mirror transports the API's exact main");
    assert_eq!(
        main_record_git(&checkout, &["rev-parse", MAIN_OF_RECORD]),
        api_main
    );
    main_record_git(
        &checkout,
        &["update-ref", "refs/atpkg-publish/unrelated", &api_main],
    );

    std::fs::write(checkout.join("README"), "mirror only\n").expect("the mirror-only source");
    main_record_git(
        &checkout,
        &[
            "-c",
            "user.name=Fixture",
            "commit",
            "--quiet",
            "-am",
            "mirror only",
        ],
    );
    main_record_git(&checkout, &["push", "--quiet", "origin", "main"]);
    let mirror_main = main_record_git(&checkout, &["rev-parse", "HEAD"]);
    assert_ne!(mirror_main, api_main);
    let tracking = main_record_git(&checkout, &["rev-parse", "refs/remotes/origin/main"]);
    let err = host
        .fetch_main(&slug(), &checkout, MAIN_OF_RECORD)
        .expect_err("a fresh fetch of a rewritten mirror cannot define the host's main");
    assert!(matches!(err, HostError::Unreachable(_)), "{err:?}");
    assert!(
        err.to_string().contains(&mirror_main) && err.to_string().contains(&api_main),
        "{err}"
    );
    assert_eq!(
        main_record_git(
            &checkout,
            &["for-each-ref", "--format=%(refname)", "refs/atpkg-publish/"]
        ),
        "refs/atpkg-publish/unrelated",
        "only the failed attempt's exact record ref is removed"
    );
    assert_eq!(
        main_record_git(&checkout, &["rev-parse", "refs/remotes/origin/main"]),
        tracking,
        "the checkout's remote-tracking ref is untouched"
    );
    let seen = rig.fake.seen();
    assert_eq!(seen.len(), 2, "one API read per attempt, without retries");
    assert!(seen.iter().all(|request| {
        request.method == "GET"
            && request.path_and_query == format!("/repos/{SLUG}/commits/refs/heads/main")
            && request.as_login.as_deref() == Some(LOGIN)
            && request.had_authorization
    }));
}

#[test]
fn an_unreadable_main_of_record_never_starts_its_git_fetch() {
    use atpkg_publish::artifact::package::MAIN_OF_RECORD;

    let rig = Rig::new();
    let (host, checkout, api_main) = main_record_fixture(&rig);
    main_record_git(&checkout, &["update-ref", MAIN_OF_RECORD, &api_main]);
    std::fs::write(checkout.join("README"), "mirror only\n").expect("the mirror-only source");
    main_record_git(
        &checkout,
        &[
            "-c",
            "user.name=Fixture",
            "commit",
            "--quiet",
            "-am",
            "mirror only",
        ],
    );
    main_record_git(&checkout, &["push", "--quiet", "origin", "main"]);
    let api_path = format!("/repos/{SLUG}/commits/refs/heads/main");
    let bodies = [
        "{}".to_string(),
        "{\"sha\":null}".to_string(),
        "{\"sha\":42}".to_string(),
        "{\"sha\":\"abc\"}".to_string(),
        format!("{{\"sha\":\"{}\"}}", "A".repeat(40)),
        format!("{{\"sha\":\"{}\"}}", "g".repeat(40)),
        format!("{{\"sha\":\"{}\"}}", "a".repeat(41)),
        format!("{{\"sha\":\" {api_main}\"}}"),
    ];
    let attempts = bodies.len() + 1;
    for fault in bodies
        .into_iter()
        .map(|body| Fault::Status {
            status: 200,
            body,
            headers: Vec::new(),
        })
        .chain([Fault::Close])
    {
        rig.fake.fault(Match::api(&api_path), fault);
        let err = host
            .fetch_main(&slug(), &checkout, MAIN_OF_RECORD)
            .expect_err("an unreadable API main is no authority");
        assert!(matches!(err, HostError::Unreachable(_)), "{err:?}");
        assert!(err.to_string().contains("host API"), "{err}");
        assert_eq!(
            main_record_git(&checkout, &["rev-parse", MAIN_OF_RECORD]),
            api_main,
            "the API refusal happens before fetching or cleaning any record ref"
        );
    }
    assert_eq!(rig.fake.seen().len(), attempts, "no API retry");
}

#[test]
fn git_hands_its_token_over_whatever_helpers_and_use_http_path_the_users_config_holds() {
    let rig = Rig::new();
    let config = rig.dir.path().join("gitconfig");
    let planted = rig.dir.path().join("planted-credentials");
    let user_config = |base: &str| {
        std::fs::write(
            &planted,
            format!(
                "{}://intruder:planted@{}/fixture-dev/packs.git\n",
                base.split_once("://").expect("a scheme").0,
                base.split_once("://").expect("a scheme").1
            ),
        )
        .expect("the planted credential");
        std::fs::write(
            &config,
            format!(
                "[credential]\n\tuseHttpPath = true\n\thelper = store --file={}\n",
                planted.display()
            ),
        )
        .expect("the user's config");
    };
    let env = [
        ("GIT_CONFIG_NOSYSTEM", OsStr::new("1")),
        ("GIT_CONFIG_GLOBAL", config.as_os_str()),
        ("XDG_CONFIG_HOME", OsStr::new("/dev/null")),
        ("GIT_ALLOW_PROTOCOL", OsStr::new("http")),
    ];
    let seen = against_git_server(|base| {
        user_config(base);
        let host = RestHost::at(
            Some(rig.token_file.clone()),
            rig.origins.clone(),
            base.to_string(),
        )
        .with_git_env(&env);
        let clone = host.clone_repo(&slug(), &rig.dir.path().join("clone"));
        assert!(matches!(clone, Err(HostError::Unreachable(_))), "{clone:?}");
    });
    assert_eq!(seen, [None, Some(basic("x-access-token", &rig.secret))]);
    let git = |base: &str, extra: &[String], dest: &str| {
        let out = aterm_uds::fixture_git::git()
            .args(extra)
            .args(["clone", "--quiet", "--"])
            .arg(format!("{base}/fixture-dev/packs.git"))
            .arg(rig.dir.path().join(dest))
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("GIT_ALLOW_PROTOCOL", "http")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .output()
            .expect("git");
        assert!(!out.status.success(), "{out:?}");
    };
    let seen = against_git_server(|base| {
        user_config(base);
        git(base, &[], "unreset");
    });
    assert_eq!(seen, [None, Some(basic("intruder", "planted"))]);
    let store = rig.dir.path().join("store");
    let seen = against_git_server(|base| {
        user_config(base);
        let (scheme, host) = base.split_once("://").expect("a scheme");
        std::fs::write(
            &store,
            format!("{scheme}://x-access-token:{}@{host}\n", rig.secret),
        )
        .expect("the store line");
        let helper = format!("credential.helper=store --file={}", store.display());
        git(
            base,
            &[
                "-c".into(),
                "credential.helper=".into(),
                "-c".into(),
                helper,
            ],
            "pathless",
        );
    });
    assert_eq!(seen, [None]);
}

/// The writes `rig`'s fake saw from `from` on: each one's origin, method and path.
fn writes_since(rig: &Rig, from: usize) -> Vec<String> {
    rig.fake.seen()[from..]
        .iter()
        .filter(|s| s.method != "GET")
        .map(|s| format!("{:?} {} {}", s.origin, s.method, s.path_and_query))
        .collect()
}

#[test]
fn a_clobber_puts_the_new_bytes_up_before_the_old_asset_goes() {
    let rig = Rig::new();
    let host = rig.host();
    let id = rig.fake.add_release(
        SLUG,
        ReleaseSpec::published("atpkg-ay-1", "2026-10-01T00:00:00Z").prerelease(true),
    );
    let old = rig
        .fake
        .add_asset(id, AssetSpec::uploaded("ay.toml", b"old manifest"));
    let manifest = rig.file("ay.toml", b"new manifest");
    let upload = || {
        host.upload(
            &slug(),
            "atpkg-ay-1",
            std::slice::from_ref(&manifest),
            Replace::Clobber,
        )
    };
    rig.fake.fault(
        Match::uploads(&format!("/repos/{SLUG}/releases/{id}/assets")),
        Fault::Close,
    );
    let before = rig.fake.seen().len();
    let failed = upload();
    assert!(
        matches!(failed, Err(HostError::Unreachable(_))),
        "{failed:?}"
    );
    let staged = format!("ay.toml{STAGED}");
    assert_eq!(
        writes_since(&rig, before),
        [format!(
            "Uploads POST /repos/{SLUG}/releases/{id}/assets?name={staged}"
        )]
    );
    assert_eq!(
        rig.state("atpkg-ay-1").1,
        [("ay.toml".to_string(), b"old manifest".to_vec())]
    );
    let stale = rig
        .fake
        .add_asset(id, AssetSpec::uploaded(&staged, b"a stale stage"));
    let before = rig.fake.seen().len();
    upload().expect("the clobber");
    let writes = writes_since(&rig, before);
    let new = writes
        .iter()
        .find_map(|w| w.strip_prefix("Api PATCH "))
        .and_then(|p| p.rsplit('/').next())
        .expect("the rename")
        .to_string();
    assert_eq!(
        writes,
        [
            format!("Api DELETE /repos/{SLUG}/releases/assets/{stale}"),
            format!("Uploads POST /repos/{SLUG}/releases/{id}/assets?name={staged}"),
            format!("Api DELETE /repos/{SLUG}/releases/assets/{old}"),
            format!("Api PATCH /repos/{SLUG}/releases/assets/{new}"),
        ]
    );
    assert_eq!(
        rig.state("atpkg-ay-1").1,
        [("ay.toml".to_string(), b"new manifest".to_vec())]
    );
    let named = rig.file(&staged, b"x");
    let refused = host.upload(&slug(), "atpkg-ay-1", &[named], Replace::Clobber);
    assert!(matches!(refused, Err(HostError::Refused(_))), "{refused:?}");
    rig.fake.assert_clean();
}

/// During the swap both hosts write the same staging releases, so a clobber drops the other
/// host's leftover too: a cut agh clobber's `{name}.agh-{8 hex}` beside a cut replace's own
/// `{name}.atpkg-staged`, both when the release lacks the asset and when it holds an older
/// one. The release ends holding the file alone, so the mirror finds no extra asset.
/// `AghHost`'s half is `channel::agh_write_contract`'s
/// `a_clobber_drops_either_host_s_leftover_temporary`.
#[test]
fn a_clobber_drops_either_host_s_leftover_temporary() {
    let rig = Rig::new();
    let host = rig.host();
    let id = rig.fake.add_release(
        SLUG,
        ReleaseSpec::published("atpkg-ay-1", "2026-10-01T00:00:00Z").prerelease(true),
    );
    let manifest = rig.file("ay.toml", b"new manifest");
    let leftovers = |rig: &Rig| {
        rig.fake.add_asset(
            id,
            AssetSpec::uploaded("ay.toml.agh-00000000", b"an agh temporary"),
        );
        rig.fake.add_asset(
            id,
            AssetSpec::uploaded(&format!("ay.toml{STAGED}"), b"a stale stage"),
        );
    };
    leftovers(&rig);
    host.upload(
        &slug(),
        "atpkg-ay-1",
        std::slice::from_ref(&manifest),
        Replace::Clobber,
    )
    .expect("the clobber of an asset the release lacks");
    assert_eq!(
        rig.state("atpkg-ay-1").1,
        [("ay.toml".to_string(), b"new manifest".to_vec())]
    );
    leftovers(&rig);
    let newer = rig.file("ay.toml", b"newer manifest");
    host.upload(&slug(), "atpkg-ay-1", &[newer], Replace::Clobber)
        .expect("the clobber of an asset the release holds");
    assert_eq!(
        rig.state("atpkg-ay-1").1,
        [("ay.toml".to_string(), b"newer manifest".to_vec())]
    );
    rig.fake.assert_clean();
}

#[test]
fn a_create_killed_before_its_publish_leaves_a_draft_no_client_sees() {
    let rig = Rig::new();
    let host = rig.host();
    let files = [rig.file("index.toml", b"the index")];
    rig.fake.fault(
        Match::uploads(&format!("/repos/{SLUG}/releases/")),
        Fault::Close,
    );
    let failed = host.create(&slug(), "atpkg-index-9", "atpkg index 9", "notes", &files);
    assert!(
        matches!(failed, Err(HostError::Unreachable(_))),
        "{failed:?}"
    );
    let (spec, assets) = rig.state("atpkg-index-9");
    assert!(spec.draft, "created a draft: {spec:?}");
    assert!(spec.prerelease, "{spec:?}");
    assert!(assets.is_empty());
    host.create(&slug(), "atpkg-index-10", "atpkg index 10", "notes", &files)
        .expect("the create");
    assert!(!rig.state("atpkg-index-10").0.draft);
}

#[test]
fn a_download_shorter_than_its_listing_is_refused_and_leaves_no_file() {
    let rig = Rig::new();
    let host = rig.host();
    let id = rig.fake.add_release(
        SLUG,
        ReleaseSpec::published("atpkg-ay-1", "2026-10-01T00:00:00Z").prerelease(true),
    );
    rig.fake
        .add_asset(id, AssetSpec::uploaded("a.bin", &[7_u8; 3000]));
    rig.fake.fault(
        Match::cdn(),
        Fault::Status {
            status: 200,
            body: "short".into(),
            headers: vec![("Content-Type".into(), "application/octet-stream".into())],
        },
    );
    let out = rig.dir.path().join("out");
    let short = host.download(&slug(), "atpkg-ay-1", &["a.bin"], &out);
    assert!(
        matches!(&short, Err(HostError::Unreachable(m)) if m.contains("lists 3000 bytes")),
        "{short:?}"
    );
    assert!(!out.join("a.bin").exists());
    assert!(!out.join("a.bin.atpkg-part").exists());

    std::fs::create_dir_all(out.join("a.bin")).expect("blocking directory");
    let blocked = host.download(&slug(), "atpkg-ay-1", &["a.bin"], &out);
    assert!(
        matches!(&blocked, Err(HostError::Refused(_))),
        "{blocked:?}"
    );
    assert!(!out.join("a.bin.atpkg-part").exists());
    std::fs::remove_dir(out.join("a.bin")).expect("remove blocking directory");

    host.download(&slug(), "atpkg-ay-1", &["a.bin"], &out)
        .expect("the download");
    assert_eq!(
        std::fs::read(out.join("a.bin")).expect("a.bin"),
        [7_u8; 3000]
    );
}

/// A PUBLIC READ RIDES THE DOWNLOAD HOST'S OUTAGE: with the web route answering 504, the asset is
/// read through the API route, whose requests alone carry the identity's token; the web and CDN
/// legs stay anonymous.
/// NEGATIVE CONTROL: an asset the release lacks is absent on the web route's 404, and the API is
/// never asked; a partial left by an earlier run is not continued; a read over its cap leaves
/// neither the file nor a partial.
#[test]
fn a_public_read_takes_the_api_route_when_the_web_route_fails_and_a_404_is_absent() {
    let rig = Rig::new();
    let host = rig.host();
    let id = rig.fake.add_release(
        SLUG,
        ReleaseSpec::published("atpkg-ay-1", "2026-10-01T00:00:00Z").prerelease(true),
    );
    rig.fake
        .add_asset(id, AssetSpec::uploaded("ay.toml", b"the manifest"));
    let dest = rig.dir.path().join("public").join("ay.toml");

    assert_eq!(
        host.fetch_public(&slug(), "atpkg-ay-1", "missing.toml", &dest, 64),
        Probe::Absent
    );
    assert!(
        rig.fake.seen().iter().all(|s| s.origin != FakeOrigin::Api),
        "a 404 asks the API nothing"
    );

    rig.fake.fault_always(
        Match::web("/fixture-dev/packs/releases/"),
        Fault::Status {
            status: 504,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    rig.fake.clear_seen();
    // A partial an earlier run left (of bytes since replaced) is not continued.
    let part = rig.dir.path().join("public").join("ay.toml.part");
    std::fs::write(&part, b"XX").expect("a stale partial");
    assert_eq!(
        host.fetch_public(&slug(), "atpkg-ay-1", "ay.toml", &dest, 64),
        Probe::Present
    );
    assert_eq!(
        std::fs::read(&dest).expect("the fetched file"),
        b"the manifest"
    );
    let seen = rig.fake.seen();
    assert!(seen.iter().any(|s| s.origin == FakeOrigin::Api), "{seen:?}");
    assert!(
        seen.iter()
            .all(|s| s.had_authorization == (s.origin == FakeOrigin::Api)),
        "only the API leg carries the token: {seen:?}"
    );

    std::fs::remove_file(&dest).expect("remove the fetched file");
    let over = host.fetch_public(&slug(), "atpkg-ay-1", "ay.toml", &dest, 4);
    assert!(matches!(over, Probe::Unknown(_)), "{over:?}");
    assert!(!dest.exists());
    assert!(!part.exists());
}
