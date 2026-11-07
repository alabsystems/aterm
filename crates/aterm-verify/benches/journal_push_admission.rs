// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual native admission measurement. Build this and the agh helper with `--profile opt-test`.
//! Arguments: the source checkout, built agh helper and optional `history` comparison.
//! All remotes and tokens are fixtures.

#[cfg(all(unix, debug_assertions))]
mod native {
    use agh::config::{self, Dirs};
    use agh::git::HelperCommand;
    use agh::identity::MapEnv;
    use agh::policy::{RouteRequest, RouteTarget, Router};
    use agh::wire::Endpoints;
    use agh::{Github, GithubBuilder};
    use agh_fake::{FakeGithub, RepoSpec};
    use aterm_json::json;
    use std::error::Error;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const REF: &str = "refs/aterm/judged";
    const HISTORY_REF: &str = "refs/aterm/history";
    const SLUG: &str = "fixture-dev/app";

    fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
        let mut command = aterm_uds::fixture_git::git();
        command
            .current_dir(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid");
        let out = aterm_uds::deadline::output_until_complete_stdout(
            &mut command,
            Instant::now() + Duration::from_secs(60),
        )?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }

    fn path(path: &Path) -> Result<&str, Box<dyn Error>> {
        path.to_str()
            .ok_or_else(|| "fixture path is not UTF-8".into())
    }

    fn mirror(base: &Path, from: &Path, to: &Path) -> Result<(), Box<dyn Error>> {
        git(
            base,
            &[
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.file.allow=always",
                "clone",
                "--quiet",
                "--mirror",
                "--local",
                path(from)?,
                path(to)?,
            ],
        )?;
        Ok(())
    }

    fn object_kind(root: &Path, oid: &str) -> Result<String, Box<dyn Error>> {
        let mut command = aterm_uds::fixture_git::git();
        command
            .current_dir(root)
            .args(["cat-file", "--batch-check=%(objecttype)"]);
        let out = aterm_uds::deadline::output_with_input_until(
            &mut command,
            aterm_uds::secret::Secret::new(format!("{oid}\n").as_bytes()),
            Instant::now() + Duration::from_secs(60),
            1024,
        )?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }

    fn same_tree_child(root: &Path, parent: &str) -> Result<String, Box<dyn Error>> {
        let tree = git(root, &["rev-parse", &format!("{parent}^{{tree}}")])?;
        let child = git(
            root,
            &[
                "commit-tree",
                &tree,
                "-p",
                parent,
                "-m",
                "isolated admission same-tree child",
            ],
        )?;
        assert_eq!(
            git(root, &["rev-parse", &format!("{child}^{{tree}}")])?,
            tree
        );
        assert_eq!(git(root, &["rev-parse", &format!("{child}^")])?, parent);
        assert_eq!(
            git(
                root,
                &["rev-list", "--objects", &child, &format!("^{parent}")]
            )?,
            child,
            "the new closure is exactly one commit, with no new tree or blob"
        );
        Ok(child)
    }

    fn fetch(root: &Path, from: &Path, reference: &str) -> Result<(), Box<dyn Error>> {
        git(
            root,
            &[
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.file.allow=always",
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                "--recurse-submodules=no",
                path(from)?,
                reference,
            ],
        )?;
        Ok(())
    }

    struct Fixture {
        _root: aterm_tempfile::TempDir,
        dirs: Dirs,
        router: Router,
        fake: FakeGithub,
        helper: PathBuf,
        remote: PathBuf,
    }

    impl Fixture {
        fn client(&self) -> Result<Github, Box<dyn Error>> {
            let env = ["PATH", "USER", "LOGNAME"]
                .into_iter()
                .fold(MapEnv::new(), |env, key| {
                    std::env::var(key).map_or(env.clone(), |value| env.with(key, &value))
                });
            let ports = self.fake.ports();
            let start = agh::Fx::live().wall_secs()?;
            Ok(GithubBuilder::new(self.router.clone())
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
                .fx(Arc::new(agh::Fx::simulated(start)))
                .caller("journal-admission-benchmark")
                .retry(agh::retry::RetryPolicy::NONE)
                .journal(self.dirs.clone())
                .git_helper_for_tests(HelperCommand {
                    exe: self.helper.clone(),
                    args: Vec::new(),
                })
                .git_remote_for_tests(SLUG, &self.remote)
                .build())
        }
    }

