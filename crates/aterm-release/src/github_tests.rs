// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release's GitHub lane against agh-fake's loopback GitHub, through the real `/usr/bin/curl`.

use super::*;
use agh_fake::{AssetSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec, token};
use aterm_update_core::github_rest::Direct;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

const SLUG: &str = "fixture-rel/aterm";
const LOGIN: &str = "fixture-rel-bot";

/// One curl the lane started, as a process listing and its environment would show it, and
/// what it was given on stdin.
#[derive(Clone, Debug)]
struct Spawned {
    program: String,
    args: Vec<String>,
    env: Vec<(String, Option<String>)>,
    stdin: Option<Vec<u8>>,
}

/// A [`Spawn`] that records each child, then (with `env`, to play a hostile environment)
/// starts it as [`Direct`] does.
#[derive(Default)]
struct Recorder {
    spawned: Mutex<Vec<Spawned>>,
    env: Vec<(String, String)>,
}

impl Recorder {
    fn spawned(&self) -> Vec<Spawned> {
        self.spawned.lock().unwrap().clone()
    }
}

impl Spawn for Recorder {
    fn run(
        &self,
        command: &mut aterm_uds::spawnfd::Command,
        stdin: Option<&[u8]>,
    ) -> std::io::Result<std::process::Output> {
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
        for (name, value) in &self.env {
            command.env(name, value);
        }
        Direct.run(command, stdin)
    }
}

/// Where `secret` shows in what a process listing or an environment read would show: each
/// argument and each environment value holding it.
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

/// agh-fake with `LOGIN` holding push on the public `SLUG`, its token, and this thread's
/// GitHub routed to it as that token (or anonymously) through `recorder`.
struct World {
    fake: FakeGithub,
    secret: String,
    recorder: Arc<Recorder>,
    _routed: FixtureGuard,
}

fn world(with_token: bool, recorder: Recorder) -> World {
    let fake = FakeGithub::start();
    let secret = token("release-lane");
    fake.add_token(&secret, LOGIN);
    fake.add_repo(SLUG, RepoSpec::public().push(LOGIN));
    let recorder = Arc::new(recorder);
    let p = fake.ports();
    let routed = Fixture {
        origins: Origins::loopback(p.api, p.uploads, p.web, p.cdn),
        token: with_token.then(|| secret.clone()),
        spawn: Some(Arc::clone(&recorder) as Arc<dyn Spawn>),
    }
    .enter();
    World {
        fake,
        secret,
        recorder,
        _routed: routed,
    }
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::TempDir::new().unwrap()
}

/// The release-create POST for `tag`, sent once.
fn create_draft(tag: &str) -> RunOut {
    let payload =
        aterm_json::to_vec(&aterm_json::json!({"tag_name": tag, "name": tag, "draft": true}))
            .unwrap();
    let path = format!("repos/{SLUG}/releases");
    run_once(prepare(OneShotUrl::Api(&path), Body::Json(&payload)).unwrap()).unwrap()
}

/// The asset upload POST of `file` as `name` onto release `id`, sent once.
fn upload(id: u64, name: &str, file: &std::path::Path) -> RunOut {
    let path = format!("repos/{SLUG}/releases/{id}/assets?name={name}");
    run_once(prepare(OneShotUrl::Uploads(&path), Body::File(file)).unwrap()).unwrap()
}

fn json_of(out: &RunOut) -> Value {
    aterm_json::from_str(std::str::from_utf8(&out.stdout).unwrap()).unwrap()
}

