// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! What only the production release host does ([`RestHost`]: GitHub's REST API over the
//! update lanes' `/usr/bin/curl`, agh-fake playing GitHub): a create is a draft prerelease
//! that is uploaded to and then published, still a prerelease, and one killed before its
//! publish leaves a draft; an upload replaces only when asked, its new bytes up before the old
//! asset goes; a download shorter than its listing is refused; a write without a token is
//! refused before anything is sent; the token rides curl's stdin, never its argv or
//! environment; git takes it from one helper scoped to the git origin, whatever helpers and
//! `credential.useHttpPath` the user's config holds, and a `file://` remote takes none. Each
//! guard comes with the negative control that shows it would catch what it guards against.
//!
//! Unix only: the token is written into a private file (`aterm_uds::private_file`).

#![cfg(unix)]

use std::ffi::OsStr;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agh_fake::{AssetSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec};
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

/// One curl the host started, as a process listing and its environment would show it, and
/// what it was given on stdin.
#[derive(Clone, Debug)]
struct Spawned {
    program: String,
    args: Vec<String>,
    env: Vec<(String, Option<String>)>,
    stdin: Option<Vec<u8>>,
}

/// A [`Spawn`] that records each child and starts it as [`Direct`] does. With `plant` (the
/// negative control), it puts that text where a guard must find it, in the child's
/// environment or as a last argument, records the child, and starts nothing.
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

/// agh-fake with `fixture-dev-bot` (push on [`SLUG`]), its token in a private file, and the
/// host's pieces.
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

    /// The host speaking as the token file, its git at `git_base` and hermetic (no system or
    /// global config of this machine's).
    fn host_at(&self, git_base: &str) -> RestHost {
        RestHost::at(
            Some(self.token_file.clone()),
            self.origins.clone(),
            git_base.to_string(),
        )
        .with_git_env(&hermetic_git())
    }

    /// The host speaking as the token file.
    fn host(&self) -> RestHost {
        self.host_at(&self.origins.web)
    }

    /// A file named `name` holding `bytes`, in this rig's scratch.
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

    /// The requests the fake has seen at the uploads origin.
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
    // The writes went in order: the draft, each upload, then the publish.
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
    // A second create of a published tag is the racing publisher's refusal.
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
    // A draft a killed create left is finished by tag.
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
    // Control: the same write with the token goes.
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
            // The CDN hop: anonymous, and named by the API's redirect.
            None => {
                assert!(!s.args.iter().any(|a| a == "@-"), "{s:?}");
                anonymous += 1;
            }
        }
    }
    assert!(credentialed >= 5, "{spawned:#?}");
    assert_eq!(anonymous, 1, "the one download's CDN hop: {spawned:#?}");
    rig.fake.assert_clean();
    // Negative controls: the token in the environment, then on the argv, is caught.
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

/// A loopback HTTP server playing a git host that wants a credential: a request with none is
/// answered 401 with a Basic challenge, one with a credential is recorded and answered 403.
/// It serves until `stop` is set (or a minute, the hang detector).
fn git_server(seen: Arc<Seen>, stop: Arc<AtomicBool>) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().expect("the address").port();
    let thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
            let Ok((stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            stream.set_nonblocking(false).expect("blocking");
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
    let seen = against_git_server(|base| {
        let host = rig.host_at(base);
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
    // Each of the two asked once bare, then once with the token git's helper handed it.
    assert_eq!(
        seen,
        [None, Some(expected.clone()), None, Some(expected.clone())]
    );
    // Control: with no token git has no credential to give, and says so.
    let seen = against_git_server(|base| {
        let host =
            RestHost::at(None, rig.origins.clone(), base.to_string()).with_git_env(&hermetic_git());
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

#[test]
fn git_hands_its_token_over_whatever_helpers_and_use_http_path_the_users_config_holds() {
    let rig = Rig::new();
    let config = rig.dir.path().join("gitconfig");
    let planted = rig.dir.path().join("planted-credentials");
    // The user's global config: every request's path named to helpers, and a helper of its own
    // holding another credential for the very repository.
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
    // Negative controls, the same config: git without the host's reset hands over the user's
    // helper's credential, and the path-less `store` line this helper replaced matches no
    // request git asks about under `useHttpPath`, so git hands over none.
    let git = |base: &str, extra: &[String], dest: &str| {
        let out = aterm_uds::fixture_git::git()
            .args(extra)
            .args(["clone", "--quiet", "--"])
            .arg(format!("{base}/fixture-dev/packs.git"))
            .arg(rig.dir.path().join(dest))
            .env("GIT_CONFIG_GLOBAL", &config)
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
    // The upload dies unanswered: nothing but it was sent, so the old asset is still served.
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
    // The next replace deletes a staged asset an interrupted one left, puts the new bytes up
    // under the staged name, and only then deletes the old asset and renames the new one: the
    // delete the failed run never reached.
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
    // A file named as a stage is never uploaded: it would be taken for one.
    let named = rig.file(&staged, b"x");
    let refused = host.upload(&slug(), "atpkg-ay-1", &[named], Replace::Clobber);
    assert!(matches!(refused, Err(HostError::Refused(_))), "{refused:?}");
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
    // Control: the same read tells a published release from a draft.
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
    // Control: without the fault the same download lands whole.
    host.download(&slug(), "atpkg-ay-1", &["a.bin"], &out)
        .expect("the download");
    assert_eq!(
        std::fs::read(out.join("a.bin")).expect("a.bin"),
        [7_u8; 3000]
    );
}
