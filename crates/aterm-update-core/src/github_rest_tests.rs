// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! REST transport tests against `agh-fake`'s loopback GitHub via the platform's curl
//! ([`crate::http::CURL_BIN`]).

use super::*;
use agh_fake::{AssetSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec, token};
use std::sync::Mutex;

const SLUG: &str = "fixture-dev/app";
const LOGIN: &str = "fixture-dev-bot";
/// Where a control curl writes the answer it discards.
const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// One curl a client started.
#[derive(Clone, Debug)]
struct Spawned {
    program: String,
    args: Vec<String>,
    env: Vec<(String, Option<String>)>,
    stdin: Option<Vec<u8>>,
}

/// A [`Spawn`] that records each child before forwarding to [`Direct`].
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
        command: &mut Command,
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

/// agh-fake with `LOGIN` holding push on the public `SLUG`, and its token.
fn fixture() -> (FakeGithub, String) {
    let fake = FakeGithub::start();
    let secret = token("rest");
    fake.add_token(&secret, LOGIN);
    fake.add_repo(SLUG, RepoSpec::public().push(LOGIN));
    (fake, secret)
}

/// A client of `fake` as `secret` (or anonymous), starting curl through `recorder`.
fn client(fake: &FakeGithub, secret: Option<&str>, recorder: &Arc<Recorder>) -> Client {
    let p = fake.ports();
    let token = secret.map(|s| Token::new(s.as_bytes()).unwrap());
    Client::new(Origins::loopback(p.api, p.uploads, p.web, p.cdn), token)
        .with_spawn(Arc::clone(recorder) as Arc<dyn Spawn>)
}

fn scratch() -> aterm_tempfile::TempDir {
    crate::test_scratch_dir("gh-rest")
}

#[test]
fn the_token_rides_stdin_and_never_argv_or_the_environment() {
    let (fake, secret) = fixture();
    let recorder = Arc::new(Recorder::default());
    let rest = client(&fake, Some(&secret), &recorder);
    let user = rest.read("/user").unwrap();
    assert_eq!(
        user["login"].as_str(),
        Some(LOGIN),
        "the token reached GitHub"
    );
    let seen = fake.seen();
    assert_eq!(seen.last().unwrap().as_login.as_deref(), Some(LOGIN));
    let spawned = recorder.spawned();
    assert_eq!(spawned.len(), 1);
    let curl = &spawned[0];
    assert_eq!(curl.program, crate::http::CURL_BIN);
    #[cfg(unix)]
    assert_eq!(curl.program, "/usr/bin/curl", "never a PATH lookup");
    assert_eq!(
        curl.args.first().map(String::as_str),
        Some("-q"),
        "no curlrc"
    );
    assert!(curl.args.windows(2).any(|w| w == ["-H", "@-"]));
    for scrubbed in [
        "CURL_HOME",
        "XDG_CONFIG_HOME",
        "SSL_CERT_FILE",
        "CURL_CA_BUNDLE",
        "GH_TOKEN",
        "GITHUB_TOKEN",
    ] {
        assert!(
            curl.env.iter().any(|(k, v)| k == scrubbed && v.is_none()),
            "{scrubbed} is removed: {:?}",
            curl.env
        );
    }
    assert_eq!(exposed(curl, &secret), Vec::<String>::new());
    let stdin = curl.stdin.as_deref().unwrap();
    assert_eq!(
        stdin,
        format!("Authorization: Bearer {secret}\n").as_bytes()
    );
    let in_argv = Spawned {
        args: vec!["-H".into(), format!("Authorization: Bearer {secret}")],
        ..curl.clone()
    };
    assert_eq!(exposed(&in_argv, &secret).len(), 1);
    let in_env = Spawned {
        env: vec![("GH_TOKEN".into(), Some(secret.clone()))],
        ..curl.clone()
    };
    assert_eq!(exposed(&in_env, &secret), ["env: GH_TOKEN"]);
    fake.assert_clean();
}

