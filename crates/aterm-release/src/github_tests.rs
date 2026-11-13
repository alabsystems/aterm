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

/// One curl the lane started.
#[derive(Clone, Debug)]
struct Spawned {
    program: String,
    args: Vec<String>,
    env: Vec<(String, Option<String>)>,
    stdin: Option<Vec<u8>>,
}

/// A [`Spawn`] that records each child.
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

/// A client fixture for retry-loop tests whose injected operation never reaches the network.
fn retry_fixture() -> FixtureGuard {
    Fixture {
        origins: Origins::loopback(1, 1, 1, 1),
        token: None,
        spawn: None,
    }
    .enter()
}

fn status_error(status: u16, retry_after_secs: Option<u64>) -> RestError {
    RestError::Status {
        status,
        retry_after_secs,
        message: format!("fixture HTTP {status}"),
    }
}

fn curl_error(exit: i32, status: Option<u16>) -> RestError {
    RestError::Transport {
        exit: Some(exit),
        status,
        retry_after_secs: None,
        message: format!("fixture curl exit {exit}"),
    }
}

fn create_draft(tag: &str) -> RunOut {
    let payload =
        aterm_json::to_vec(&aterm_json::json!({"tag_name": tag, "name": tag, "draft": true}))
            .unwrap();
    let path = format!("repos/{SLUG}/releases");
    run_once(prepare(OneShotUrl::Api(&path), Body::Json(&payload)).unwrap()).unwrap()
}

fn upload(id: u64, name: &str, file: &std::path::Path) -> RunOut {
    let path = format!("repos/{SLUG}/releases/{id}/assets?name={name}");
    run_once(prepare(OneShotUrl::Uploads(&path), Body::File(file)).unwrap()).unwrap()
}

/// The engine's loopback GitHub seam with fixed per-upload service delays.
/// This measures scheduling and real curl/HTTP/byte-verification overhead,
/// never public GitHub throughput. No release credential is reachable.
#[test]
#[ignore = "explicit loopback upload wall measurement"]
fn concurrent_upload_fixture_walls() {
    const ASSETS: [(&str, u64); 13] = [
        ("aterm-0.181.0-mac.zip", 300),
        ("aterm-0.181.0-mac.zip.sha256", 45),
        ("aterm-0.181.0.dmg", 369),
        ("aterm-0.181.0.dmg.sha256", 52),
        ("aterm-mac.zip", 299),
        ("aterm-mac.zip.sha256", 43),
        ("aterm-machines.toml", 15),
        ("aterm-machines.toml.sig", 15),
        ("aterm-service-qualification.json", 43),
        ("aterm.dmg", 258),
        ("aterm.dmg.sha256", 44),
        ("aterm-appcast.toml.sig", 42),
        ("aterm-appcast.toml", 46),
    ];
    struct Delayed;
    impl Spawn for Delayed {
        fn run(
            &self,
            command: &mut Command,
            stdin: Option<&[u8]>,
        ) -> std::io::Result<std::process::Output> {
            if let Some(millis) = command.get_args().find_map(|arg| {
                let arg = arg.to_str()?;
                let (_, name) = arg.split_once("/assets?name=")?;
                ASSETS
                    .iter()
                    .find(|(asset, _)| *asset == name)
                    .map(|(_, millis)| *millis)
            }) {
                std::thread::sleep(std::time::Duration::from_millis(millis));
            }
            Direct.run(command, stdin)
        }
    }
    for bound in [1, 2, 4, 6, 6, 4, 2, 1] {
        let w = world(true, Recorder::default());
        let p = w.fake.ports();
        let _route = Fixture {
            origins: Origins::loopback(p.api, p.uploads, p.web, p.cdn),
            token: Some(w.secret.clone()),
            spawn: Some(Arc::new(Delayed)),
        }
        .enter();
        let created = create_draft("v0.181.0");
        assert!(created.success());
        let id = json_of(&created)["id"].as_u64().unwrap();
        let dir = scratch();
        for (name, _) in ASSETS {
            std::fs::write(dir.path().join(name), name.as_bytes()).unwrap();
        }
        let mut cut = crate::publish::CutCtx {
            credentials: None,
            apple: crate::sign::AppleTier::Inactive,
            repo: dir.path().to_owned(),
            tree: dir.path().to_owned(),
            dist: dir.path().to_owned(),
            journal_path: dir.path().join("cut-state.toml"),
            slug: SLUG.into(),
            version: "0.181.0".into(),
            tag: "v0.181.0".into(),
            build: 1810,
            commit: "a".repeat(40),
            min_build: None,
            artifact: None,
            macho_sha256: None,
            linux: None,
            manifest_signed: true,
            signature_required: true,
            signature_pubkey: None,
            verify_pubkey: None,
            signature_machine_id: None,
            attribution: None,
            roster: None,
            release_id: Some(id),
            release_intent: true,
            upload_intents: Vec::new(),
            kind: crate::publish::CutKind::DryRun,
            lease: None,
            fence: None,
            journal: None,
            adopted: false,
        };
        let ctx = Mutex::new(&mut cut);
        let put = |(name, _): &(&str, u64)| {
            crate::publish::upload_channel_asset(&ctx, SLUG, id, &dir.path().join(name), true)
        };
        let started = std::time::Instant::now();
        crate::publish::upload_pool(&ASSETS[..11], bound, &put).unwrap();
        for asset in &ASSETS[11..] {
            put(asset).unwrap();
        }
        println!(
            "upload-http-fixture bound={bound} wall_s={:.6}",
            started.elapsed().as_secs_f64()
        );
        assert_eq!(w.fake.release_state(id).unwrap().1.len(), ASSETS.len());
        assert_eq!(ctx.lock().unwrap().upload_intents.len(), ASSETS.len());
        // Resume converges byte-identical objects without minting another POST.
        crate::publish::upload_pool(&ASSETS, bound, &put).unwrap();
        assert_eq!(ctx.lock().unwrap().upload_intents.len(), ASSETS.len());
    }
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
    assert!(
        gate.to_string().contains("aterm secrets need gh:release"),
        "{gate}"
    );
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
    assert!(
        incomplete.contains(&AUTHENTICATED_RETRY_POLICY.bound()),
        "{incomplete}"
    );
    assert!(incomplete.contains("advertises more"), "{incomplete}");
    let before = w.fake.seen().len();
    assert!(crate::publish::release_listing("fixture-rel/none").is_err());
    assert_eq!(w.fake.seen().len(), before + 1);
}

