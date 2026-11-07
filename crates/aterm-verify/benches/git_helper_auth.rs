// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual whole-operation HTTP-loopback measurements using the real agh helper.
//! Build both this benchmark and agh with `--profile opt-test`; pass the agh executable.
//! These ref-only fixtures implement neither pack upload nor atomic append nor TLS.

#[cfg(all(unix, debug_assertions))]
#[path = "support/git_provenance.rs"]
mod git_provenance;

#[cfg(all(unix, debug_assertions))]
mod native {
    use agh::config::{self, Dirs};
    use agh::git::{GitOut, GitRun, HelperCommand};
    use agh::identity::MapEnv;
    use agh::policy::Router;
    use agh::wire::Endpoints;
    use agh::{Fx, Github, GithubBuilder};
    use agh_fake::{FakeGithub, FakeOrigin, RepoSpec};
    use aterm_json::json;
    use std::error::Error;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Instant;

    const SLUG: &str = "fixture-dev/app";
    const SHA: &str = "1234567890123456789012345678901234567890";

    #[derive(Clone, Copy)]
    enum Operation {
        AnonymousRead,
        PrivateRead,
        WriterDelete,
    }

    impl Operation {
        fn name(self) -> &'static str {
            match self {
                Self::AnonymousRead => "anonymous_read",
                Self::PrivateRead => "private_read",
                Self::WriterDelete => "private_writer_delete",
            }
        }