    pub(super) fn run() -> Result<(), Box<dyn Error>> {
        let args: Vec<_> = std::env::args_os()
            .skip(1)
            .filter(|a| a != "--bench")
            .collect();
        if !(args.len() == 2 || (args.len() == 3 && args[2] == "history")) {
            return Err("usage: journal_push_admission SOURCE_CHECKOUT AGH_HELPER [history] (build both with --profile opt-test)".into());
        }
        let history = args.len() == 3;
        let source = std::fs::canonicalize(&args[0])?;
        let helper = std::fs::canonicalize(&args[1])?;
        let source_head = git(&source, &["rev-parse", "HEAD"])?;
        if !git(&source, &["status", "--porcelain"])?.is_empty() {
            return Err("the source checkout must be committed and clean".into());
        }
        let root = aterm_tempfile::Builder::new()
            .prefix("journal-admission-")
            .suffix(".noindex")
            .tempdir()?;
        let base = root.path();
        let common = git(
            &source,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        if std::fs::metadata(Path::new(&common).join("objects"))?.dev()
            != std::fs::metadata(base)?.dev()
        {
            return Err(
                "source objects and scratch must share a filesystem for local hard links".into(),
            );
        }
        let full = base.join("full.git");
        // Local clone hard-links immutable objects; it does not duplicate a large source store.
        mirror(base, &source, &full)?;
        let work = base.join("journal-work");
        std::fs::create_dir(&work)?;
        git(&work, &["init", "--quiet", "-b", "journal"])?;
        std::fs::create_dir(work.join("verdicts"))?;
        for index in 0..64 {
            std::fs::write(
                work.join(format!("verdicts/{index:040x}.toml")),
                format!(
                    "tree = \"{index:040x}\"\nstate = \"judging\"\nlease_until = 120\n# {}\n",
                    "fixture".repeat(140)
                ),
            )?;
        }
        git(&work, &["add", "."])?;
        git(&work, &["commit", "--quiet", "-m", "journal before"])?;
        let before = git(&work, &["rev-parse", "HEAD"])?;
        let after = if history {
            let child = same_tree_child(&work, &before)?;
            git(&work, &["update-ref", "refs/heads/journal", &child])?;
            child
        } else {
            std::fs::write(
                work.join("verdicts/0000000000000000000000000000000000000000.toml"),
                "tree = \"0000000000000000000000000000000000000000\"\nstate = \"judging\"\nlease_until = 150\n",
            )?;
            git(&work, &["add", "."])?;
            git(&work, &["commit", "--quiet", "-m", "journal after"])?;
            git(&work, &["rev-parse", "HEAD"])?
        };
        let journal = base.join("journal.git");
        git(
            base,
            &[
                "clone",
                "--quiet",
                "--bare",
                "--local",
                path(&work)?,
                path(&journal)?,
            ],
        )?;
        let remote = base.join("remote.git");
        if history {
            // Clone before either fixture AFTER is imported or created: the remote owns
            // source history, but neither new commit. Both cases then use this same remote.
            mirror(base, &full, &remote)?;
        } else {
            git(base, &["init", "--quiet", "--bare", path(&remote)?])?;
        }
        fetch(&remote, &work, &format!("{before}:{REF}"))?;
        fetch(&full, &work, &format!("{after}:{REF}"))?;
        let history_batch = if history {
            let child = same_tree_child(&full, &source_head)?;
            assert_eq!(
                git(&full, &["cat-file", "-s", &child])?,
                git(&full, &["cat-file", "-s", &after])?,
                "both new commit payloads have the same byte length"
            );
            git(&full, &["update-ref", HISTORY_REF, &child])?;
            git(&remote, &["update-ref", HISTORY_REF, &source_head])?;
            assert_eq!(object_kind(&remote, &child)?, format!("{child} missing"));
            Some(agh::github::refs::AtomicRefBatch::new(vec![
                agh::github::refs::RefUpdate {
                    reference: HISTORY_REF.into(),
                    before: source_head.clone(),
                    after: child,
                },
            ])?)
        } else {
            None
        };
        assert_eq!(object_kind(&remote, &after)?, format!("{after} missing"));
        // Both local stores hold the same synthetic journal graph. No store changes during calls.
        assert_eq!(
            git(&full, &["rev-parse", &format!("{after}^{{tree}}")])?,
            git(&journal, &["rev-parse", &format!("{after}^{{tree}}")])?
        );
        assert_eq!(
            git(&full, &["rev-list", "--objects", &after])?,
            git(&journal, &["rev-list", "--objects", &after])?
        );
        assert!(
            !git(&journal, &["rev-list", "--all"])?
                .lines()
                .any(|line| line == source_head)
        );
        let full_refs = git(&full, &["show-ref"])?;
        let journal_refs = git(&journal, &["show-ref"])?;
        let remote_refs = git(&remote, &["show-ref"])?;
        let full_config = std::fs::read(full.join("config"))?;
        let journal_config = std::fs::read(journal.join("config"))?;
        let remote_config = std::fs::read(remote.join("config"))?;
        let fake = FakeGithub::start();
        fake.add_repo(SLUG, RepoSpec::private().push("fixture-dev-bot"));
        fake.set_branch(SLUG, "main", &before);
        let dirs = Dirs::under(base);
        for (folder, name, label, login) in [
            ("tokens", "dev.token", "dev", "fixture-dev-bot"),
            ("secrets", "release.token", "release", "fixture-release-bot"),
        ] {
            let dir = base.join(folder);
            aterm_uds::private_file::create_private_dir(&dir)?;
            let token = agh_fake::token(label);
            fake.add_token(&token, login);
            aterm_uds::private_file::write_private(&dir, name, format!("{token}\n").as_bytes())?;
        }
        let escaped = path(base)?.replace('\\', "\\\\").replace('"', "\\\"");
        let text =
            include_str!("../../agh/tests/fixtures/canonical.toml").replace("{root}", &escaped);
        std::fs::create_dir_all(dirs.config.parent().ok_or("fixture config has no parent")?)?;
        aterm_uds::private_file::create_private_dir(&dirs.config)?;
        aterm_uds::private_file::write_private(&dirs.config, "agh.toml", text.as_bytes())?;
        let parsed = config::parse(&text).map_err(|e| format!("fixture config: {e:?}"))?;
        let validated = config::validate(&parsed, base, &config::file_id)
            .map_err(|e| format!("fixture config: {e:?}"))?;
        let fixture = Fixture {
            dirs,
            router: Router::from_validated(validated),
            fake,
            helper,
            remote,
            _root: root,
        };
        let slug = agh::Slug::parse(SLUG)?;
        let batch = agh::github::refs::AtomicRefBatch::new(vec![agh::github::refs::RefUpdate {
            reference: REF.into(),
            before: before.clone(),
            after: after.clone(),
        }])?;
        let history_metadata = if let Some(batch) = &history_batch {
            let update = &batch.updates()[0];
            let commits = git(&full, &["rev-list", "--count", &update.after])?.parse::<u64>()?;
            assert!(
                commits > 2,
                "source history must be larger than the journal fixture"
            );
            json!({"before":update.before,"after":update.after,"commits":commits,"new_commits":1,"new_objects_each":1,"new_commit_bytes_each":git(&full,&["cat-file","-s",&update.after])?.parse::<u64>()?,"before_after_tree_equal_each":true})
        } else {
            json!(null)
        };
        let comparison = if history { "history" } else { "inventory" };
        let subjects = if let Some(history_batch) = &history_batch {
            [
                ("journal", &full, &batch),
                ("source_history", &full, history_batch),
            ]
        } else {
            [("full", &full, &batch), ("journal", &journal, &batch)]
        };
        println!(
            "journal_admission_metadata: {}",
            json!({"comparison":comparison,"source_head":source_head,"before":before,"after":after,"journal_records":64,"journal_commits":git(&full,&["rev-list","--count",&after])?,"history":history_metadata,"full_store":git(&full,&["count-objects","-v"] )?,"journal_store":git(&journal,&["count-objects","-v"] )?,"profile":"opt-test: opt3, debug assertions, no LTO,16CGU","clock":"real Instant wall, not SDK simulated clock","scope":"isolated loopback/local-remote fixtures; dry-run only; source checkout is read-only input; no operator GitHub/vault write or installed-release latency claim"})
        );
        let reused = fixture.client()?;
        for phase in ["fresh_client", "reused_client"] {
            if phase == "reused_client" {
                let permit = reused
                    .router()
                    .permit(
                        &RouteRequest::write(RouteTarget::Repo(slug.clone())),
                        &|_| agh::policy::Presence::Present,
                    )
                    .map_err(|e| format!("fixture permit: {e:?}"))?;
                for (_, store, batch) in subjects {
                    reused.refs(&slug).check_push(&permit, store, batch)?;
                }
                println!("journal_admission_warmup: both_cases_before_reused_client_samples");
            }
            for sample in 0..3 {
                let order = if sample % 2 == 0 {
                    subjects
                } else {
                    [subjects[1], subjects[0]]
                };
                for (position, (variant, store, batch)) in order.into_iter().enumerate() {
                    let fresh = if phase == "fresh_client" {
                        Some(fixture.client()?)
                    } else {
                        None
                    };
                    let client = fresh.as_ref().unwrap_or(&reused);
                    let permit = client
                        .router()
                        .permit(
                            &RouteRequest::write(RouteTarget::Repo(slug.clone())),
                            &|_| agh::policy::Presence::Present,
                        )
                        .map_err(|e| format!("fixture permit: {e:?}"))?;
                    let began = Instant::now();
                    let result = client.refs(&slug).check_push(&permit, store, batch);
                    let elapsed = began.elapsed().as_nanos();
                    println!(
                        "journal_admission_sample: {}",
                        json!({"comparison":comparison,"phase":phase,"sample":sample,"position":position,"variant":variant,"wall_ns":elapsed,"ok":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{:?}: {}",e.kind,e.why))})
                    );
                    result?;
                }
            }
        }
        assert_eq!(git(&full, &["show-ref"])?, full_refs);
        assert_eq!(git(&journal, &["show-ref"])?, journal_refs);
        assert_eq!(git(&fixture.remote, &["show-ref"])?, remote_refs);
        assert_eq!(std::fs::read(full.join("config"))?, full_config);
        assert_eq!(std::fs::read(journal.join("config"))?, journal_config);
        assert_eq!(std::fs::read(fixture.remote.join("config"))?, remote_config);
        assert_eq!(
            object_kind(&fixture.remote, &after)?,
            format!("{after} missing")
        );
        if let Some(batch) = &history_batch {
            let after = &batch.updates()[0].after;
            assert_eq!(
                object_kind(&fixture.remote, after)?,
                format!("{after} missing")
            );
        }
        assert!(
            !fixture.dirs.journal().exists(),
            "dry runs created no write-intent journal"
        );
        assert_eq!(git(&source, &["rev-parse", "HEAD"])?, source_head);
        println!(
            "journal_admission_probe: equal_graph_remote_refs_source_refs_and_configs_unchanged_no_intents"
        );
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(all(unix, debug_assertions))]
    {
        native::run()
    }
    #[cfg(not(all(unix, debug_assertions)))]
    {
        Err("requires Unix with debug assertions: build and run with --profile opt-test".into())
    }
}
