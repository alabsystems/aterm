// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_git_credential_never_debugs_or_passes_its_secret_in_argv() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let file = dir.path().join("token");
    let secret = "github_pat_redaction_fixture";
    std::fs::write(&file, secret).expect("fixture credential");
    let token = Token::from_file(&file)
        .expect("a readable credential")
        .expect("a present credential");
    let auth = GitAuth::new(Some(token), "https://github.com");
    assert!(!format!("{auth:?}").contains(secret));
    assert!(
        !auth
            .arguments()
            .iter()
            .any(|arg| arg.to_string_lossy().contains(secret))
    );
    assert_eq!(
        auth.environment().expect("credential environment"),
        vec![(GIT_TOKEN_ENV.into(), secret.into())]
    );
}

#[test]
fn a_name_is_one_segment_and_a_credential_is_scoped_to_an_http_origin() {
    assert_eq!(segment("pkg/v1 a&b"), "pkg%2Fv1%20a%26b");
    assert_eq!(segment("atpkg-index-7.tar.zst"), "atpkg-index-7.tar.zst");
    assert_eq!(
        credential_origin("https://github.com"),
        Some("https://github.com")
    );
    assert_eq!(
        credential_origin("http://127.0.0.1:8080"),
        Some("http://127.0.0.1:8080")
    );
    assert!(credential_origin("file:///tmp/remotes").is_none());
    assert!(credential_origin("https://github.com/x").is_none());
    for bad in ["", ".", "..", "../x", "a/b", "x.atpkg-part"] {
        assert!(!plain_name(bad), "{bad:?}");
    }
    assert!(plain_name("a.bin"));
}

fn text(s: &std::ffi::OsStr) -> String {
    s.to_string_lossy().into_owned()
}

fn shows(cmd: &Command, secret: &str) -> Vec<String> {
    let mut found: Vec<String> = cmd
        .get_args()
        .map(text)
        .filter(|a| a.contains(secret))
        .map(|a| format!("argv: {a}"))
        .collect();
    found.extend(
        cmd.get_envs()
            .filter(|(_, v)| v.is_some_and(|v| text(v).contains(secret)))
            .map(|(k, _)| format!("env: {}", text(k))),
    );
    found
}

#[test]
fn gits_token_is_in_its_environment_alone_never_its_argv_or_a_file() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let secret = "github_pat_fixture0123";
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, format!("{secret}\n")).expect("the token file");
    let host = RestHost::at(
        Some(token_file),
        Origins::github(),
        "https://github.com".to_string(),
    );
    let cmd = host
        .git_command(&["fetch".into()], None)
        .expect("the command");
    assert_eq!(shows(&cmd, secret), [format!("env: {GIT_TOKEN_ENV}")]);
    for name in ["GIT_TRACE2", "GIT_TRACE2_EVENT", "GIT_TRACE2_PERF"] {
        assert!(
            cmd.get_envs()
                .any(|(key, value)| key == name && value == Some(std::ffi::OsStr::new("0")))
        );
    }
    let args: Vec<String> = cmd.get_args().map(text).collect();
    assert_eq!(
        args[..2],
        ["-c", "credential.helper="],
        "every helper reset first"
    );
    let helper = format!("credential.https://github.com.helper={}", git_helper());
    assert_eq!(
        args[2..4],
        ["-c".to_string(), helper],
        "then the one, scoped"
    );
    assert_eq!(
        args[4..8],
        crate::git::GIT_TLS_PINS,
        "then agh's TLS pins: verification on, no redirect"
    );
    assert!(git_helper().contains(&format!("\"${GIT_TOKEN_ENV}\"")));
    for gone in ["GITHUB_TOKEN", "GH_TOKEN"] {
        assert!(
            cmd.get_envs().any(|(k, v)| k == gone && v.is_none()),
            "{gone} is removed"
        );
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("scratch").count(),
        1,
        "no file but the token's own"
    );
    let mut planted = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
    planted.env("GIT_PASSWORD", secret);
    planted.arg(format!("https://x:{secret}@github.com/o/r.git"));
    assert_eq!(shows(&planted, secret).len(), 2);
    let local = RestHost::at(
        host.token_file.clone(),
        Origins::github(),
        "file:///r".into(),
    );
    let cmd = local
        .git_command(&["fetch".into()], None)
        .expect("the command");
    assert_eq!(shows(&cmd, secret), Vec::<String>::new());
    assert!(!cmd.get_args().any(|a| text(a).contains("helper=!")));
    assert!(credential_origin("https://github.com.helper=!evil").is_none());
    assert!(credential_origin("https://user@github.com").is_none());
    assert!(
        cmd.get_envs()
            .any(|(k, v)| k == GIT_TOKEN_ENV && v.is_none())
    );

    let anon = RestHost::at(None, Origins::github(), "https://github.com".into());
    assert!(crate::host::ReleaseHost::reads_anonymously(&anon));
    let absent = RestHost::at(
        Some(dir.path().join("missing-token")),
        Origins::github(),
        "https://github.com".into(),
    );
    assert!(crate::host::ReleaseHost::reads_anonymously(&absent));
    assert!(!crate::host::ReleaseHost::reads_anonymously(&host));

    let blank = dir.path().join("blank-token");
    std::fs::write(&blank, "  \n\t\r\n").expect("blank token");
    let blank_host = RestHost::at(Some(blank), Origins::github(), "https://github.com".into());
    assert!(
        crate::host::ReleaseHost::reads_anonymously(&blank_host),
        "whitespace-only token file reads anonymously through CDN probes"
    );

    let corrupt = dir.path().join("corrupt-token");
    std::fs::write(&corrupt, b"ghp_bad\x00token\n").expect("corrupt token");
    let corrupt_host = RestHost::at(
        Some(corrupt),
        Origins::github(),
        "https://github.com".into(),
    );
    assert!(
        !crate::host::ReleaseHost::reads_anonymously(&corrupt_host),
        "corrupt token file must not silently fall back to anonymous reads"
    );
}