        fn profile(self) -> Option<&'static str> {
            match self {
                Self::AnonymousRead => None,
                Self::PrivateRead => Some("reader"),
                Self::WriterDelete => Some("dev"),
            }
        }

        fn login(self) -> Option<&'static str> {
            match self {
                Self::AnonymousRead => None,
                Self::PrivateRead => Some("fixture-reader"),
                Self::WriterDelete => Some("fixture-dev-bot"),
            }
        }

        fn deletes(self) -> bool {
            matches!(self, Self::WriterDelete)
        }
    }

    struct Fixture {
        client: Github,
        fake: FakeGithub,
        dirs: Dirs,
        _root: aterm_tempfile::TempDir,
    }

    impl Fixture {
        fn new(helper: &Path, op: Operation, expired: bool) -> Result<Self, Box<dyn Error>> {
            let root = aterm_tempfile::Builder::new()
                .prefix("git-helper-auth-")
                .suffix(".noindex")
                .tempdir()?;
            let base = root.path();
            let dirs = Dirs::under(base);
            let fake = FakeGithub::start();
            let repo = if matches!(op, Operation::AnonymousRead) {
                RepoSpec::public()
            } else {
                RepoSpec::private()
                    .push("fixture-dev-bot")
                    .pull("fixture-reader")
            };
            fake.add_repo(SLUG, repo);
            fake.set_branch(SLUG, "main", SHA);
            fake.set_tag(SLUG, "v1", SHA);
            let tokens = base.join("tokens");
            aterm_uds::private_file::create_private_dir(&tokens)?;
            for (profile, login) in [("reader", "fixture-reader"), ("dev", "fixture-dev-bot")] {
                let token = agh_fake::token(profile);
                fake.add_token(&token, login);
                aterm_uds::private_file::write_private(
                    &tokens,
                    &format!("{profile}.token"),
                    format!("{token}\n").as_bytes(),
                )?;
            }
            let escaped = base
                .to_str()
                .ok_or("non-UTF8 fixture path")?
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            let read = if op.profile().is_none() {
                "\"anonymous\""
            } else {
                "\"reader\", \"dev\""
            };
            let text = format!(
                r#"[profiles.reader]
token_file = "{escaped}/tokens/reader.token"
tier = "dev"
login = "fixture-reader"
[profiles.dev]
token_file = "{escaped}/tokens/dev.token"
tier = "dev"
login = "fixture-dev-bot"
[[owners]]
name = "fixture-dev"
tier = "dev"
read = [{read}]
write = "dev"
[default]
read = ["anonymous"]
"#
            );
            std::fs::create_dir_all(dirs.config.parent().ok_or("config has no parent")?)?;
            aterm_uds::private_file::create_private_dir(&dirs.config)?;
            aterm_uds::private_file::write_private(&dirs.config, "agh.toml", text.as_bytes())?;
            let parsed = config::parse(&text).map_err(|e| format!("fixture config: {e:?}"))?;
            let validated = config::validate(&parsed, base, &config::file_id)
                .map_err(|e| format!("fixture config: {e:?}"))?;
            let env = ["PATH", "USER", "LOGNAME"]
                .into_iter()
                .fold(MapEnv::new(), |env, key| match std::env::var(key) {
                    Ok(value) => env.with(key, &value),
                    Err(_) => env,
                });
            let ports = fake.ports();
            let start = if expired { 1 } else { Fx::live().wall_secs()? };
            let client = GithubBuilder::new(Router::from_validated(validated))
                .transport(Arc::new(agh::wire::http::HttpTransport::direct(Arc::new(
                    env.clone(),
                ))))
                .endpoints(Endpoints::loopback(
                    ports.api,
                    ports.uploads,
                    ports.web,
                    ports.cdn,
                ))
                .env(Arc::new(env))
                .fx(Arc::new(Fx::simulated(start)))
                .caller("git-helper-auth-benchmark")
                .retry(agh::retry::RetryPolicy::NONE)
                .journal(dirs.clone())
                .git_helper_for_tests(HelperCommand {
                    exe: helper.to_owned(),
                    args: Vec::new(),
                })
                .build();
            Ok(Self {
                client,
                fake,
                dirs,
                _root: root,
            })
        }

        fn run(&self, op: Operation) -> agh::Result<GitOut> {
            let slug = agh::Slug::parse(SLUG)?;
            let request = if op.deletes() {
                GitRun::push_delete(&slug, "refs/tags/v1", SHA)
            } else {
                GitRun::ls_remote(&slug, None)
            };
            self.client.run_git(&request)
        }

        // Only publish allowlisted structural evidence; never raw headers, env, tokens or grants.
        fn evidence(
            &self,
            op: Operation,
            result: &agh::Result<GitOut>,
            expired: bool,
        ) -> Result<(aterm_json::Value, bool), Box<dyn Error>> {
            let requests: Vec<_> = self
                .fake
                .seen()
                .into_iter()
                .filter(|s| {
                    s.origin == FakeOrigin::Web
                        && s.path_and_query.starts_with("/fixture-dev/app.git/")
                })
                .collect();
            let rows: Vec<_> = requests.iter().map(|s| json!({
                "method":s.method,
                "endpoint":if s.path_and_query.contains("git-receive-pack") {"receive_pack"} else {"upload_pack"},
                "authorization_present":s.had_authorization,
                "identity":match s.as_login.as_deref() {
                    Some("fixture-reader") => "reader",
                    Some("fixture-dev-bot") => "writer",
                    None => "anonymous",
                    Some(_) => "unexpected",
                }
            })).collect();
            let grant_files = match std::fs::read_dir(self.dirs.grants()) {
                Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e.into()),
            };
            // The runner resolves Git independently of fixture_git. Query the actual
            // recorded executable under its exact environment, outside the timer.
            let (actual_git, git_valid) = crate::git_provenance::recorded_git(&self.client)?;
            let refs = self.fake.git_refs(SLUG);
            let main_stable = refs
                .iter()
                .any(|(name, oid)| name == "refs/heads/main" && oid == SHA);
            let tag_present = refs
                .iter()
                .any(|(name, oid)| name == "refs/tags/v1" && oid == SHA);
            let first_anonymous = requests.first().is_some_and(|s| !s.had_authorization);
            let identity_ok = requests.iter().all(|s| {
                (!s.had_authorization && s.as_login.is_none())
                    || (s.had_authorization
                        && s.as_login.as_deref()
                            == if expired {
                                Some("fixture-reader")
                            } else {
                                op.login()
                            }
                        && (expired || op.login().is_some()))
            });
            // Rule 5 routes an expired dev grant through the read chain. Its reader
            // cannot authorize receive-pack; the expired writer must never answer.
            let expected_authenticated = expired || op.login().is_some();
            let authenticated = requests.iter().any(|s| s.had_authorization);
            let receive_post = requests.iter().any(|s| {
                s.method == "POST"
                    && s.path_and_query.ends_with("/git-receive-pack")
                    && s.had_authorization
            });
            let result_ok = if expired {
                result
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.kind == agh::Kind::Unauthenticated)
            } else {
                result.as_ref().is_ok_and(|out| {
                    out.as_.as_deref() == op.profile()
                        && out.grant.is_some() == op.deletes()
                        && (op.deletes()
                            || (out.stdout.contains(SHA)
                                && out.stdout.contains("refs/heads/main")
                                && out.stdout.contains("refs/tags/v1")))
                })
            };
            let valid = git_valid
                && result_ok
                && first_anonymous
                && identity_ok
                && authenticated == expected_authenticated
                && grant_files == 0
                && main_stable
                && refs.len() == if !expired && op.deletes() { 1 } else { 2 }
                && tag_present == (expired || !op.deletes())
                && receive_post == (!expired && op.deletes());
            Ok((
                json!({"requests":rows,"first_request_anonymous":first_anonymous,"actual_git":actual_git,
                "grant_files_after":grant_files,"main_stable":main_stable,"tag_present":tag_present,
                "result_matches_expected":result_ok,"valid":valid}),
                valid,
            ))
        }
    }

    fn emit(label: &str, row: aterm_json::Value) -> Result<(), Box<dyn Error>> {
        println!("{label}: {row}");
        std::io::stdout().flush()?;
        Ok(())
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() != 1 {
            return Err("usage: git_helper_auth AGH_HELPER".into());
        }
        let helper: PathBuf = std::fs::canonicalize(&args[0])?;
        let version = aterm_uds::fixture_git::git().arg("--version").output()?;
        if !version.status.success() {
            return Err("fixture git --version failed".into());
        }
        emit(
            "git_helper_auth_metadata",
            json!({"fixture_git_version":String::from_utf8(version.stdout)?.trim(),
            "transport":"HTTP loopback", "blocks":6,"calls":18,"warmups":0,"retries":0,
            "fixture":"fresh server, config, tokens, client and refs per call; synthetic ref advertisements only",
            "timing":"whole run_git operation; setup and evidence validation excluded"}),
        )?;
        let control = Fixture::new(&helper, Operation::WriterDelete, true)?;
        let outcome = control.run(Operation::WriterDelete);
        let evidence = control.evidence(Operation::WriterDelete, &outcome, true);
        // Record the attempt even if evidence collection or the expected refusal failed.
        emit(
            "git_helper_auth_control",
            json!({"control":"expired_writer_grant", "run_succeeded":outcome.is_ok(),
            "evidence":evidence.as_ref().ok().map(|(row,_)| row)}),
        )?;
        if !evidence?.1 {
            return Err("expired writer grant control failed".into());
        }
        drop(control);
        use Operation::{AnonymousRead as A, PrivateRead as R, WriterDelete as W};
        for (block, order) in [
            [A, R, W],
            [A, W, R],
            [R, A, W],
            [R, W, A],
            [W, A, R],
            [W, R, A],
        ]
        .into_iter()
        .enumerate()
        {
            for (position, op) in order.into_iter().enumerate() {
                let fixture = Fixture::new(&helper, op, false)?;
                let began = Instant::now();
                let outcome = fixture.run(op);
                let wall = began.elapsed();
                let evidence = fixture.evidence(op, &outcome, false);
                emit(
                    "git_helper_auth_sample",
                    json!({"block":block,"position":position,"operation":op.name(),
                    "wall_ns":wall.as_nanos(),"run_succeeded":outcome.is_ok(),
                    "evidence":evidence.as_ref().ok().map(|(row,_)| row)}),
                )?;
                if !evidence?.1 {
                    return Err("sample validation failed; attempted row retained".into());
                }
            }
        }
        emit(
            "git_helper_auth_probe",
            json!({"complete":true,"calls":18,"valid":true}),
        )?;
        Ok(())
    }
}

fn main() {
    #[cfg(all(unix, debug_assertions))]
    if let Err(error) = native::run() {
        eprintln!("git_helper_auth: {error}");
        std::process::exit(1);
    }
    #[cfg(not(all(unix, debug_assertions)))]
    {
        eprintln!("git_helper_auth requires unix and debug assertions; use --profile opt-test");
        std::process::exit(2);
    }
}