#[test]
fn a_draft_is_created_uploaded_published_listed_and_read_back_through_curl() {
    let w = world(true, Recorder::default());
    let created = create_draft("v0.90.0");
    assert_eq!(
        created.status,
        0,
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let id = json_of(&created)["id"].as_u64().unwrap();
    assert_eq!(w.fake.release_ids(SLUG), [id]);

    let dir = scratch();
    let file = dir.path().join("aterm-appcast.toml");
    let bytes = b"version = \"0.90.0\"\n".repeat(64);
    std::fs::write(&file, &bytes).unwrap();
    let uploaded = upload(id, "aterm-appcast.toml", &file);
    assert_eq!(
        uploaded.status,
        0,
        "{}",
        String::from_utf8_lossy(&uploaded.stderr)
    );
    assert_eq!(
        w.fake.release_state(id).unwrap().1,
        [("aterm-appcast.toml".to_string(), bytes.clone())]
    );
    let again = upload(id, "aterm-appcast.toml", &file);
    assert_eq!(again.status, 22);
    assert_eq!(
        crate::publish::post_outcome(&again),
        crate::publish::PostOutcome::Refused
    );
    assert!(String::from_utf8_lossy(&again.stdout).contains("already_exists"));
    assert_ne!(
        crate::publish::post_outcome(&uploaded),
        crate::publish::PostOutcome::Refused
    );

    send(
        Method::Patch,
        &format!("repos/{SLUG}/releases/{id}"),
        Some(&aterm_json::json!({"draft": false})),
        || Ok(()),
    )
    .unwrap();
    let listed = crate::publish::release_listing(SLUG).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        identity_row(&listed[0]),
        format!("{id}\tv0.90.0\tfalse\tmain")
    );
    let read =
        crate::publish::download_release_asset_for_release_id(SLUG, id, "aterm-appcast.toml")
            .unwrap();
    assert_eq!(read, bytes);
    let cdn: Vec<_> = w
        .fake
        .seen()
        .into_iter()
        .filter(|s| s.origin == FakeOrigin::Cdn)
        .collect();
    assert_eq!(cdn.len(), 1);
    assert!(
        !cdn[0].had_authorization,
        "the CDN hop carries no credential"
    );
    let spawned = w.recorder.spawned();
    assert!(!spawned.is_empty());
    for curl in &spawned {
        assert_eq!(curl.program, "/usr/bin/curl", "never a PATH lookup");
        assert_eq!(
            curl.args.first().map(String::as_str),
            Some("-q"),
            "no curlrc"
        );
    }
    w.fake.assert_clean();
}

#[test]
fn the_token_rides_curls_stdin_and_never_its_argv_or_environment() {
    let w = world(true, Recorder::default());
    assert_eq!(create_draft("v0.90.1").status, 0);
    assert_eq!(
        crate::gates::github_identity_gate(false)
            .unwrap()
            .as_deref(),
        Some(LOGIN),
        "the token reached GitHub"
    );
    let spawned = w.recorder.spawned();
    assert_eq!(spawned.len(), 2, "the create and the identity read");
    for curl in &spawned {
        assert_eq!(exposed(curl, &w.secret), Vec::<String>::new(), "{curl:?}");
        assert!(curl.args.windows(2).any(|a| a == ["-H", "@-"]), "{curl:?}");
        assert_eq!(
            curl.stdin.as_deref(),
            Some(format!("Authorization: Bearer {}\n", w.secret).as_bytes())
        );
        for scrubbed in ["GH_TOKEN", "GITHUB_TOKEN", "CURL_HOME"] {
            assert!(
                curl.env.iter().any(|(k, v)| k == scrubbed && v.is_none()),
                "{scrubbed} is removed: {:?}",
                curl.env
            );
        }
    }
    let in_argv = Spawned {
        args: vec!["-H".into(), format!("Authorization: Bearer {}", w.secret)],
        ..spawned[0].clone()
    };
    assert_eq!(exposed(&in_argv, &w.secret).len(), 1);
    let in_env = Spawned {
        env: vec![("GH_TOKEN".into(), Some(w.secret.clone()))],
        ..spawned[0].clone()
    };
    assert_eq!(exposed(&in_env, &w.secret), ["env: GH_TOKEN"]);
    w.fake.assert_clean();
}

#[test]
fn a_write_without_the_token_is_refused_before_curl_starts() {
    let w = world(false, Recorder::default());
    let path = format!("repos/{SLUG}/releases");
    let Err(refused) = prepare(OneShotUrl::Api(&path), Body::Json(b"{}")) else {
        panic!("a write without the token was prepared");
    };
    assert!(refused.to_string().contains("without a token"), "{refused}");
    let gate = crate::gates::github_identity_gate(false).unwrap_err();
    assert!(gate.to_string().contains("no release token"), "{gate}");
    assert!(w.recorder.spawned().is_empty(), "nothing was started");
    assert!(w.fake.seen().is_empty(), "nothing was sent");
    assert!(crate::publish::release_listing(SLUG).unwrap().is_empty());
    assert_eq!(w.recorder.spawned().len(), 1);
    drop(w);
    let w = world(true, Recorder::default());
    assert!(prepare(OneShotUrl::Api(&path), Body::Json(b"{}")).is_ok());
    drop(w);
}