/// A network git's TLS posture, read by the real git this host runs as git's HTTP transport
/// reads it (by URL match against the remote it is given, `config --get-urlmatch`), beside a
/// user config that turns verification off and follows every redirect, unscoped and scoped to
/// `https://github.com` and to the repository's exact URL: agh's pins win, so the token this
/// host's helper answers never goes to an unverified or redirected peer. NEGATIVE CONTROL: a
/// git given no remote URL carries the unscoped pins alone, and the scoped keys win there.
#[test]
fn a_network_git_pins_tls_over_a_url_scoped_user_config() {
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
    const URL: &str = "https://github.com/fixture-dev/packs.git";
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let user = dir.path().join("gitconfig");
    let off = "\tsslVerify = false\n\tfollowRedirects = true\n";
    std::fs::write(
        &user,
        format!("[http]\n{off}[http \"https://github.com\"]\n{off}[http \"{URL}\"]\n{off}"),
    )
    .expect("the user's config");
    let host = RestHost::at(None, Origins::github(), "https://github.com".into()).with_git_env(&[
        ("GIT_CONFIG_NOSYSTEM", std::ffi::OsStr::new("1")),
        ("GIT_CONFIG_GLOBAL", user.as_os_str()),
        ("XDG_CONFIG_HOME", std::ffi::OsStr::new("/dev/null")),
    ]);
    let read = |url: Option<&str>, key: &str| {
        let args = ["config", "--get-urlmatch", key, URL].map(std::ffi::OsString::from);
        let mut git = host.git_command(&args, url).expect("the command");
        git.current_dir(dir.path());
        // A hang detector, not a budget: git reads its config at once.
        let out =
            aterm_uds::deadline::output_until(&mut git, std::time::Instant::now() + HANG_DETECTOR)
                .expect("git config");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout)
            .expect("UTF-8 value")
            .trim_end()
            .to_string()
    };
    assert_eq!(read(Some(URL), "http.sslVerify"), "true");
    assert_eq!(read(Some(URL), "http.followRedirects"), "false");
    assert_eq!(read(None, "http.sslVerify"), "false");
    assert_eq!(read(None, "http.followRedirects"), "true");
}

