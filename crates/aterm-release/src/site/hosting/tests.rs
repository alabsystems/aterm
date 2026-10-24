// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Hosting deploy against [`super::fake`], a loopback token endpoint and Hosting API,
//! through the real `/usr/bin/curl`; and firebase.json's translation, key by key.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use aterm_json::{Value, json};
use aterm_uds::secret::Secret;
use aterm_update_core::github_rest::{Direct, Spawn};

use super::fake::{Fake, TOKEN};
use super::key::tests::{TEST_PKCS8, key_json};
use super::*;

const ISSUER: &str = "deployer@fake-project.iam.gserviceaccount.com";

/// The committed fixture's firebase.json, shaped as the site's own.
fn fixture_config() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site/firebase.json"),
    )
    .unwrap()
}

/// One curl the deploy started, as a process listing and its environment show it, what it
/// read on stdin, and every file under the scratch parent while it ran.
#[derive(Clone, Debug)]
struct Spawned {
    program: String,
    args: Vec<String>,
    env: Vec<(String, Option<String>)>,
    stdin: Option<Vec<u8>>,
    files: Vec<(String, Vec<u8>)>,
}

/// A [`Spawn`] that records each child and the files under `scratch` as it starts, then
/// starts it as [`Direct`] does — or, for a URL holding [`Recorder::refuse`]'s text, fails
/// it without starting it, as a start that never happened (`true`) or as a child whose wait
/// failed after it started (`false`).
struct Recorder {
    scratch: std::path::PathBuf,
    spawned: Mutex<Vec<Spawned>>,
    refuse: Mutex<Option<(&'static str, bool)>>,
}

/// Every file under `dir`, with its bytes.
fn files_under(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                found.push((path.display().to_string(), bytes));
            }
        }
    }
    found
}

impl Recorder {
    fn new(scratch: &Path) -> Arc<Recorder> {
        Arc::new(Recorder {
            scratch: scratch.to_path_buf(),
            spawned: Mutex::new(Vec::new()),
            refuse: Mutex::new(None),
        })
    }

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
            files: files_under(&self.scratch),
        });
        let url = command
            .get_args()
            .last()
            .map(|a| a.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some((text, unstarted)) = *self.refuse.lock().unwrap()
            && url.contains(text)
        {
            let e = std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "not started: the release was interrupted",
            );
            return Err(if unstarted {
                aterm_update_core::github_rest::unstarted(e)
            } else {
                std::io::Error::other("waiting for curl failed")
            });
        }
        Direct.run(command, stdin)
    }
}

/// Where `secret` shows in a spawn: an argument, an environment value, or a file.
fn exposed(spawned: &Spawned, secret: &[u8]) -> Vec<String> {
    let holds = |hay: &[u8]| hay.windows(secret.len()).any(|w| w == secret);
    let mut found: Vec<String> = spawned
        .args
        .iter()
        .filter(|a| holds(a.as_bytes()))
        .map(|a| format!("argv: {a}"))
        .collect();
    found.extend(
        spawned
            .env
            .iter()
            .filter(|(_, v)| v.as_deref().is_some_and(|v| holds(v.as_bytes())))
            .map(|(k, _)| format!("env: {k}")),
    );
    found.extend(
        spawned
            .files
            .iter()
            .filter(|(_, bytes)| holds(bytes))
            .map(|(path, _)| format!("file: {path}")),
    );
    found
}

/// A deploy's surroundings: the fake, a site checkout with `public/`, a scratch parent, a
/// recorder, and a key exchanged at the fake's token endpoint.
struct Lab {
    fake: Fake,
    dir: aterm_tempfile::TempDir,
    recorder: Arc<Recorder>,
}