#[test]
fn two_504s_then_a_200_survive_under_the_shared_retry_policy() {
    let _routed = retry_fixture();
    let mut asked = 0;
    let got = retried(
        "injected GET",
        || Ok(()),
        |_| {
            asked += 1;
            if asked < 3 {
                Err(status_error(504, None))
            } else {
                Ok("answer")
            }
        },
    )
    .unwrap();
    assert_eq!(got, "answer");
    assert_eq!(asked, 3, "both transient answers must be retried");
}

#[test]
fn a_raw_public_channel_read_retries_504s_but_not_absence() {
    let w = world(false, Recorder::default());
    w.fake.put_file(
        SLUG,
        "main",
        "Cargo.toml",
        b"[package]\nversion = \"1.2.3\"\n",
    );
    let path = format!("/repos/{SLUG}/contents/Cargo.toml?ref=main");
    let unavailable = Fault::Status {
        status: 504,
        body: "gateway timeout".into(),
        headers: Vec::new(),
    };
    w.fake.fault(Match::api(&path), unavailable.clone());
    w.fake.fault(Match::api(&path), unavailable);
    let answer = raw(&path).unwrap();
    assert_eq!(answer.status, 200);
    assert_eq!(answer.body, b"[package]\nversion = \"1.2.3\"\n");
    assert_eq!(
        w.fake
            .seen()
            .iter()
            .filter(|seen| seen.path_and_query == path)
            .count(),
        3
    );

    let absent = format!("/repos/{SLUG}/contents/absent?ref=main");
    let before = w.fake.seen().len();
    assert_eq!(raw(&absent).unwrap().status, 404);
    assert_eq!(w.fake.seen().len(), before + 1, "a 404 is not retried");
}

#[test]
fn definitive_4xx_answers_are_not_retried() {
    for error in [
        status_error(400, None),
        status_error(404, None),
        status_error(408, None),
        status_error(429, None),
        status_error(499, None),
    ] {
        let _routed = retry_fixture();
        let mut asked = 0;
        let refused = retried::<()>(
            "injected GET",
            || Ok(()),
            |_| {
                asked += 1;
                Err(error.clone())
            },
        )
        .unwrap_err()
        .to_string();
        assert_eq!(asked, 1, "{error} must be a final answer: {refused}");
        assert!(!refused.contains("retry-pause bound"), "{refused}");
    }
}

#[test]
fn an_endless_5xx_stops_at_and_names_the_shared_bound() {
    let _routed = retry_fixture();
    let mut asked = 0;
    let refused = retried::<()>(
        "injected GET",
        || Ok(()),
        |_| {
            asked += 1;
            Err(status_error(503, None))
        },
    )
    .unwrap_err()
    .to_string();
    assert_eq!(asked, READ_RETRY_POLICY.attempts());
    assert!(refused.contains(&READ_RETRY_POLICY.bound()), "{refused}");
    assert!(refused.contains("HTTP 503"), "{refused}");
}

#[test]
fn authenticated_mutations_keep_their_three_attempt_bound() {
    let _routed = retry_fixture();
    let mut asked = 0;
    let refused = retried_via_policy::<()>(
        None,
        AUTHENTICATED_RETRY_POLICY,
        "injected PATCH",
        || Ok(()),
        |_| {
            asked += 1;
            Err(status_error(503, None))
        },
    )
    .unwrap_err()
    .to_string();
    assert_eq!(asked, 3, "mutation retries did not inherit the read budget");
    assert!(
        refused.contains(&AUTHENTICATED_RETRY_POLICY.bound()),
        "{refused}"
    );
}