#[test]
fn the_listing_is_read_whole_and_an_incomplete_page_is_refused() {
    let w = world(true, Recorder::default());
    for n in 0..101u32 {
        w.fake.add_release(
            SLUG,
            ReleaseSpec::published(
                &format!("v0.1.{n}"),
                &format!("2026-10-01T00:{:02}:{:02}Z", n / 60, n % 60),
            ),
        );
    }
    let listed = crate::publish::release_listing(SLUG).unwrap();
    assert_eq!(listed.len(), 101, "both pages");
    let pages = |w: &World| {
        w.fake
            .seen()
            .iter()
            .filter(|s| s.path_and_query.contains("/releases?per_page=100"))
            .count()
    };
    assert_eq!(pages(&w), 2);
    let api = w.fake.ports().api;
    let empty_with_next = Fault::Status {
        status: 200,
        body: "[]".into(),
        headers: vec![(
            "Link".into(),
            format!(
                "<http://127.0.0.1:{api}/repositories/1000/releases?per_page=100&page=3>; \
                 rel=\"next\""
            ),
        )],
    };
    let page_two = format!("/repos/{SLUG}/releases?per_page=100&page=2");
    w.fake.fault(Match::api(&page_two), empty_with_next.clone());
    assert_eq!(crate::publish::release_listing(SLUG).unwrap().len(), 101);
    w.fake.fault_always(Match::api(&page_two), empty_with_next);
    let incomplete = crate::publish::release_listing(SLUG)
        .unwrap_err()
        .to_string();
    assert!(incomplete.contains("after 3 attempts"), "{incomplete}");
    assert!(incomplete.contains("advertises more"), "{incomplete}");
    let before = w.fake.seen().len();
    assert!(crate::publish::release_listing("fixture-rel/none").is_err());
    assert_eq!(w.fake.seen().len(), before + 1);
}

#[test]
fn an_asset_redirect_off_the_cdn_is_refused_and_never_followed() {
    let w = world(true, Recorder::default());
    let id = w.fake.add_release(
        SLUG,
        ReleaseSpec::published("v0.91.0", "2026-10-01T00:00:00Z"),
    );
    let asset = w
        .fake
        .add_asset(id, AssetSpec::uploaded("aterm-appcast.toml", b"v = 1\n"));
    let web = w.fake.ports().web;
    w.fake.fault(
        Match::api(&format!("/repos/{SLUG}/releases/assets/{asset}")),
        Fault::RedirectTo {
            status: 302,
            location: format!("http://127.0.0.1:{web}/elsewhere?sig=1"),
        },
    );
    let refused =
        crate::publish::download_release_asset_for_release_id(SLUG, id, "aterm-appcast.toml")
            .unwrap_err()
            .to_string();
    assert!(refused.contains("off the asset CDN"), "{refused}");
    assert!(
        !refused.contains("sig="),
        "the Location's query is not repeated"
    );
    assert!(
        !w.fake.seen().iter().any(|s| s.origin == FakeOrigin::Web),
        "the off-CDN Location was never asked"
    );
    let read =
        crate::publish::download_release_asset_for_release_id(SLUG, id, "aterm-appcast.toml")
            .unwrap();
    assert_eq!(read, b"v = 1\n");
}

#[test]
fn a_hostile_curlrc_is_never_read() {
    let home = scratch();
    std::fs::write(
        home.path().join(".curlrc"),
        "header = \"X-Planted: from-a-curlrc\"\n",
    )
    .unwrap();
    let dir = home.path().to_str().unwrap().to_string();
    let w = world(
        true,
        Recorder {
            env: vec![("CURL_HOME".into(), dir.clone()), ("HOME".into(), dir)],
            ..Recorder::default()
        },
    );
    assert_eq!(create_draft("v0.92.0").status, 0);
    let planted = |w: &World| {
        w.fake
            .seen()
            .iter()
            .any(|s| s.header("x-planted").is_some())
    };
    assert!(!planted(&w), "-q kept the curlrc out");
    let mut bare = aterm_uds::spawnfd::command("/usr/bin/curl");
    bare.args(["-sS", "-o", "/dev/null", "-H", "User-Agent: control"])
        .arg(format!(
            "http://127.0.0.1:{}/rate_limit",
            w.fake.ports().api
        ))
        .env("CURL_HOME", home.path())
        .env("HOME", home.path());
    assert!(bare.output().unwrap().status.success());
    assert!(planted(&w), "the control reads the curlrc");
}