impl Lab {
    fn new() -> Lab {
        let pair = ring::signature::RsaKeyPair::from_pkcs8(TEST_PKCS8).unwrap();
        let fake = Fake::start(pair.public().as_ref().to_vec(), ISSUER);
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-hosting-")
            .tempdir()
            .unwrap();
        let site = dir.path().join("site");
        for (rel, text) in [
            ("public/index.html", "<!doctype html><p>home</p>\n"),
            ("public/terminal.html", "<!doctype html><p>terminal</p>\n"),
            ("public/assets/term/engine.js", "export {};\n"),
            ("public/assets/copy.txt", "<!doctype html><p>home</p>\n"),
            ("public/.DS_Store", "junk"),
            ("public/firebase.json", "{}"),
        ] {
            let path = site.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        std::fs::write(site.join("firebase.json"), fixture_config()).unwrap();
        std::fs::create_dir_all(dir.path().join("scratch")).unwrap();
        let recorder = Recorder::new(&dir.path().join("scratch"));
        Lab {
            fake,
            dir,
            recorder,
        }
    }

    fn site(&self) -> std::path::PathBuf {
        self.dir.path().join("site")
    }

    fn client(&self) -> Client {
        Client::new(
            self.fake.origins.clone(),
            Arc::clone(&self.recorder) as Arc<dyn Spawn>,
            &self.dir.path().join("scratch"),
        )
        .unwrap()
    }

    fn key(&self, token_uri: &str) -> Key {
        Key::parse(&Secret::new(key_json(token_uri).as_bytes())).unwrap()
    }

    fn plan(&self) -> Plan {
        let hosting = config::read(&self.site()).unwrap();
        Plan::of(
            &self.site(),
            &hosting,
            Some("fake-site".into()),
            &BTreeMap::new(),
        )
        .unwrap()
    }

    /// The deploy, its log, and its result.
    fn deploy(&self, key: &Key) -> (Result<Released, Failed>, String) {
        let mut out = Vec::new();
        let ended = deploy(
            &self.client(),
            key,
            &self.plan(),
            "ship site: test",
            1_700_000_000,
            &mut out,
        );
        (ended, String::from_utf8(out).unwrap())
    }
}

fn gunzip(member: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    aterm_codec::inflate::stream::GzipReader::new(member)
        .read_to_end(&mut out)
        .unwrap();
    out
}

/// The whole flow, in order: a token, a version with the translated config, the files
/// named by the hashes of their members, an upload of each distinct member (each one the
/// OS's gzip also reads back to the file), the finalize, and the release that makes it
/// live. NEGATIVE CONTROL: a key whose issuer the token endpoint does not take gets no
/// token, and Hosting is never asked anything.
#[test]
fn the_full_flow_creates_populates_uploads_finalizes_and_releases() {
    let lab = Lab::new();
    let key = lab.key(&lab.fake.origins.token);
    let (ended, log) = lab.deploy(&key);
    let released = ended.unwrap();
    assert_eq!(released.version, "sites/fake-site/versions/v1");
    assert_eq!(released.release, "sites/fake-site/releases/r1");
    assert_eq!((released.files, released.uploaded), (4, 3), "{log}");
    assert_eq!(
        lab.fake.calls(),
        [
            "POST /token",
            "POST /v1beta1/sites/fake-site/versions",
            "POST /v1beta1/sites/fake-site/versions/v1:populateFiles",
            "POST upload",
            "POST upload",
            "POST upload",
            "PATCH /v1beta1/sites/fake-site/versions/v1?updateMask=status",
            "POST /v1beta1/sites/fake-site/releases?versionName=sites%2Ffake-site%2Fversions%2Fv1",
        ]
    );
    let st = lab.fake.state();
    assert_eq!(st.live.as_deref(), Some("sites/fake-site/versions/v1"));
    let config = &st.configs["sites/fake-site/versions/v1"];
    assert_eq!(config, &config::parse(&fixture_config()).unwrap().config);
    let populated = &st.populated["sites/fake-site/versions/v1"];
    assert_eq!(
        populated.keys().collect::<Vec<_>>(),
        [
            "/assets/copy.txt",
            "/assets/term/engine.js",
            "/index.html",
            "/terminal.html"
        ],
        "firebase.json, the dot file and nothing else are left out"
    );
    for (path, hash) in populated {
        let disk = std::fs::read(lab.site().join("public").join(&path[1..])).unwrap();
        let member = &st.uploaded[hash];
        assert_eq!(gunzip(member), disk, "{path}");
        let mut gzip = aterm_uds::spawnfd::command("/usr/bin/gzip");
        gzip.arg("-dc")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped());
        let mut child = gzip.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), member).unwrap();
        let read = child.wait_with_output().unwrap();
        assert!(read.status.success(), "the OS's gzip reads {path}'s member");
        assert_eq!(read.stdout, disk, "the OS's gzip agrees on {path}");
    }
    let release = st.seen.last().unwrap();
    assert_eq!(
        aterm_json::from_slice::<Value>(&release.body).unwrap(),
        json!({"message": "ship site: test"})
    );
    drop(st);
    assert!(
        log.contains("released sites/fake-site/versions/v1"),
        "{log}"
    );

    let stranger = Lab::new();
    stranger.fake.state().issuer = "someone@else.iam.gserviceaccount.com".into();
    let key = stranger.key(&stranger.fake.origins.token);
    let (ended, _) = stranger.deploy(&key);
    match ended {
        Err(Failed::Credential(why)) => {
            assert!(why.contains("invalid_grant"), "{why}");
        }
        other => panic!("a key the endpoint refuses deploys nothing: {other:?}"),
    }
    assert_eq!(stranger.fake.calls(), ["POST /token"]);
}