#[test]
fn credentialed_reads_keep_their_three_attempt_bound() {
    let _routed = Fixture {
        origins: Origins::loopback(1, 1, 1, 1),
        token: Some("fixture-token".into()),
        spawn: None,
    }
    .enter();
    let mut asked = 0;
    let refused = retried::<()>(
        "credentialed GET",
        || Ok(()),
        |_| {
            asked += 1;
            Err(status_error(503, None))
        },
    )
    .unwrap_err()
    .to_string();
    assert_eq!(asked, AUTHENTICATED_RETRY_POLICY.attempts());
    assert!(
        refused.contains(&AUTHENTICATED_RETRY_POLICY.bound()),
        "{refused}"
    );
}

/// NEGATIVE CONTROL: narrowing the new anonymous classifier must not silently change the
/// cutter's long-standing authenticated retry classes.
#[test]
fn credentialed_reads_keep_their_legacy_transient_classes() {
    let errors = [
        status_error(408, None),
        status_error(429, None),
        RestError::Transport {
            exit: Some(1),
            status: None,
            retry_after_secs: None,
            message: "generic transport failure".into(),
        },
        RestError::Malformed("incomplete JSON".into()),
    ];
    for error in errors {
        let _routed = Fixture {
            origins: Origins::loopback(1, 1, 1, 1),
            token: Some("fixture-token".into()),
            spawn: None,
        }
        .enter();
        let mut asked = 0;
        let refused = retried::<()>(
            "credentialed GET",
            || Ok(()),
            |_| {
                asked += 1;
                Err(error.clone())
            },
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            asked,
            AUTHENTICATED_RETRY_POLICY.attempts(),
            "{error}: {refused}"
        );
    }
}

#[test]
fn numeric_retry_after_and_curl_transport_exits_use_the_shared_classifier() {
    assert!(matches!(
        retry_cause(&status_error(429, Some(7))),
        Some(Cause::RateLimited { retry_after }) if retry_after == std::time::Duration::from_secs(7)
    ));
    assert_eq!(retry_cause(&status_error(429, None)), None);
    assert_eq!(retry_cause(&curl_error(56, None)), Some(Cause::Reset));
    assert_eq!(retry_cause(&curl_error(18, None)), Some(Cause::Truncated));
    assert_eq!(retry_cause(&curl_error(28, None)), Some(Cause::Timeout));
    assert_eq!(
        retry_cause(&curl_error(56, Some(504))),
        Some(Cause::Server(504)),
        "curl 8.7.1 exit 56 on an HTTP/2 5xx stays transient"
    );
    assert_eq!(
        retry_cause(&curl_error(56, Some(404))),
        None,
        "an answered 404 takes precedence over curl exit 56"
    );

    let _routed = retry_fixture();
    let mut asked = 0;
    let got = retried(
        "rate-limited GET",
        || Ok(()),
        |_| {
            asked += 1;
            if asked == 1 {
                Err(status_error(429, Some(7)))
            } else {
                Ok("answer")
            }
        },
    )
    .unwrap();
    assert_eq!((got, asked), ("answer", 2));
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
    assert!(
        failed.contains(&AUTHENTICATED_RETRY_POLICY.bound()),
        "{failed}"
    );
    assert_eq!(started, AUTHENTICATED_RETRY_POLICY.attempts());
    assert_eq!(
        asked(&w),
        usize::try_from(AUTHENTICATED_RETRY_POLICY.attempts()).unwrap() + 1
    );
    assert!(
        w.recorder.spawned().is_empty(),
        "the launch, never the client's own spawn, started them"
    );
}

/// The probe mode of [`a_real_sigterm_reaches_a_post_cut_reads_curl_and_nothing_more_is_asked`].
#[cfg(unix)]
const SIGTERM_PROBE: &str = "ATERM_RELEASE_GITHUB_SIGTERM_PROBE";
/// The probe's lane: `launched` (the release's `StepChildren`) or `direct` (the client's own).
#[cfg(unix)]
const SIGTERM_LANE: &str = "ATERM_RELEASE_GITHUB_SIGTERM_LANE";

/// The probe.
#[cfg(unix)]
fn sigterm_probe(dir: &std::path::Path, launched: bool) -> ! {
    use std::time::{Duration, Instant};
    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    // SAFETY: SIG_DFL (0) for SIGTERM, which the parent sends this probe, whatever this test
    // binary was started with: `install` leaves an ignored one ignored.
    unsafe {
        signal(crate::interrupt::SIGTERM, 0);
    }
    crate::interrupt::install();
    let fake = FakeGithub::start();
    let secret = token("release-lane");
    fake.add_token(&secret, LOGIN);
    fake.add_repo(SLUG, RepoSpec::public().push(LOGIN));
    let latest = format!("/repos/{SLUG}/releases/latest");
    // The hold outlives the parent's hang detector in both lanes: the signal ends it (the direct
    // lane's watcher calls `end_hangs` once it is caught), never its own clock.
    let hang = 2 * aterm_spec::wait::MUST_HAPPEN;
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

/// A REAL INTERRUPT OF A POST-CUT READ.
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
        let sent = command(aterm_uds::os_path!("kill"))
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
