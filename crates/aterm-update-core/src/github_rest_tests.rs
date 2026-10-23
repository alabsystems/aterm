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
    aterm_tempfile::TempDir::new().unwrap()
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
    let mut child = rest.open_asset(&asset_path).unwrap();
    let mut streamed = Vec::new();
    std::io::Read::read_to_end(child.stdout.as_mut().unwrap(), &mut streamed).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(streamed, bytes);
    let capped = rest.download_asset(&asset_path, &got, 199_999).unwrap_err();
    assert!(matches!(capped, RestError::Malformed(_)), "{capped}");
    assert!(!got.exists());
    let cdn: Vec<Spawned> = recorder
        .spawned()
        .into_iter()
        .filter(|s| !s.args.iter().any(|a| a == "@-"))
        .collect();
    assert_eq!(
        cdn.len(),
        2,
        "two recorded CDN hops (the stream starts its own)"
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
    assert!(matches!(failed, RestError::Transport(_)), "{failed:?}");
    assert!(fake.seen().is_empty(), "nothing was sent");
}