/// populateFiles' answer decides the uploads: a member Hosting already holds is never
/// sent, and two files with the same bytes send one member. NEGATIVE CONTROL: with nothing
/// held, every distinct member goes up.
#[test]
fn only_the_hashes_hosting_asks_for_are_uploaded() {
    let lab = Lab::new();
    let plan = lab.plan();
    let index = plan
        .files
        .iter()
        .find(|f| f.path == "/index.html")
        .unwrap()
        .hash
        .clone();
    let engine = plan
        .files
        .iter()
        .find(|f| f.path == "/assets/term/engine.js")
        .unwrap()
        .hash
        .clone();
    lab.fake
        .state()
        .held
        .extend([index.clone(), engine.clone()]);
    let key = lab.key(&lab.fake.origins.token);
    let (ended, log) = lab.deploy(&key);
    assert_eq!(ended.unwrap().uploaded, 1, "{log}");
    let st = lab.fake.state();
    let sent: Vec<&String> = st.uploaded.keys().collect();
    let terminal = &plan
        .files
        .iter()
        .find(|f| f.path == "/terminal.html")
        .unwrap()
        .hash;
    assert_eq!(sent, [terminal], "only the member Hosting lacked");
    assert!(
        !st.uploaded.contains_key(&index),
        "/assets/copy.txt has index.html's bytes, which Hosting holds"
    );
    drop(st);

    let fresh = Lab::new();
    let key = fresh.key(&fresh.fake.origins.token);
    let (ended, _) = fresh.deploy(&key);
    assert_eq!(
        ended.unwrap().uploaded,
        3,
        "four files, three distinct members"
    );
    assert_eq!(fresh.fake.state().uploaded.len(), 3);
}