#[test]
fn rows_are_jqs_tsv() {
    let release = aterm_json::json!({
        "id": 7,
        "tag_name": "v1\tx\\y\nz\r",
        "draft": false,
        "target_commitish": null,
        "assets": [{"name": "a", "id": 9, "size": 3}, {"name": "b", "id": 10, "size": 0}],
    });
    assert_eq!(identity_row(&release), "7\tv1\\tx\\\\y\\nz\\r\tfalse\t");
    assert_eq!(asset_rows(&release), "a\t9\t3\nb\t10\t0\n");
    assert_eq!(asset_rows(&aterm_json::json!({})), "");
    assert_eq!(
        tostring(&aterm_json::json!(true)),
        aterm_json::json!("true")
    );
    assert_eq!(tostring(&aterm_json::json!("s")), aterm_json::json!("s"));
}

/// A [`crate::site::Launch`] that starts each child as it is and counts it. With `trip`, an
/// interrupt (SIGTERM) arrives while the first one runs, after which, as under the release,
/// nothing more starts.
struct Tripwire {
    trip: bool,
    started: Arc<AtomicU32>,
    caught: AtomicI32,
}

impl crate::site::Launch for Tripwire {
    fn interrupted(&self) -> Option<i32> {
        let caught = self.caught.load(Ordering::SeqCst);
        (caught != 0).then_some(caught)
    }

    fn spawn(
        &self,
        cmd: &mut aterm_uds::spawnfd::Command,
    ) -> std::io::Result<(aterm_uds::spawnfd::Child, crate::site::Guard)> {
        if self.interrupted().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "not started: interrupted",
            ));
        }
        let child = cmd.spawn()?;
        self.started.fetch_add(1, Ordering::SeqCst);
        if self.trip {
            self.caught.store(15, Ordering::SeqCst);
        }
        Ok((child, Box::new(())))
    }
}

#[test]
fn a_launched_read_starts_each_curl_through_the_launch_and_none_once_interrupted() {
    let w = world(true, Recorder::default());
    let latest = format!("/repos/{SLUG}/releases/latest");
    w.fake.fault_always(
        Match::api(&latest),
        Fault::Status {
            status: 502,
            body: "{}".into(),
            headers: Vec::new(),
        },
    );
    let asked = |w: &World| {
        w.fake
            .seen()
            .iter()
            .filter(|s| s.path_and_query == latest)
            .count()
    };
    let read = |trip: bool| {
        let started = Arc::new(AtomicU32::new(0));
        let launch = Tripwire {
            trip,
            started: Arc::clone(&started),
            caught: AtomicI32::new(0),
        };
        let spawn: Arc<dyn Spawn> = Arc::new(Launched(launch));
        let got = get_via(Some(&spawn), &format!("repos/{SLUG}/releases/latest"));
        (
            got.map_err(|e| e.to_string()),
            started.load(Ordering::SeqCst),
        )
    };
    let (got, started) = read(true);
    let refused = got.unwrap_err();
    assert!(refused.contains("not started"), "{refused}");
    assert_eq!(started, 1, "the one curl was the launch's");
    assert_eq!(asked(&w), 1, "nothing was asked after the interrupt");
    let (got, started) = read(false);
    let failed = got.unwrap_err();
    assert!(failed.contains("after 3 attempts"), "{failed}");
    assert_eq!(started, 3);
    assert_eq!(asked(&w), 4);
    assert!(
        w.recorder.spawned().is_empty(),
        "the launch, never the client's own spawn, started them"
    );
}

/// The probe mode of [`a_real_sigterm_reaches_a_post_cut_reads_curl_and_nothing_more_is_asked`]:
/// set (to a scratch directory), the test is the release itself (its own process, its own
/// signal handlers).
#[cfg(unix)]
const SIGTERM_PROBE: &str = "ATERM_RELEASE_GITHUB_SIGTERM_PROBE";
/// The probe's lane: `launched` (the release's `StepChildren`) or `direct` (the client's own).
#[cfg(unix)]
const SIGTERM_LANE: &str = "ATERM_RELEASE_GITHUB_SIGTERM_LANE";