#[test]
fn a_hostile_curlrc_is_never_read() {
    let (fake, secret) = fixture();
    let home = scratch();
    std::fs::write(
        home.path().join(".curlrc"),
        "header = \"X-Planted: from-a-curlrc\"\n",
    )
    .unwrap();
    let dir = home.path().to_str().unwrap().to_string();
    let recorder = Arc::new(Recorder {
        env: vec![("CURL_HOME".into(), dir.clone()), ("HOME".into(), dir)],
        ..Recorder::default()
    });
    client(&fake, Some(&secret), &recorder)
        .read("/user")
        .unwrap();
    let planted = |seen: &[agh_fake::Seen]| seen.iter().any(|s| s.header("x-planted").is_some());
    assert!(!planted(&fake.seen()), "-q kept the curlrc out");
    let mut bare = aterm_uds::spawnfd::command(crate::http::CURL_BIN);
    bare.args(["-sS", "-o", NULL_DEVICE, "-H", "User-Agent: control"])
        .arg(format!("http://127.0.0.1:{}/rate_limit", fake.ports().api))
        .env("CURL_HOME", home.path())
        .env("HOME", home.path());
    assert!(bare.output().unwrap().status.success());
    assert!(planted(&fake.seen()), "the control reads the curlrc");
}

#[test]
fn a_release_is_created_uploaded_listed_and_downloaded_through_curl() {
    let (fake, secret) = fixture();
    let recorder = Arc::new(Recorder::default());
    let rest = client(&fake, Some(&secret), &recorder);
    let created = rest
        .send(
            Method::Post,
            "/repos/fixture-dev/app/releases",
            Some(&aterm_json::json!({"tag_name": "v1", "draft": true, "prerelease": true})),
        )
        .unwrap()
        .success()
        .unwrap()
        .json()
        .unwrap();
    let id = created["id"].as_u64().unwrap();
    let dir = scratch();
    let file = dir.path().join("a.bin");
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&file, &bytes).unwrap();
    let url = rest.uploads(&format!(
        "/repos/fixture-dev/app/releases/{id}/assets?name=a.bin"
    ));
    let asset = rest
        .upload(&url, &file)
        .unwrap()
        .success()
        .unwrap()
        .json()
        .unwrap();
    let asset_id = asset["id"].as_u64().unwrap();
    assert_eq!(
        fake.release_state(id).unwrap().1,
        [("a.bin".into(), bytes.clone())]
    );
    let listed = rest
        .pages("/repos/fixture-dev/app/releases?per_page=100", 100)
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["assets"][0]["name"].as_str(), Some("a.bin"));
    let asset_path = format!("/repos/fixture-dev/app/releases/assets/{asset_id}");
    let got = dir.path().join("got.bin");
    rest.download_asset(&asset_path, &got, 200_000).unwrap();
    assert_eq!(std::fs::read(&got).unwrap(), bytes);
    let capped = rest.download_asset(&asset_path, &got, 199_999).unwrap_err();
    assert!(matches!(capped, RestError::Malformed(_)), "{capped}");
    assert_eq!(
        std::fs::read(&got).unwrap(),
        bytes,
        "a refused replacement preserves the complete prior asset"
    );
    let cdn: Vec<Spawned> = recorder
        .spawned()
        .into_iter()
        .filter(|s| !s.args.iter().any(|a| a == "@-"))
        .collect();
    assert_eq!(
        cdn.len(),
        2,
        "the successful and cap-refused atomic CDN transfers"
    );
    assert!(
        cdn.iter()
            .all(|s| s.stdin.is_none() && exposed(s, &secret).is_empty())
    );
    assert!(
        fake.seen()
            .iter()
            .any(|s| s.origin == FakeOrigin::Cdn && !s.had_authorization)
    );
    fake.assert_clean();
}