/// A failed upload, or a failed finalize, stops the deploy before `releases.create`: the
/// live site keeps the release it had, and the failure names the version left unreleased.
/// NEGATIVE CONTROL: the same lab with no fault releases.
#[test]
fn a_failed_upload_or_finalize_leaves_the_live_release_untouched() {
    for (route, step) in [
        ("upload", "uploading"),
        ("finalize", "finalizing the version"),
    ] {
        let lab = Lab::new();
        lab.fake.state().live = Some("sites/fake-site/versions/v0".into());
        lab.fake.state().faults.insert(route, 503);
        let key = lab.key(&lab.fake.origins.token);
        let (ended, log) = lab.deploy(&key);
        let failed = ended.unwrap_err();
        match &failed {
            Failed::NotReleased { why, version } => {
                assert!(why.starts_with(step), "{route}: {why}");
                assert!(why.contains("HTTP 503"), "{route}: {why}");
                assert_eq!(version.as_deref(), Some("sites/fake-site/versions/v1"));
            }
            other => panic!("{route}: {other:?}"),
        }
        let said = failed.to_string();
        assert!(
            said.contains("the live site still serves its previous release")
                && said.contains("sites/fake-site/versions/v1 was left unreleased"),
            "{said}"
        );
        assert!(
            !lab.fake.calls().iter().any(|c| c.contains("/releases")),
            "{route}: no releases.create: {:?}",
            lab.fake.calls()
        );
        assert_eq!(
            lab.fake.state().live.as_deref(),
            Some("sites/fake-site/versions/v0"),
            "{route}: {log}"
        );
        lab.fake.state().faults.clear();
        let (ended, _) = lab.deploy(&key);
        assert!(
            ended.is_ok(),
            "{route}: with the fault gone the rerun releases"
        );
        assert_eq!(
            lab.fake.state().live.as_deref(),
            Some("sites/fake-site/versions/v2")
        );
    }
}

/// The access token rides curl's stdin as a header line, and the signed assertion rides it as
/// the token request's body: neither is ever an argument, an environment value, a file under
/// the scratch directory, or a line of the log. NEGATIVE CONTROL: the recorder does see a
/// credential where one was put — the stdin it was handed, and a file planted holding it.
#[test]
fn the_token_never_reaches_argv_env_a_file_or_the_log() {
    let lab = Lab::new();
    let key = lab.key(&lab.fake.origins.token);
    let (ended, log) = lab.deploy(&key);
    ended.unwrap();
    let assertion_head = {
        let jwt = key.assertion(1).unwrap();
        let text = String::from_utf8(jwt.expose().to_vec()).unwrap();
        text.split('.').nth(1).unwrap()[..24].to_string()
    };
    let spawned = lab.recorder.spawned();
    assert_eq!(spawned.len(), lab.fake.calls().len());
    let bearer = format!("Authorization: Bearer {TOKEN}\n");
    for (i, s) in spawned.iter().enumerate() {
        assert_eq!(s.program, "/usr/bin/curl");
        assert_eq!(s.args.first().map(String::as_str), Some("-q"), "no curlrc");
        assert!(
            exposed(s, TOKEN.as_bytes()).is_empty(),
            "{:?}",
            exposed(s, TOKEN.as_bytes())
        );
        assert!(
            exposed(s, assertion_head.as_bytes()).is_empty(),
            "{:?}",
            exposed(s, assertion_head.as_bytes())
        );
        let stdin = s.stdin.as_deref().unwrap_or_default();
        if i == 0 {
            assert!(s.args.windows(2).any(|w| w == ["--data-binary", "@-"]));
            assert!(
                stdin
                    .windows(assertion_head.len())
                    .any(|w| w == assertion_head.as_bytes())
            );
        } else {
            assert!(s.args.windows(2).any(|w| w == ["-H", "@-"]), "{:?}", s.args);
            assert_eq!(stdin, bearer.as_bytes(), "the header line, on stdin alone");
        }
        for scrubbed in ["CURL_HOME", "XDG_CONFIG_HOME", "GH_TOKEN", "GITHUB_TOKEN"] {
            assert!(
                s.env.iter().any(|(k, v)| k == scrubbed && v.is_none()),
                "{scrubbed} is removed"
            );
        }
    }
    assert!(
        !log.contains(TOKEN) && !log.contains(&assertion_head),
        "{log}"
    );
    let seen = lab.fake.state().seen.clone();
    let authorised = |s: &super::fake::Seen| {
        s.headers
            .iter()
            .any(|(k, v)| k == "authorization" && *v == format!("Bearer {TOKEN}"))
    };
    assert!(!authorised(&seen[0]), "the token request carries no bearer");
    assert!(
        seen[1..].iter().all(authorised),
        "every Hosting call reached the fake with the bearer curl read from stdin"
    );
    assert!(
        spawned.iter().any(|s| !s.files.is_empty()),
        "the request bodies were in the scratch directory the scan reads"
    );
    std::fs::write(lab.dir.path().join("scratch/planted"), TOKEN).unwrap();
    let planted = Spawned {
        files: files_under(&lab.dir.path().join("scratch")),
        ..spawned[1].clone()
    };
    assert_eq!(
        exposed(&planted, TOKEN.as_bytes()).len(),
        1,
        "the scan finds a file"
    );
    let argv = Spawned {
        args: vec![format!("-HAuthorization: Bearer {TOKEN}")],
        ..spawned[1].clone()
    };
    assert_eq!(exposed(&argv, TOKEN.as_bytes()).len(), 1, "and an argument");
}