/// The `-c` values on the command line of the one top-level git whose Trace2 events `trace`
/// holds (the session id with no parent's), as its `start` event records it.
fn command_line_config(trace: &Path) -> Vec<String> {
    let events = std::fs::read_to_string(trace).expect("git's Trace2 events");
    let started: Vec<Vec<String>> = events
        .lines()
        .filter_map(|line| {
            let event: Value = aterm_json::from_str(line).expect("a Trace2 event");
            let top = event["sid"].as_str().is_some_and(|sid| !sid.contains('/'));
            (top && event["event"].as_str() == Some("start")).then(|| {
                event["argv"]
                    .as_array()
                    .expect("its command line")
                    .iter()
                    .map(|word| word.as_str().expect("a UTF-8 word").to_string())
                    .collect()
            })
        })
        .collect();
    let [argv] = started.as_slice() else {
        panic!("one top-level git: {started:?}")
    };
    argv[1..]
        .chunks(2)
        .take_while(|pair| pair.len() == 2 && pair[0] == "-c")
        .map(|pair| pair[1].clone())
        .collect()
}

/// Each call site that hands a network git its remote passes that URL on to `git_command`
/// (`git_attempt` under the anonymous policy, `git_network`'s and `clone_repo`'s credentialed
/// branches): the git that runs, as its own Trace2 `start` event records its command line,
/// carries agh's pins at `git_url(slug)` among its `-c` pairs, on the anonymous and the
/// credentialed path, for a clone and for a fetch. The user's config rewrites
/// `https://github.com/` to a local mirror and `GIT_ALLOW_PROTOCOL=file` admits no other
/// transport, so nothing reaches the network. What the pins win over is
/// [`a_network_git_pins_tls_over_a_url_scoped_user_config`]'s. NEGATIVE CONTROLS: any of the
/// three call sites handing `git_command` no URL fails its cases.
#[test]
fn every_network_git_carries_the_pins_at_the_url_it_is_given() {
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let fixture = |cwd: &Path, args: &[&str]| {
        let mut git = crate::testkit::git();
        git.current_dir(cwd)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args);
        // A hang detector, not a budget: each is one local repository write.
        let out =
            aterm_uds::deadline::output_until(&mut git, std::time::Instant::now() + HANG_DETECTOR)
                .expect("fixture git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    let work = dir.path().join("work");
    std::fs::create_dir(&work).expect("the work tree");
    fixture(&work, &["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(work.join("README"), "fixture\n").expect("a file");
    fixture(&work, &["add", "README"]);
    fixture(&work, &["commit", "--quiet", "-m", "fixture"]);
    let remotes = dir.path().join("remotes");
    let bare = remotes.join("fixture-dev/packs.git");
    let utf8 = |path: &Path| path.to_str().expect("a UTF-8 scratch").to_string();
    fixture(
        dir.path(),
        &["clone", "--quiet", "--bare", &utf8(&work), &utf8(&bare)],
    );
    let user = dir.path().join("gitconfig");
    std::fs::write(
        &user,
        format!(
            "[url \"file://{}/\"]\n\tinsteadOf = https://github.com/\n",
            remotes.display()
        ),
    )
    .expect("the user's config");
    let token = dir.path().join("token");
    std::fs::write(&token, "github_pat_fixture\n").expect("the token file");
    let slug = Slug::new("fixture-dev", "packs");
    for token_file in [None, Some(token)] {
        let path = if token_file.is_some() {
            "credentialed"
        } else {
            "anonymous"
        };
        let dest = dir.path().join(format!("{path}-clone"));
        for op in ["clone", "fetch"] {
            let trace = dir.path().join(format!("{path}-{op}.trace2"));
            let host = RestHost::at(
                token_file.clone(),
                Origins::github(),
                "https://github.com".into(),
            )
            .with_git_env(&[
                ("GIT_CONFIG_NOSYSTEM", std::ffi::OsStr::new("1")),
                ("GIT_CONFIG_GLOBAL", user.as_os_str()),
                ("XDG_CONFIG_HOME", std::ffi::OsStr::new("/dev/null")),
                ("GIT_ALLOW_PROTOCOL", std::ffi::OsStr::new("file")),
                ("GIT_TRACE2_EVENT", trace.as_os_str()),
            ]);
            let url = host.git_url(&slug);
            assert_eq!(
                host.canonical_anonymous_git(&url).expect("the policy"),
                token_file.is_none(),
                "{path}"
            );
            let done = if op == "clone" {
                host.clone_repo(&slug, &dest)
            } else {
                host.fetch_repo(&slug, &dest)
            };
            done.unwrap_or_else(|e| panic!("{path} {op}: {e}"));
            let config = command_line_config(&trace);
            for pin in [
                format!("http.{url}.sslVerify=true"),
                format!("http.{url}.followRedirects=false"),
            ] {
                assert!(config.contains(&pin), "{path} {op}: {config:?}");
            }
        }
    }
}

fn git_status(status: u16) -> aterm_update_core::git_https::Failure {
    aterm_update_core::git_https::Failure::diagnostic(format!(
        "fatal: unable to access 'https://github.com/o/r.git/': The requested URL returned error: {status}"
    ))
}

struct ScriptedGit {
    answers: std::sync::Mutex<
        std::collections::VecDeque<std::result::Result<(), aterm_update_core::git_https::Failure>>,
    >,
    calls: std::sync::Mutex<Vec<Vec<OsString>>>,
    race_dest: Option<PathBuf>,
}

impl ScriptedGit {
    fn new(
        answers: impl IntoIterator<
            Item = std::result::Result<(), aterm_update_core::git_https::Failure>,
        >,
    ) -> Self {
        Self {
            answers: std::sync::Mutex::new(answers.into_iter().collect()),
            calls: std::sync::Mutex::new(Vec::new()),
            race_dest: None,
        }
    }

    fn racing(
        answers: impl IntoIterator<
            Item = std::result::Result<(), aterm_update_core::git_https::Failure>,
        >,
        race_dest: PathBuf,
    ) -> Self {
        Self {
            race_dest: Some(race_dest),
            ..Self::new(answers)
        }
    }

    fn calls(&self) -> Vec<Vec<OsString>> {
        self.calls.lock().expect("script calls").clone()
    }
}

impl GitTransport for ScriptedGit {
    fn run(
        &self,
        args: &[OsString],
        trace: aterm_update_core::git_https::Trace<'_>,
        waiting_on: &str,
    ) -> std::result::Result<(), aterm_update_core::git_https::Failure> {
        assert!(trace.path().is_some(), "the anonymous policy owns a trace");
        assert!(
            waiting_on.starts_with("anonymous GitHub"),
            "the progress owner names the read: {waiting_on}"
        );
        let attempt = {
            let mut calls = self.calls.lock().expect("script calls");
            calls.push(args.to_vec());
            calls.len()
        };
        let answer = self
            .answers
            .lock()
            .expect("script answers")
            .pop_front()
            .expect("the RestHost made an extra Git attempt");
        if args.iter().any(|arg| arg == std::ffi::OsStr::new("clone")) {
            let attempt_dest = PathBuf::from(args.last().expect("clone destination"));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode =
                    std::fs::metadata(attempt_dest.parent().expect("the private clone stage"))
                        .expect("clone stage metadata")
                        .permissions()
                        .mode();
                assert_eq!(mode & 0o077, 0, "the retry stage is private");
            }
            std::fs::create_dir(&attempt_dest).expect("the clone attempt directory");
            std::fs::write(
                attempt_dest.join("transport-marker"),
                if answer.is_ok() {
                    format!("complete-{attempt}\n")
                } else {
                    format!("partial-{attempt}\n")
                },
            )
            .expect("the clone attempt marker");
            if answer.is_ok()
                && let Some(dest) = &self.race_dest
            {
                std::fs::create_dir(dest).expect("the racing caller-owned destination");
                std::fs::write(dest.join("owner-marker"), "caller-owned\n")
                    .expect("the caller-owned marker");
            }
        }
        answer
    }
}

fn scripted_host(script: std::sync::Arc<ScriptedGit>) -> RestHost {
    let mut host = RestHost::at(None, Origins::github(), "https://github.com".into());
    host.git_transport = Some(script);
    host
}

#[test]
fn only_tokenless_canonical_github_git_selects_the_anonymous_policy() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let url = "https://github.com/o/r.git";
    let anonymous = RestHost::at(None, Origins::github(), "https://github.com".into());
    assert!(anonymous.canonical_anonymous_git(url).unwrap());
    assert!(
        !anonymous
            .canonical_anonymous_git("https://elsewhere.example/o/r.git")
            .unwrap()
    );

    let missing = RestHost::at(
        Some(dir.path().join("missing")),
        Origins::github(),
        "https://github.com".into(),
    );
    assert!(missing.canonical_anonymous_git(url).unwrap());
    let blank = dir.path().join("blank");
    std::fs::write(&blank, " \n\t").expect("blank token");
    let blank = RestHost::at(Some(blank), Origins::github(), "https://github.com".into());
    assert!(blank.canonical_anonymous_git(url).unwrap());

    let token = dir.path().join("token");
    std::fs::write(&token, "github_pat_fixture\n").expect("token");
    let credentialed = RestHost::at(Some(token), Origins::github(), "https://github.com".into());
    assert!(!credentialed.canonical_anonymous_git(url).unwrap());
    let custom = RestHost::at(None, Origins::github(), "https://mirror.example".into());
    assert!(!custom.canonical_anonymous_git(url).unwrap());
    let file = RestHost::at(None, Origins::github(), "file:///repos".into());
    assert!(
        !file
            .canonical_anonymous_git("file:///repos/o/r.git")
            .unwrap()
    );
}

#[test]
fn anonymous_clone_retries_504_504_200_in_fresh_private_stages() {
    let script = std::sync::Arc::new(ScriptedGit::new([
        Err(git_status(504)),
        Err(git_status(504)),
        Ok(()),
    ]));
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let dest = dir.path().join("clone");
    scripted_host(std::sync::Arc::clone(&script))
        .clone_repo(&Slug::new("o", "r"), &dest)
        .expect("the third attempt succeeds");
    assert_eq!(
        std::fs::read_to_string(dest.join("transport-marker")).unwrap(),
        "complete-3\n"
    );
    let calls = script.calls();
    assert_eq!(calls.len(), 3);
    let attempts: Vec<PathBuf> = calls
        .iter()
        .map(|args| PathBuf::from(args.last().expect("clone destination")))
        .collect();
    assert!(attempts.iter().all(|attempt| attempt != &dest));
    assert!(attempts.iter().all(|attempt| {
        attempt
            .parent()
            .and_then(Path::parent)
            .is_some_and(|parent| parent == dir.path())
    }));
    assert!(attempts.windows(2).all(|pair| pair[0] != pair[1]));
    assert!(
        attempts.iter().all(|attempt| !attempt.exists()),
        "failed private stages were removed and the complete one moved"
    );
}

#[test]
fn anonymous_fetch_keeps_404_final_bounds_endless_5xx_and_retries_curl_56() {
    let one_404 = std::sync::Arc::new(ScriptedGit::new([Err(git_status(404)), Ok(())]));
    let absent = scripted_host(std::sync::Arc::clone(&one_404))
        .fetch_repo(&Slug::new("o", "r"), Path::new("unused"));
    assert!(
        matches!(absent, Err(HostError::Unreachable(_))),
        "{absent:?}"
    );
    assert_eq!(one_404.calls().len(), 1, "404 is an immediate answer");

    let endless = std::sync::Arc::new(ScriptedGit::new(
        (0..Policy::STANDARD.attempts()).map(|_| Err(git_status(503))),
    ));
    let refused = scripted_host(std::sync::Arc::clone(&endless))
        .fetch_repo(&Slug::new("o", "r"), Path::new("unused"))
        .expect_err("the shared bound refuses an endless 5xx");
    assert_eq!(endless.calls().len() as u32, Policy::STANDARD.attempts());
    assert!(
        refused.to_string().contains("19 attempts / 200s"),
        "{refused}"
    );

    let curl_56 = std::sync::Arc::new(ScriptedGit::new([
        Err(aterm_update_core::git_https::Failure::diagnostic(
            "error: RPC failed; curl 56 Recv failure: Connection reset by peer",
        )),
        Ok(()),
    ]));
    scripted_host(std::sync::Arc::clone(&curl_56))
        .fetch_repo(&Slug::new("o", "r"), Path::new("unused"))
        .expect("curl 56 is transient");
    assert_eq!(curl_56.calls().len(), 2);
}

#[test]
fn a_racing_clone_destination_is_never_replaced_or_deleted() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let dest = dir.path().join("clone");
    let script = std::sync::Arc::new(ScriptedGit::racing([Ok(())], dest.clone()));
    let refused = scripted_host(std::sync::Arc::clone(&script))
        .clone_repo(&Slug::new("o", "r"), &dest)
        .expect_err("the caller wins the destination race");
    assert!(matches!(refused, HostError::Refused(_)), "{refused:?}");
    assert_eq!(
        std::fs::read_to_string(dest.join("owner-marker")).unwrap(),
        "caller-owned\n"
    );
    assert!(!dest.join("transport-marker").exists());
    assert_eq!(script.calls().len(), 1);
}