#[test]
fn a_redirect_off_the_cdn_origin_is_refused_and_never_followed() {
    let (fake, secret) = fixture();
    let id = fake.add_release(SLUG, ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"));
    let asset_id = fake.add_asset(id, AssetSpec::uploaded("a.bin", b"ay"));
    let asset_path = format!("/repos/fixture-dev/app/releases/assets/{asset_id}");
    let web = fake.ports().web;
    fake.fault(
        Match::api(&asset_path),
        Fault::RedirectTo {
            status: 302,
            location: format!("http://127.0.0.1:{web}/elsewhere?sig=1"),
        },
    );
    let recorder = Arc::new(Recorder::default());
    let rest = client(&fake, Some(&secret), &recorder);
    let dir = scratch();
    let dest = dir.path().join("a.bin");
    let refused = rest.download_asset(&asset_path, &dest, 1024).unwrap_err();
    assert!(matches!(refused, RestError::Redirect(_)), "{refused}");
    assert!(
        !refused.to_string().contains("sig="),
        "the Location's query is not repeated"
    );
    fake.fault(
        Match::api(&asset_path),
        Fault::RedirectTo {
            status: 302,
            location: "/elsewhere?sig=secret_rel&next=https://cdn.example/x".into(),
        },
    );
    let rel_refused = rest.download_asset(&asset_path, &dest, 1024).unwrap_err();
    assert!(
        matches!(rel_refused, RestError::Redirect(_)),
        "{rel_refused}"
    );
    assert!(
        !rel_refused.to_string().contains("sig=")
            && !rel_refused.to_string().contains("/elsewhere"),
        "a relative Location with :// in its query never leaks path or query: {rel_refused}"
    );
    fake.fault(
        Match::api(&asset_path),
        Fault::RedirectTo {
            status: 302,
            location: format!("http://secret_user:secret_pw@127.0.0.1:{web}/elsewhere?sig=1"),
        },
    );
    let cred_refused = rest.download_asset(&asset_path, &dest, 1024).unwrap_err();
    assert!(
        matches!(cred_refused, RestError::Redirect(_)),
        "{cred_refused}"
    );
    assert!(
        !cred_refused.to_string().contains("secret_") && !cred_refused.to_string().contains("sig="),
        "userinfo and query are never repeated: {cred_refused}"
    );
    assert!(
        !fake.seen().iter().any(|s| s.origin == FakeOrigin::Web),
        "never asked"
    );
    assert!(!dest.exists());
    rest.download_asset(&asset_path, &dest, 1024).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"ay");
    fake.fault(Match::cdn(), Fault::SecondHop);
    let again = rest.download_asset(&asset_path, &dest, 1024).unwrap_err();
    assert!(matches!(again, RestError::Redirect(_)), "{again}");
    fake.rename_repo(SLUG, "fixture-dev/renamed");
    let moved = rest.read("/repos/fixture-dev/app").unwrap_err();
    assert!(matches!(moved, RestError::Redirect(_)), "{moved}");
    let followed = fake
        .seen()
        .iter()
        .any(|s| s.path_and_query.starts_with("/repositories/"));
    assert!(!followed, "the 301 was not followed");
}