/// A key whose `token_uri` is not the admitted endpoint is refused before curl starts: the
/// assertion is never signed for, or sent to, anywhere else. Every refused URI is on `.invalid`
/// or the loopback fake, so a regression that admits one still reaches no real host. (That
/// Google's own endpoint is the one production admits is `rest`'s
/// `only_googles_token_endpoint_is_admitted`, which starts nothing.) NEGATIVE CONTROL: the
/// admitted endpoint exchanges it.
#[test]
fn a_token_uri_off_the_admitted_endpoint_is_refused() {
    let lab = Lab::new();
    let fake_token = lab.fake.origins.token.clone();
    for off in [
        "https://oauth2.googleapis.com.attacker.invalid/token".to_string(),
        "https://token-endpoint.invalid/token".to_string(),
        format!("{fake_token}/"),
        fake_token.replace("/token", "/elsewhere"),
    ] {
        let (ended, _) = lab.deploy(&lab.key(&off));
        match ended {
            Err(Failed::Credential(why)) => assert!(why.contains("token_uri"), "{why}"),
            other => panic!("{off}: {other:?}"),
        }
    }
    assert!(lab.recorder.spawned().is_empty(), "no curl started");
    assert!(lab.fake.calls().is_empty());
    let (ended, _) = lab.deploy(&lab.key(&fake_token));
    assert!(ended.is_ok());
}