/// The probe: the release's handlers installed, one read of a release that agh-fake holds
/// unanswered, `armed` written in `dir` once the fake has the request; then what was asked and
/// how the read ended, on stdout.
#[cfg(unix)]
fn sigterm_probe(dir: &std::path::Path, launched: bool) -> ! {
    use std::time::{Duration, Instant};
    crate::interrupt::install();
    let fake = FakeGithub::start();
    let secret = token("release-lane");
    fake.add_token(&secret, LOGIN);
    fake.add_repo(SLUG, RepoSpec::public().push(LOGIN));
    let latest = format!("/repos/{SLUG}/releases/latest");
    let hang = Duration::from_secs(if launched { 120 } else { 10 });
    fake.fault_always(Match::api(&latest), Fault::Hang(hang));
    let p = fake.ports();
    let _routed = Fixture {
        origins: Origins::loopback(p.api, p.uploads, p.web, p.cdn),
        token: Some(secret),
        spawn: None,
    }
    .enter();
    let spawn: Arc<dyn Spawn> = if launched {
        Arc::new(Launched(crate::release_steps::StepChildren { noted: None }))
    } else {
        Arc::new(Direct)
    };
    let asked = || {
        fake.seen()
            .iter()
            .filter(|s| s.path_and_query == latest)
            .count()
    };
    let got = std::thread::scope(|scope| {
        scope.spawn(|| {
            let bound = Instant::now() + Duration::from_secs(60);
            while Instant::now() < bound {
                if asked() > 0 {
                    std::fs::write(dir.join("armed"), "armed\n").unwrap();
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // The client's own curl takes no signal (the lane's point): once the release has
            // the SIGTERM, the read ends as the hang running out would end it.
            while !launched && Instant::now() < bound {
                if crate::interrupt::caught().is_some() {
                    fake.end_hangs();
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        get_via(Some(&spawn), &format!("repos/{SLUG}/releases/latest"))
    });
    println!("asked={}", asked());
    match got {
        Ok(value) => println!("result=ok {value}"),
        Err(e) => println!("result=err {e}"),
    }
    std::process::exit(0);
}

/// A REAL INTERRUPT OF A POST-CUT READ. The release (this test binary re-run as the probe)
/// reads through the release's child rules (`StepChildren`): a SIGTERM sent to the RELEASE
/// alone is handed to the curl, which leads a session of its own, and nothing more is asked.
#[cfg(unix)]
#[test]
fn a_real_sigterm_reaches_a_post_cut_reads_curl_and_nothing_more_is_asked() {
    use aterm_uds::spawnfd::{Stdio, command};
    use std::time::{Duration, Instant};
    if let Some(dir) = std::env::var_os(SIGTERM_PROBE) {
        let launched = std::env::var(SIGTERM_LANE).is_ok_and(|l| l == "launched");
        sigterm_probe(std::path::Path::new(&dir), launched);
    }
    let run = |lane: &str| -> String {
        let dir = scratch();
        let mut probe = command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "github::tests::a_real_sigterm_reaches_a_post_cut_reads_curl_and_nothing_more_is_asked",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SIGTERM_PROBE, dir.path())
            .env(SIGTERM_LANE, lane)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let bound = Instant::now() + Duration::from_secs(60);
        while !dir.path().join("armed").exists() {
            if Instant::now() > bound || probe.try_wait().unwrap().is_some() {
                let _ = probe.kill();
                let out = probe.wait_with_output().unwrap();
                panic!(
                    "{lane}: the read never reached the fake: {}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let sent = command("kill")
            .args(["-TERM", &probe.id().to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
        let bound = Instant::now() + Duration::from_secs(60);
        while probe.try_wait().unwrap().is_none() {
            if Instant::now() > bound {
                let _ = probe.kill();
                panic!("{lane}: the read did not end after SIGTERM");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let out = probe.wait_with_output().unwrap();
        assert!(out.status.success(), "{lane}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let launched = run("launched");
    assert!(launched.contains("asked=1\n"), "{launched}");
    assert!(
        launched.contains("interrupted by signal 15"),
        "nothing more was asked: {launched}"
    );
    assert!(
        launched.contains("ended by a signal"),
        "the SIGTERM reached the curl: {launched}"
    );
    let direct = run("direct");
    assert!(direct.contains("asked=1\n"), "{direct}");
    assert!(direct.contains("interrupted by signal 15"), "{direct}");
    assert!(!direct.contains("ended by a signal"), "{direct}");
}