#[test]
fn a_listing_is_read_to_its_end_and_an_incomplete_page_is_refused() {
    let (fake, secret) = fixture();
    for n in 1..=5 {
        fake.add_release(
            SLUG,
            ReleaseSpec::published(&format!("v{n}"), &format!("2026-10-0{n}T00:00:00Z")),
        );
    }
    let recorder = Arc::new(Recorder::default());
    let rest = client(&fake, Some(&secret), &recorder);
    let base = "/repos/fixture-dev/app/releases?per_page=2";
    let tags: Vec<String> = rest
        .pages(base, 2)
        .unwrap()
        .iter()
        .map(|r| r["tag_name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        tags,
        ["v5", "v4", "v3", "v2", "v1"],
        "every page, linked by repository id"
    );
    let api = fake.ports().api;
    fake.fault(
        Match::api("/repos/fixture-dev/app/releases?per_page=2&page=2"),
        Fault::Status {
            status: 200,
            body: "[]".into(),
            headers: vec![(
                "Link".into(),
                format!("<http://127.0.0.1:{api}/repositories/1000/releases?per_page=2&page=3>; rel=\"next\""),
            )],
        },
    );
    let incomplete = rest.pages(base, 2).unwrap_err();
    assert!(
        matches!(incomplete, RestError::Malformed(_)),
        "{incomplete}"
    );
    assert!(
        incomplete.to_string().contains("advertises more"),
        "{incomplete}"
    );
    fake.fault(
        Match::api("/repos/fixture-dev/app/releases?per_page=2&page=2"),
        Fault::Status {
            status: 502,
            body: "{\"message\":\"Bad Gateway\"}".into(),
            headers: vec![],
        },
    );
    assert_eq!(rest.pages(base, 2).unwrap_err().status(), Some(502));
    assert!(
        rest.pages("/repos/fixture-dev/none/releases?per_page=2", 2)
            .unwrap_err()
            .is_not_found()
    );
    fake.assert_clean();
}

#[test]
fn a_write_without_a_token_and_a_url_off_the_origins_never_start_curl() {
    let (fake, secret) = fixture();
    let recorder = Arc::new(Recorder::default());
    let anonymous = client(&fake, None, &recorder);
    let write = anonymous
        .send(Method::Delete, "/repos/fixture-dev/app/releases/1", None)
        .unwrap_err();
    assert!(matches!(write, RestError::Local(_)), "{write}");
    let named = client(&fake, Some(&secret), &recorder);
    let off = Request::get("http://127.0.0.1:1/user");
    assert!(matches!(named.exchange(&off), Err(RestError::Local(_))));
    let sneaky = format!("{}@127.0.0.1:1/user", named.api(""));
    assert!(matches!(
        named.exchange(&Request::get(&sneaky)),
        Err(RestError::Local(_))
    ));
    assert!(recorder.spawned().is_empty(), "nothing started");
    assert_eq!(
        anonymous.read("/repos/fixture-dev/app").unwrap()["id"].as_u64(),
        Some(1000)
    );
    assert_eq!(recorder.spawned().len(), 1);
    assert_eq!(recorder.spawned()[0].stdin, None);
}

#[test]
fn a_token_that_could_write_a_header_is_refused() {
    assert!(Token::new(b"").is_err());
    assert!(Token::new(b"   \n").is_err());
    assert!(Token::new(b"abc\r\nX-Injected: 1").is_err());
    assert!(Token::new(b"a b").is_err());
    assert!(
        Token::new(b"  ghp_abc\n").is_ok(),
        "surrounding whitespace is dropped"
    );
    let dir = scratch();
    assert!(
        Token::from_file(&dir.path().join("absent"))
            .unwrap()
            .is_none()
    );
    std::fs::write(dir.path().join("blank"), "\n").unwrap();
    assert!(
        Token::from_file(&dir.path().join("blank"))
            .unwrap()
            .is_none()
    );
    std::fs::write(dir.path().join("two"), "one\ntwo\n").unwrap();
    assert!(Token::from_file(&dir.path().join("two")).is_err());
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        let fifo = dir.path().join("token.fifo");
        let c_fifo = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_fifo` is a valid NUL-terminated path inside the private scratch directory.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        assert!(
            Token::from_file(&fifo).is_err(),
            "a FIFO at the token path must be refused without blocking"
        );
    }
    assert_eq!(
        format!("{:?}", Token::new(b"ghp_abc").unwrap()),
        "Token(<redacted>)"
    );
}

#[test]
fn fail_with_body_keeps_curls_own_reading_and_githubs_words() {
    let (fake, secret) = fixture();
    fake.add_release(SLUG, ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"));
    let recorder = Arc::new(Recorder::default());
    let rest = client(&fake, Some(&secret), &recorder);
    let url = rest.api("/repos/fixture-dev/app/releases");
    let body = br#"{"tag_name":"v1"}"#;
    let request = Request {
        method: Method::Post,
        url: &url,
        accept: ACCEPT_JSON,
        body: Body::Json(body),
        fail_with_body: true,
    };
    let prepared = rest.prepare(&request).unwrap();
    assert!(prepared.argv().iter().all(|a| !a.contains(&secret)));
    let refused = prepared.run().unwrap();
    assert_eq!(refused.exit, Some(22));
    assert!(
        refused.stderr.contains("returned error: 422"),
        "{}",
        refused.stderr
    );
    assert_eq!(refused.status, Some(422));
    let answer = refused.answer().unwrap();
    assert_eq!(
        answer.message(),
        "Validation Failed (tag_name: already_exists)"
    );
    let plain = rest
        .exchange(&Request {
            fail_with_body: false,
            ..request
        })
        .unwrap();
    assert_eq!((plain.exit, plain.status), (Some(0), Some(422)));
}

/// The release retry policy consumes these structured fields. NEGATIVE CONTROL: both decimal
/// and HTTP-date `Retry-After` values are admitted, and a transport failure retains both curl's
/// exit and any status it observed rather than forcing callers to parse the diagnostic.
#[test]
fn retry_classification_inputs_survive_the_rest_error_boundary() {
    let limited = Answer {
        status: 429,
        headers: vec![("Retry-After".into(), "17".into())],
        body: br#"{"message":"slow down"}"#.to_vec(),
    }
    .success()
    .unwrap_err();
    assert_eq!(limited.status(), Some(429));
    assert_eq!(limited.retry_after_secs(), Some(17));

    let dated = vec![("retry-after".into(), "Sun, 08 Nov 2026 12:00:00 GMT".into())];
    let now = u64::try_from(aterm_types::rfc3339::days_from_civil(2026, 11, 8)).unwrap() * 86_400
        + 11 * 3_600
        + 59 * 60
        + 43;
    assert_eq!(
        retry_after_header_at(&dated, "retry-after", Some(now)),
        Some(17)
    );

    let reset = Exchange {
        exit: Some(56),
        stderr: "curl: (56) HTTP/2 stream was reset".into(),
        status: Some(504),
        headers: vec![("Retry-After".into(), "9".into())],
        body: Vec::new(),
    }
    .answer()
    .unwrap_err();
    assert_eq!(reset.curl_exit(), Some(56));
    assert_eq!(reset.status(), Some(504));
    assert_eq!(reset.retry_after_secs(), Some(9));
}

/// HTTP status evidence outranks curl's body-cap exit and a body already over the cap.
/// NEGATIVE CONTROLS: cap-first classification makes 504/404 malformed, while an oversized
/// successful answer must remain malformed.
#[test]
fn answered_status_precedes_the_json_body_cap() {
    for status in [504, 404] {
        assert!(!answer_exceeds_cap(
            Some(63),
            Some(status),
            "curl: (63) Maximum file size exceeded",
            JSON_CAP + 1,
        ));
    }
    assert!(answer_exceeds_cap(
        Some(63),
        Some(200),
        "curl: (63) Maximum file size exceeded",
        JSON_CAP + 1,
    ));

    let failure = |status| {
        Exchange {
            exit: Some(63),
            stderr: "curl: (63) Maximum file size exceeded".into(),
            status: Some(status),
            headers: Vec::new(),
            body: Vec::new(),
        }
        .answer()
        .unwrap_err()
    };
    assert_eq!(failure(504).retry_cause(), Some(Cause::Server(504)));
    let missing = failure(404);
    assert!(missing.is_not_found());
    assert_eq!(missing.retry_cause(), None);

    let server = cdn_failure(
        Some(63),
        Some(504),
        None,
        "curl: (63) Maximum file size exceeded",
        9,
    );
    assert_eq!(server.retry_cause(), Some(Cause::Server(504)));
    let missing = cdn_failure(
        Some(63),
        Some(404),
        None,
        "curl: (63) Maximum file size exceeded",
        9,
    );
    assert!(missing.is_not_found());
    assert_eq!(missing.retry_cause(), None);
    assert!(matches!(
        cdn_failure(
            Some(63),
            None,
            None,
            "curl: (63) Maximum file size exceeded",
            9,
        ),
        RestError::Malformed(_)
    ));
}

fn asset_status(status: u16) -> RestError {
    RestError::Status {
        status,
        retry_after_secs: None,
        message: format!("injected HTTP {status}"),
    }
}

fn asset_transport(exit: i32) -> RestError {
    RestError::Transport {
        exit: Some(exit),
        status: None,
        retry_after_secs: None,
        message: format!("injected curl exit {exit}"),
    }
}

fn downloadable_asset() -> (FakeGithub, String, String, Arc<Recorder>) {
    let (fake, secret) = fixture();
    let release = fake.add_release(SLUG, ReleaseSpec::published("v1", "2026-11-08T00:00:00Z"));
    let asset = fake.add_asset(release, AssetSpec::uploaded("a.bin", b"abcd"));
    let path = format!("/repos/{SLUG}/releases/assets/{asset}");
    (fake, secret, path, Arc::new(Recorder::default()))
}

/// The CDN transport is injected below the real, retried API-location read. NEGATIVE CONTROLS:
/// removing the retry admits neither the third answer nor the exact three-attempt assertion;
/// widening the classifier retries the 404 and consumes the deliberately absent second result.
#[test]
fn an_atomic_asset_download_retries_504_but_never_404() {
    use std::cell::Cell;

    let (fake, secret, path, recorder) = downloadable_asset();
    let rest = client(&fake, Some(&secret), &recorder);
    let dir = scratch();
    let dest = dir.path().join("a.bin");
    std::fs::write(&dest, b"stale").unwrap();
    let calls = Cell::new(0_u32);
    rest.download_asset_via(
        &path,
        &dest,
        4,
        |_location, staged, _headers, remaining, offset| {
            assert_eq!(
                std::fs::read(&dest).unwrap(),
                b"stale",
                "the previous asset remains readable until publication"
            );
            assert_ne!(staged, dest.as_path(), "bytes stay staged until success");
            assert_eq!((remaining, offset), (4, 0));
            let call = calls.get().saturating_add(1);
            calls.set(call);
            if call <= 2 {
                Err(asset_status(504))
            } else {
                std::fs::write(staged, b"abcd").unwrap();
                Ok(())
            }
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 3);
    assert_eq!(std::fs::read(&dest).unwrap(), b"abcd");

    let refused = dir.path().join("refused.bin");
    std::fs::write(&refused, b"stale").unwrap();
    let calls = Cell::new(0_u32);
    let error = rest
        .download_asset_via(
            &path,
            &refused,
            4,
            |_location, _staged, _headers, _remaining, _offset| {
                calls.set(calls.get().saturating_add(1));
                Err(asset_status(404))
            },
        )
        .unwrap_err();
    assert_eq!(error.status(), Some(404));
    assert_eq!(calls.get(), 1, "a 404 is final on its first answer");
    assert_eq!(
        std::fs::read(&refused).unwrap(),
        b"stale",
        "a refused replacement preserves the previous complete asset"
    );
}

/// The real curl path must retain the status trailer that drives the injected state-machine
/// controls above. NEGATIVE CONTROL: either deleting the retry or treating 404 as transient
/// changes the exact CDN request counts.
#[test]
fn the_cdn_status_trailer_retries_two_504s_then_stops_on_404() {
    let (fake, secret, path, recorder) = downloadable_asset();
    let unavailable = Fault::Status {
        status: 504,
        body: "gateway timeout exceeds the four-byte asset cap".into(),
        headers: vec![],
    };
    fake.fault(Match::cdn(), unavailable.clone());
    fake.fault(Match::cdn(), unavailable);
    let rest = client(&fake, Some(&secret), &recorder);
    let dir = scratch();
    let dest = dir.path().join("a.bin");
    rest.download_asset(&path, &dest, 4).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"abcd");
    let first = fake
        .seen()
        .iter()
        .filter(|seen| seen.origin == FakeOrigin::Cdn)
        .count();
    assert_eq!(first, 3, "504, 504, then the successful CDN answer");

    fake.fault(
        Match::cdn(),
        Fault::Status {
            status: 404,
            body: "not found exceeds the four-byte asset cap".into(),
            headers: vec![],
        },
    );
    let error = rest.download_asset(&path, &dest, 4).unwrap_err();
    assert_eq!(error.status(), Some(404), "{error}");
    let after = fake
        .seen()
        .iter()
        .filter(|seen| seen.origin == FakeOrigin::Cdn)
        .count();
    assert_eq!(after - first, 1, "a 404 is never retried");
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        b"abcd",
        "a refused refresh preserves the prior complete asset"
    );
}

/// NEGATIVE CONTROL: if the partial is discarded, the second injected attempt sees offset zero
/// and this test fails; if curl 18 stops being transient, there is no second attempt.
#[test]
fn a_partial_asset_transfer_earns_a_bounded_resume() {
    use std::cell::{Cell, RefCell};
    use std::io::Write as _;

    let (fake, secret, path, recorder) = downloadable_asset();
    let rest = client(&fake, Some(&secret), &recorder);
    let dir = scratch();
    let dest = dir.path().join("a.bin");
    let calls = Cell::new(0_u32);
    let offsets = RefCell::new(Vec::new());
    rest.download_asset_via(
        &path,
        &dest,
        4,
        |_location, staged, _headers, remaining, offset| {
            calls.set(calls.get().saturating_add(1));
            offsets.borrow_mut().push(offset);
            match calls.get() {
                1 => {
                    assert_eq!(remaining, 4);
                    std::fs::write(staged, b"ab").unwrap();
                    Err(asset_transport(18))
                }
                2 => {
                    assert_eq!(remaining, 2);
                    let mut file = std::fs::OpenOptions::new()
                        .append(true)
                        .open(staged)
                        .unwrap();
                    file.write_all(b"cd").unwrap();
                    Ok(())
                }
                _ => panic!("the completed resume was retried"),
            }
        },
    )
    .unwrap();
    assert_eq!(&*offsets.borrow(), &[0, 2]);
    assert_eq!(std::fs::read(dest).unwrap(), b"abcd");
}

/// A CDN may decline Range after the first response broke. NEGATIVE CONTROL: without the one
/// fresh restart, curl exit 33 is final and the third injected answer is never reached.
#[test]
fn a_refused_range_restarts_the_asset_once_from_byte_zero() {
    use std::cell::{Cell, RefCell};

    let (fake, secret, path, recorder) = downloadable_asset();
    let rest = client(&fake, Some(&secret), &recorder);
    let dir = scratch();
    let dest = dir.path().join("a.bin");
    let calls = Cell::new(0_u32);
    let offsets = RefCell::new(Vec::new());
    rest.download_asset_via(
        &path,
        &dest,
        4,
        |_location, staged, _headers, _remaining, offset| {
            calls.set(calls.get().saturating_add(1));
            offsets.borrow_mut().push(offset);
            match calls.get() {
                1 => {
                    std::fs::write(staged, b"ab").unwrap();
                    Err(asset_transport(18))
                }
                2 => Err(asset_transport(33)),
                3 => {
                    assert!(!staged.exists(), "the refused prefix was discarded");
                    std::fs::write(staged, b"abcd").unwrap();
                    Ok(())
                }
                _ => panic!("a fresh successful answer was retried"),
            }
        },
    )
    .unwrap();
    assert_eq!(&*offsets.borrow(), &[0, 2, 0]);
    assert_eq!(std::fs::read(dest).unwrap(), b"abcd");
}

/// An answered 416 is a final 4xx, not permission to discard a prefix and issue a fresh request.
/// NEGATIVE CONTROL: treating 416 as a Range-refusal retry reaches the forbidden third call.
#[test]
fn an_answered_416_is_not_retried_from_byte_zero() {
    use std::cell::{Cell, RefCell};

    let (fake, secret, path, recorder) = downloadable_asset();
    let rest = client(&fake, Some(&secret), &recorder);
    let dir = scratch();
    let dest = dir.path().join("a.bin");
    let calls = Cell::new(0_u32);
    let offsets = RefCell::new(Vec::new());
    let error = rest
        .download_asset_via(
            &path,
            &dest,
            4,
            |_location, staged, _headers, _remaining, offset| {
                calls.set(calls.get().saturating_add(1));
                offsets.borrow_mut().push(offset);
                match calls.get() {
                    1 => {
                        std::fs::write(staged, b"ab").unwrap();
                        Err(asset_transport(18))
                    }
                    2 => Err(asset_status(416)),
                    _ => panic!("an answered 416 was retried"),
                }
            },
        )
        .unwrap_err();
    assert_eq!(error.status(), Some(416));
    assert_eq!(calls.get(), 2);
    assert_eq!(&*offsets.borrow(), &[0, 2]);
}

/// NEGATIVE CONTROL: an unbounded loop never returns; the shared policy's exact attempt count
/// and bound text both change if this reader drifts from it.
#[test]
fn an_endless_asset_5xx_stops_at_and_names_the_shared_bound() {
    use std::cell::Cell;

    let (fake, secret, path, recorder) = downloadable_asset();
    let rest = client(&fake, Some(&secret), &recorder);
    let dir = scratch();
    let calls = Cell::new(0_u32);
    let error = rest
        .download_asset_via(
            &path,
            &dir.path().join("a.bin"),
            4,
            |_location, _staged, _headers, _remaining, _offset| {
                calls.set(calls.get().saturating_add(1));
                Err(asset_status(503))
            },
        )
        .unwrap_err();
    assert_eq!(calls.get(), Policy::STANDARD.attempts());
    assert!(
        error.to_string().contains(&Policy::STANDARD.bound()),
        "{error}"
    );
}

/// The API redirect read is part of the same operation, rather than a one-shot prelude before
/// the CDN loop. Its injected endless 504 therefore exhausts the same named bound before the
/// CDN transport is ever invoked.
#[test]
fn an_asset_location_5xx_exhausts_before_the_cdn_is_started() {
    use std::cell::Cell;

    let (fake, secret, path, recorder) = downloadable_asset();
    fake.fault_always(
        Match::api(&path),
        Fault::Status {
            status: 504,
            body: r#"{"message":"Gateway Timeout"}"#.into(),
            headers: vec![],
        },
    );
    let rest = client(&fake, Some(&secret), &recorder);
    let dir = scratch();
    let transfers = Cell::new(0_u32);
    let error = rest
        .download_asset_via(
            &path,
            &dir.path().join("a.bin"),
            4,
            |_location, _staged, _headers, _remaining, _offset| {
                transfers.set(transfers.get().saturating_add(1));
                Ok(())
            },
        )
        .unwrap_err();
    assert_eq!(transfers.get(), 0);
    assert_eq!(error.status(), Some(504));
    assert!(
        error.to_string().contains(&Policy::STANDARD.bound()),
        "{error}"
    );
}

#[test]
fn an_upload_names_its_file_and_never_curls_stdin_or_a_glob() {
    let (fake, secret) = fixture();
    let recorder = Arc::new(Recorder::default());
    let rest = client(&fake, Some(&secret), &recorder);
    let id = fake.add_release(SLUG, ReleaseSpec::draft("v1", "2026-10-01T00:00:00Z"));
    let url = rest.uploads(&format!(
        "/repos/fixture-dev/app/releases/{id}/assets?name=a.bin"
    ));
    let dot = |rest: &str| format!(".{}{rest}", std::path::MAIN_SEPARATOR);
    for (given, spelled) in [
        ("-", dot("-")),
        (".", dot(".")),
        ("dist/a.bin", dot("dist/a.bin")),
        ("/abs/a[1-2].bin", "/abs/a[1-2].bin".to_owned()),
    ] {
        let argv = rest
            .prepare(&Request {
                method: Method::Post,
                url: &url,
                accept: ACCEPT_JSON,
                body: Body::File(Path::new(given)),
                fail_with_body: false,
            })
            .unwrap()
            .argv();
        assert!(
            argv.windows(2)
                .any(|w| w == ["--upload-file", spelled.as_str()]),
            "{given}: {argv:?}"
        );
        assert!(argv.iter().any(|a| a == "--globoff"), "{argv:?}");
    }
    let dir = scratch();
    let file = dir.path().join("a[1-2].bin");
    std::fs::write(&file, b"the file's own bytes").unwrap();
    rest.upload(&url, &file).unwrap().success().unwrap();
    assert_eq!(
        fake.release_state(id).unwrap().1,
        [("a.bin".to_string(), b"the file's own bytes".to_vec())]
    );
    let before = fake.seen().len();
    let bare = aterm_uds::spawnfd::command(crate::http::CURL_BIN)
        .args([
            "-q",
            "-sS",
            "-o",
            NULL_DEVICE,
            "-X",
            "POST",
            "-H",
            "Expect:",
        ])
        .arg("--upload-file")
        .arg(&file)
        .arg("--")
        .arg(&url)
        .output()
        .unwrap();
    assert_eq!(bare.status.code(), Some(26), "{bare:?}");
    assert_eq!(fake.seen().len(), before);
}

/// A [`Spawn`] that never starts curl, failing with `kind`.
struct Refusing(std::io::ErrorKind);

impl Spawn for Refusing {
    fn run(
        &self,
        _command: &mut Command,
        _stdin: Option<&[u8]>,
    ) -> std::io::Result<std::process::Output> {
        Err(std::io::Error::new(
            self.0,
            "not started: the run was interrupted",
        ))
    }
}

#[test]
fn a_spawn_that_refuses_once_interrupted_is_a_refusal_made_here() {
    let (fake, secret) = fixture();
    let p = fake.ports();
    let rest = |kind| {
        Client::new(
            Origins::loopback(p.api, p.uploads, p.web, p.cdn),
            Some(Token::new(secret.as_bytes()).unwrap()),
        )
        .with_spawn(Arc::new(Refusing(kind)) as Arc<dyn Spawn>)
    };
    let refused = rest(std::io::ErrorKind::Interrupted)
        .read("/user")
        .unwrap_err();
    assert!(
        matches!(&refused, RestError::Local(m) if m.contains("interrupted")),
        "{refused:?}"
    );
    let failed = rest(std::io::ErrorKind::NotFound)
        .read("/user")
        .unwrap_err();
    assert!(matches!(failed, RestError::Transport { .. }), "{failed:?}");
    assert!(fake.seen().is_empty(), "nothing was sent");
}

/// A child [`Direct`] could not start is marked so: [`never_started`] knows nothing it would
/// have sent went out, and the error keeps its kind and its words. NEGATIVE CONTROL: a child
/// that started and failed is not marked, and neither is an error raised anywhere else.
#[test]
fn a_child_that_never_started_is_marked_so() {
    let dir = scratch();
    let missing = dir.path().join("no-such-curl");
    let failed = Direct
        .run(&mut aterm_uds::spawnfd::command(&missing), Some(b"secret"))
        .unwrap_err();
    assert!(never_started(&failed), "{failed:?}");
    assert_eq!(failed.kind(), std::io::ErrorKind::NotFound);
    let plain = std::io::Error::new(std::io::ErrorKind::NotFound, "elsewhere");
    assert_eq!(unstarted(plain).to_string(), "elsewhere");
    assert!(!never_started(&std::io::Error::other("waiting failed")));
    let mut curl = aterm_uds::spawnfd::command(crate::http::CURL_BIN);
    curl.arg("--no-such-option");
    let ran = Direct.run(&mut curl, None).unwrap();
    assert!(
        !ran.status.success(),
        "it started, and failed by its own exit"
    );
}