/// An `uploadUrl` off the upload origin is refused before any member goes up: the bearer
/// token would go there. NEGATIVE CONTROL: the fake's own upload URL is taken (the full flow).
#[test]
fn an_upload_url_off_the_upload_origin_is_refused() {
    let lab = Lab::new();
    lab.fake.state().upload_url = Some(format!("{}/elsewhere", lab.fake.origins.api));
    let key = lab.key(&lab.fake.origins.token);
    let (ended, _) = lab.deploy(&key);
    match ended {
        Err(Failed::NotReleased { why, .. }) => assert!(why.contains("upload URL"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(!lab.fake.calls().iter().any(|c| c == "POST upload"));
}

/// Hosting refusing the key (403) is a credential failure, never a broken deploy.
/// NEGATIVE CONTROL: another failing status is not.
#[test]
fn hosting_refusing_the_key_is_a_credential_failure() {
    for (status, credential) in [(403, true), (500, false)] {
        let lab = Lab::new();
        lab.fake.state().faults.insert("create", status);
        let key = lab.key(&lab.fake.origins.token);
        let (ended, _) = lab.deploy(&key);
        assert_eq!(
            matches!(ended, Err(Failed::Credential(_))),
            credential,
            "{status}: {ended:?}"
        );
    }
}

/// Each hosting key the site's firebase.json uses becomes its `ServingConfig` field:
/// headers as maps, `cleanUrls` as it is, `trailingSlash` as `trailingSlashBehavior`.
/// NEGATIVE CONTROL: `trailingSlash: true` is `ADD`, and leaving a key out leaves its field out.
#[test]
fn the_sites_firebase_json_translates_key_by_key() {
    let hosting = config::parse(&fixture_config()).unwrap();
    assert_eq!(hosting.site, None);
    assert_eq!(
        hosting.ignore,
        [
            "firebase.json",
            "**/.*",
            "**/node_modules/**",
            "**/firebase-debug.log",
            "**/firebase-debug.*.log",
            ".firebase/*"
        ]
    );
    let config = hosting.config.as_object().unwrap();
    assert_eq!(
        config.keys().collect::<Vec<_>>(),
        ["cleanUrls", "headers", "trailingSlashBehavior"]
    );
    assert_eq!(config["cleanUrls"], json!(true));
    assert_eq!(config["trailingSlashBehavior"], json!("REMOVE"));
    let headers = config["headers"].as_array().unwrap();
    assert_eq!(headers.len(), 5);
    assert_eq!(
        headers[0],
        json!({"glob": "**", "headers": {
            "Referrer-Policy": "strict-origin-when-cross-origin",
            "X-Content-Type-Options": "nosniff",
        }})
    );
    assert_eq!(
        headers[3],
        json!({"glob": "/assets/term/aterm_wasm.*.@(js|wasm)", "headers": {
            "Cache-Control": "public, max-age=31536000, immutable",
        }})
    );
    let globs: Vec<&str> = headers
        .iter()
        .map(|h| h.get("glob").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(
        globs,
        [
            "**",
            "**/*.html",
            "/assets/**",
            "/assets/term/aterm_wasm.*.@(js|wasm)",
            "/terminal"
        ],
        "in firebase.json's order: a later rule overrides an earlier one"
    );
    let add = config::parse(r#"{"hosting": {"public": "public", "trailingSlash": true}}"#).unwrap();
    assert_eq!(add.config, json!({"trailingSlashBehavior": "ADD"}));
    let bare = config::parse(r#"{"hosting": {"public": "public"}}"#).unwrap();
    assert_eq!(bare.config, json!({}));
}

/// Redirects and rewrites, which the site does not use yet, translate as firebase-tools
/// translates them; a regex pattern stays a regex. NEGATIVE CONTROL: a redirect's default
/// status is 301 and a status Hosting does not take is refused.
#[test]
fn redirects_and_rewrites_translate_as_firebase_tools_does() {
    let hosting = config::parse(
        r#"{"hosting": {"public": "public",
            "redirects": [
                {"source": "/old", "destination": "/new", "type": 302},
                {"regex": "^/r/(\\d+)$", "destination": "/n/:1"},
                {"glob": "/g/**", "destination": "https://example.invalid/", "type": 308}
            ],
            "rewrites": [{"source": "**", "destination": "/index.html"}],
            "headers": [{"regex": "^/x$", "headers": [{"key": "A", "value": "b"}]}]
        }}"#,
    )
    .unwrap();
    assert_eq!(
        hosting.config,
        json!({
            "headers": [{"regex": "^/x$", "headers": {"A": "b"}}],
            "redirects": [
                {"glob": "/old", "location": "/new", "statusCode": 302},
                {"regex": "^/r/(\\d+)$", "location": "/n/:1", "statusCode": 301},
                {"glob": "/g/**", "location": "https://example.invalid/", "statusCode": 308},
            ],
            "rewrites": [{"glob": "**", "path": "/index.html"}],
        })
    );
    let bad = r#"{"hosting": {"public": "public", "redirects": [{"source": "/a", "destination": "/b", "type": 200}]}}"#;
    assert!(config::parse(bad).unwrap_err().contains("type"));
}

/// What this deploy cannot express is refused by name, never dropped. NEGATIVE CONTROL:
/// the fixture parses (the test above).
#[test]
fn what_the_deploy_cannot_express_is_refused_by_name() {
    let cases = [
        (r#"{"hosting": [{"public": "public"}]}"#, "array of sites"),
        (r#"{"hosting": {"public": "dist"}}"#, "hosting.public"),
        (
            r#"{"hosting": {"public": "public", "i18n": {"root": "/x"}}}"#,
            "hosting.i18n",
        ),
        (
            r#"{"hosting": {"public": "public", "appAssociation": "AUTO"}}"#,
            "hosting.appAssociation",
        ),
        (
            r#"{"hosting": {"public": "public", "rewrites": [{"source": "**", "function": "app"}]}}"#,
            "function",
        ),
        (
            r#"{"hosting": {"public": "public", "rewrites": [{"source": "**", "run": {"serviceId": "s"}}]}}"#,
            "run",
        ),
        (
            r#"{"hosting": {"public": "public", "headers": [{"source": "/a", "glob": "/b", "headers": [{"key": "A", "value": "b"}]}]}}"#,
            "more than one pattern",
        ),
        (
            r#"{"hosting": {"public": "public", "headers": [{"source": "/a", "headers": [{"key": "A", "value": "1"}, {"key": "A", "value": "2"}]}]}}"#,
            "twice",
        ),
        (
            r#"{"hosting": {"public": "public", "headers": [{"headers": [{"key": "A", "value": "b"}]}]}}"#,
            "no pattern",
        ),
        (
            r#"{"hosting": {"public": "public", "cleanUrls": "yes"}}"#,
            "cleanUrls",
        ),
        (r#"{}"#, "no hosting"),
        ("not json", "not JSON"),
    ];
    for (text, named) in cases {
        let why = config::parse(text).unwrap_err();
        assert!(why.contains(named), "{text}: {why}");
    }
}

/// The dry run's plan says each file with its size, member size and hash, and the site and
/// config it would deploy. NEGATIVE CONTROL: an overlay changes the planned bytes and hash.
#[test]
fn the_plan_says_every_file_and_an_overlay_changes_it() {
    let lab = Lab::new();
    let plan = lab.plan();
    let mut out = Vec::new();
    plan.say(&mut out);
    let said = String::from_utf8(out).unwrap();
    assert!(
        said.contains("site fake-site; config cleanUrls, headers, trailingSlashBehavior; 4 files"),
        "{said}"
    );
    for f in &plan.files {
        assert!(
            said.contains(&format!("{} {} bytes", f.path, f.bytes)),
            "{said}"
        );
        assert!(said.contains(&f.hash), "{said}");
    }
    let hosting = config::read(&lab.site()).unwrap();
    let mut overlay = BTreeMap::new();
    overlay.insert("index.html".to_string(), b"<p>planned</p>".to_vec());
    let planned = Plan::of(&lab.site(), &hosting, None, &overlay).unwrap();
    let index = |p: &Plan| {
        p.files
            .iter()
            .find(|f| f.path == "/index.html")
            .unwrap()
            .clone()
    };
    assert_eq!(index(&planned).bytes, 14);
    assert_ne!(index(&planned).hash, index(&plan).hash);
    assert!(
        lab.fake.calls().is_empty(),
        "a plan asks nothing of Hosting"
    );
}

/// A token endpoint that answers a 2xx other than 200 is not trusted, and its body, which holds
/// the token, is never said: the failure names the status alone, in the error and in the log.
/// NEGATIVE CONTROL: the generic answer-sayer would have quoted the body's token, and the same
/// lab answering 200 deploys.
#[test]
fn a_token_answer_is_never_quoted_in_a_failure() {
    let lab = Lab::new();
    lab.fake.state().token_status = Some(201);
    let key = lab.key(&lab.fake.origins.token);
    let (ended, log) = lab.deploy(&key);
    let failed = ended.unwrap_err();
    match &failed {
        Failed::NotReleased { why, version: None } => {
            assert!(why.contains("HTTP 201"), "{why}");
            assert!(!why.contains(TOKEN), "{why}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!failed.to_string().contains(TOKEN), "{failed}");
    assert!(!log.contains(TOKEN), "{log}");
    assert_eq!(
        lab.fake.calls(),
        ["POST /token"],
        "nothing asked of Hosting"
    );
    let quoted = rest::Answer {
        status: 201,
        body: format!(r#"{{"access_token":"{TOKEN}","expires_in":3599}}"#).into_bytes(),
    };
    assert!(quoted.said().contains(TOKEN), "{}", quoted.said());
    lab.fake.state().token_status = None;
    let (ended, _) = lab.deploy(&key);
    assert!(ended.is_ok(), "{ended:?}");
}

/// A `releases.create` whose curl never started leaves the live site as it was and says so
/// (not "unknown"): the finalized version is named as unreleased, with the rerun's state.
/// NEGATIVE CONTROL: a release curl that started and then failed, and a release Hosting
/// answered with a failure, are both "unknown" ([`Failed::Release`]).
#[test]
fn a_release_that_never_went_out_is_known_unreleased() {
    let lab = Lab::new();
    lab.fake.state().live = Some("sites/fake-site/versions/v0".into());
    *lab.recorder.refuse.lock().unwrap() = Some(("/releases", true));
    let key = lab.key(&lab.fake.origins.token);
    let (ended, log) = lab.deploy(&key);
    match ended.unwrap_err() {
        failed @ Failed::NotReleased { .. } => {
            let said = failed.to_string();
            assert!(
                said.starts_with("releasing sites/fake-site/versions/v1: cannot run curl")
                    && said.contains("the live site still serves its previous release")
                    && said.contains("sites/fake-site/versions/v1 was left unreleased"),
                "{said}"
            );
        }
        other => panic!("{other:?}\n{log}"),
    }
    assert!(!lab.fake.calls().iter().any(|c| c.contains("/releases")));
    assert_eq!(
        lab.fake.state().live.as_deref(),
        Some("sites/fake-site/versions/v0")
    );

    *lab.recorder.refuse.lock().unwrap() = Some(("/releases", false));
    let (ended, _) = lab.deploy(&key);
    assert!(
        matches!(&ended, Err(Failed::Release { why, .. }) if why.contains("curl did not finish")),
        "{ended:?}"
    );
    *lab.recorder.refuse.lock().unwrap() = None;
    lab.fake.state().faults.insert("release", 500);
    let (ended, _) = lab.deploy(&key);
    assert!(
        matches!(&ended, Err(Failed::Release { why, .. }) if why.contains("HTTP 500")),
        "{ended:?}"
    );
}

/// `/usr/bin/gzip` with `args` over `input`, its output.
fn os_gzip(args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut gzip = aterm_uds::spawnfd::command("/usr/bin/gzip");
    gzip.args(args)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped());
    let mut child = gzip.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || std::io::Write::write_all(&mut stdin, &input));
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert!(out.status.success(), "gzip {args:?}");
    out.stdout
}

/// Each member Hosting stores, and serves as it is to a client that takes gzip but not
/// brotli (and as every 404's body), is as small as zlib's level 9 (the OS's `gzip -9n`) to
/// within 3% over the fixture site's files and pages, and the OS's gzip reads each back to
/// its file. NEGATIVE CONTROL: zlib's fastest level misses the same bound, so a weaker
/// deflate would fail it.
#[test]
fn each_member_is_as_small_as_zlib_level_9() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site");
    let mut inputs: Vec<Vec<u8>> = files_under(&fixtures.join("public"))
        .into_iter()
        .map(|(_, bytes)| bytes)
        .collect();
    for page in ["CHANGELOG.md", "releases.golden.html", "releases-fat.json"] {
        inputs.push(std::fs::read(fixtures.join(page)).unwrap());
    }
    assert!(inputs.len() >= 10, "{}", inputs.len());
    let (mut ours, mut best, mut fast) = (0, 0, 0);
    for bytes in &inputs {
        let member = aterm_png::gzip(bytes);
        assert_eq!(&os_gzip(&["-dc"], &member), bytes, "the OS's gzip reads it");
        ours += member.len();
        best += os_gzip(&["-9nc"], bytes).len();
        fast += os_gzip(&["-1nc"], bytes).len();
    }
    assert!(
        ours * 100 <= best * 103,
        "{ours} bytes against gzip -9n's {best}"
    );
    assert!(
        fast * 100 > best * 103,
        "gzip -1n's {fast} against -9n's {best}"
    );
}
