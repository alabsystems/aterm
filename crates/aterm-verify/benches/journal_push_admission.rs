// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual native admission measurement. Build this and the agh helper with `--profile opt-test`.
//! Arguments: the source checkout, built agh helper and optional `history` or
//! `history-connectivity`, `history-phases` or `retained-phases` comparison.
//! All remotes and tokens are fixtures.

#[cfg(all(unix, debug_assertions))]
#[path = "support/journal_fixture.rs"]
mod journal_fixture;

#[cfg(all(unix, debug_assertions))]
mod native {
    use crate::journal_fixture::{
        git, object_kind, path, phase_observation, retained_history, same_tree_child,
    };
    use agh::config::{self, Dirs};
    use agh::fx::NativePushTiming;
    use agh::git::HelperCommand;
    use agh::identity::MapEnv;
    use agh::policy::{RouteRequest, RouteTarget, Router};
    use agh::wire::Endpoints;
    use agh::{Github, GithubBuilder};
    use agh_fake::{FakeGithub, RepoSpec};
    use aterm_json::json;
    use std::error::Error;
    use std::ffi::OsString;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    const REF: &str = "refs/aterm/judged";
    const HISTORY_REF: &str = "refs/aterm/history";
    const SLUG: &str = "fixture-dev/app";

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

    struct ConnectivityCommand {
        program: PathBuf,
        args: Vec<OsString>,
    }

    #[derive(Default)]
    struct ConnectivitySpan {
        began: Option<Instant>,
        wall: Option<Duration>,
        child_success: bool,
        calls: usize,
    }

    type ConnectivityProbe = Arc<Mutex<ConnectivitySpan>>;
    type PhaseProbe = Arc<Mutex<Vec<NativePushTiming>>>;

    impl Fixture {
        fn client(&self) -> Result<Github, Box<dyn Error>> {
            self.client_with_fx(agh::Fx::simulated(agh::Fx::live().wall_secs()?))
        }

        fn phase_client(&self) -> Result<(Github, PhaseProbe), Box<dyn Error>> {
            let spans = Arc::new(Mutex::new(Vec::new()));
            let observed = Arc::clone(&spans);
            let fx = agh::Fx::simulated(agh::Fx::live().wall_secs()?)
                .with_native_push_observer_for_tests(move |span| {
                    observed
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(span);
                });
            Ok((self.client_with_fx(fx)?, spans))
        }

        fn client_with_fx(&self, fx: agh::Fx) -> Result<Github, Box<dyn Error>> {
            let env = ["PATH", "USER", "LOGNAME"]
                .into_iter()
                .fold(MapEnv::new(), |env, key| {
                    std::env::var(key).map_or(env.clone(), |value| env.with(key, &value))
                });
            let ports = self.fake.ports();
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
                .fx(Arc::new(fx))
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

        fn connectivity_client(
            &self,
            command: &ConnectivityCommand,
            calibration_after: &str,
            after: &str,
        ) -> Result<(Github, ConnectivityProbe), Box<dyn Error>> {
            let mut args = command.args.clone();
            let mut replacements = 0;
            for arg in &mut args {
                if arg.as_os_str() == std::ffi::OsStr::new(calibration_after) {
                    *arg = after.into();
                    replacements += 1;
                }
            }
            assert_eq!(replacements, 1, "one exact connectivity root is replaced");
            let span = Arc::new(Mutex::new(ConnectivitySpan::default()));
            let before = Arc::clone(&span);
            let after = Arc::clone(&span);
            let fx = agh::Fx::simulated(agh::Fx::live().wall_secs()?)
                .with_git_intercept_once_for_tests(
                    command.program.clone(),
                    args,
                    move || {
                        let mut span = before.lock().unwrap_or_else(PoisonError::into_inner);
                        span.calls += 1;
                        span.began = Some(Instant::now());
                    },
                    move |result| {
                        let mut span = after.lock().unwrap_or_else(PoisonError::into_inner);
                        if let Some(began) = span.began.take() {
                            span.wall = Some(began.elapsed());
                        }
                        span.child_success = result.as_ref().is_ok_and(|out| out.code == Some(0));
                    },
                );
            Ok((self.client_with_fx(fx)?, span))
        }
    }

    pub(super) fn run() -> Result<(), Box<dyn Error>> {
        let args: Vec<_> = std::env::args_os()
            .skip(1)
            .filter(|a| a != "--bench")
            .collect();
        if !(args.len() == 2
            || (args.len() == 3
                && (args[2] == "history"
                    || args[2] == "history-connectivity"
                    || args[2] == "history-phases"
                    || args[2] == "retained-phases")))
        {
            return Err("usage: journal_push_admission SOURCE_CHECKOUT AGH_HELPER [history|history-connectivity|history-phases|retained-phases] (build both with --profile opt-test)".into());
        }
        let history = args.len() == 3;
        let connectivity = args.len() == 3 && args[2] == "history-connectivity";
        let retained = args.len() == 3 && args[2] == "retained-phases";
        let observe_phases = args.len() == 3 && (args[2] == "history-phases" || retained);
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
        if retained {
            retained_history(&work)?;
        } else {
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
        }
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
            let history_before = if retained {
                let before = git(&work, &["rev-parse", "retained"])?;
                fetch(&remote, &work, &format!("{before}:{HISTORY_REF}"))?;
                fetch(&full, &work, &format!("{before}:{HISTORY_REF}"))?;
                before
            } else {
                source_head.clone()
            };
            let child = same_tree_child(&full, &history_before)?;
            assert_eq!(
                git(&full, &["cat-file", "-s", &child])?,
                git(&full, &["cat-file", "-s", &after])?,
                "both new commit payloads have the same byte length"
            );
            git(&full, &["update-ref", HISTORY_REF, &child])?;
            git(&remote, &["update-ref", HISTORY_REF, &history_before])?;
            assert_eq!(object_kind(&remote, &child)?, format!("{child} missing"));
            Some(agh::github::refs::AtomicRefBatch::new(vec![
                agh::github::refs::RefUpdate {
                    reference: HISTORY_REF.into(),
                    before: history_before,
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
        let full_objects = git(&full, &["count-objects", "-v"])?;
        let journal_objects = git(&journal, &["count-objects", "-v"])?;
        let remote_objects = git(&remote, &["count-objects", "-v"])?;
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
        let retained_shape = if retained {
            let history_after = &history_batch
                .as_ref()
                .ok_or("retained batch missing")?
                .updates()[0]
                .after;
            let tree = git(&full, &["rev-parse", &format!("{after}^{{tree}}")])?;
            assert_eq!(
                tree,
                git(&full, &["rev-parse", &format!("{history_after}^{{tree}}")])?
            );
            let entries = git(&full, &["ls-tree", "-r", "--name-only", &after])?;
            assert_eq!(entries.lines().count(), 73);
            assert_eq!(entries.lines().filter(|p| p.ends_with(".toml")).count(), 72);
            assert_eq!(git(&full, &["rev-list", "--count", &after])?, "2");
            assert_eq!(git(&full, &["rev-list", "--count", history_after])?, "2573");
            json!({"synthetic":true,"current_tree":tree,"current_tree_entries":73,"current_toml_paths":72,"tiny_commits":2,"retained_commits":2573,"tiny_closure_objects":git(&full,&["rev-list","--objects",&after])?.lines().count(),"retained_closure_objects":git(&full,&["rev-list","--objects",history_after])?.lines().count(),"same_current_tree":true,"new_objects_each":1,"new_commit_bytes_each":git(&full,&["cat-file","-s",&after])?.parse::<u64>()?})
        } else {
            json!(null)
        };
        let comparison = if retained {
            "retained_phases"
        } else if observe_phases {
            "history_phases"
        } else if connectivity {
            "history_connectivity"
        } else if history {
            "history"
        } else {
            "inventory"
        };
        let subjects = if let Some(history_batch) = &history_batch {
            [
                (
                    if retained { "tiny_journal" } else { "journal" },
                    &full,
                    &batch,
                ),
                (
                    if retained {
                        "retained_journal"
                    } else {
                        "source_history"
                    },
                    &full,
                    history_batch,
                ),
            ]
        } else {
            [("full", &full, &batch), ("journal", &journal, &batch)]
        };
        let connectivity_command = if connectivity {
            let client = fixture.client()?;
            let permit = client
                .router()
                .permit(
                    &RouteRequest::write(RouteTarget::Repo(slug.clone())),
                    &|_| agh::policy::Presence::Present,
                )
                .map_err(|e| format!("fixture permit: {e:?}"))?;
            client.refs(&slug).check_push(&permit, &full, &batch)?;
            // Keep only this fixed local command, never publish a ChildSpec or its environment.
            let commands = client.fx().children_run();
            let selected: Vec<_> = commands
                .iter()
                .filter(|command| command.args.iter().any(|arg| arg == "--connectivity-only"))
                .collect();
            assert_eq!(selected.len(), 1, "one connectivity child in calibration");
            println!("journal_admission_calibration: one_untimed_tiny_journal_call");
            Some(ConnectivityCommand {
                program: selected[0].program.clone(),
                args: selected[0].args.clone(),
            })
        } else {
            None
        };
        println!(
            "journal_admission_metadata: {}",
            json!({"comparison":comparison,"git_version":git(&full,&["--version"])?,"source_head":source_head,"before":before,"after":after,"journal_records":if retained {72} else {64},"journal_commits":git(&full,&["rev-list","--count",&after])?,"history":history_metadata,"retained_shape":retained_shape,"connectivity_calibration_calls":usize::from(connectivity),"full_store":full_objects,"journal_store":journal_objects,"remote_store":remote_objects,"fixture_setup_maintenance_disabled":true,"profile":"opt-test: opt3, debug assertions, no LTO,16CGU","clock":"real Instant wall, not SDK simulated clock","scope":"isolated loopback/local-remote fixtures; dry-run only; source checkout is read-only input; no operator GitHub/vault write or installed-release latency claim"})
        );
        let reused = fixture.client()?;
        let phases: &[&str] = if connectivity || observe_phases {
            &["fresh_client"]
        } else {
            &["fresh_client", "reused_client"]
        };
        let samples = if connectivity || observe_phases { 6 } else { 3 };
        for &phase in phases {
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
            for sample in 0..samples {
                let order = if sample % 2 == 0 {
                    subjects
                } else {
                    [subjects[1], subjects[0]]
                };
                for (position, (variant, store, batch)) in order.into_iter().enumerate() {
                    let (fresh, probe, phase_probe) = if observe_phases {
                        let (client, spans) = fixture.phase_client()?;
                        (Some(client), None, Some(spans))
                    } else if let Some(command) = &connectivity_command {
                        let (client, span) = fixture.connectivity_client(
                            command,
                            &after,
                            &batch.updates()[0].after,
                        )?;
                        (Some(client), Some(span), None)
                    } else if phase == "fresh_client" {
                        (Some(fixture.client()?), None, None)
                    } else {
                        (None, None, None)
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
                    let finished = Instant::now();
                    let elapsed = finished.duration_since(began).as_nanos();
                    let (phase_spans, phases_valid) = if let Some(spans) = phase_probe {
                        let spans = spans.lock().unwrap_or_else(PoisonError::into_inner);
                        phase_observation(&spans, began, finished)
                    } else {
                        (json!(null), true)
                    };
                    let (connectivity_span, probe_valid) = if let Some(span) = probe {
                        let span = span.lock().unwrap_or_else(PoisonError::into_inner);
                        let wall = span.wall.map(|d| d.as_nanos());
                        let outside = wall.and_then(|wall| elapsed.checked_sub(wall));
                        let valid = span.calls == 1 && outside.is_some() && span.child_success;
                        (
                            json!({"step":"connectivity","wall_ns":wall,"outside_child_ns":outside,"child_success":span.child_success,"calls":span.calls}),
                            valid,
                        )
                    } else {
                        (json!(null), true)
                    };
                    println!(
                        "journal_admission_sample: {}",
                        json!({"comparison":comparison,"phase":phase,"sample":sample,"position":position,"variant":variant,"wall_ns":elapsed,"connectivity":connectivity_span,"native_phases":phase_spans,"ok":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{:?}: {}",e.kind,e.why))})
                    );
                    result?;
                    assert!(
                        phases_valid,
                        "three ordered successful real phases are contained by the whole call"
                    );
                    assert!(
                        probe_valid,
                        "one completed real child span is contained by the whole call"
                    );
                }
            }
        }
        assert_eq!(git(&full, &["show-ref"])?, full_refs);
        assert_eq!(git(&journal, &["show-ref"])?, journal_refs);
        assert_eq!(git(&fixture.remote, &["show-ref"])?, remote_refs);
        assert_eq!(std::fs::read(full.join("config"))?, full_config);
        assert_eq!(std::fs::read(journal.join("config"))?, journal_config);
        assert_eq!(std::fs::read(fixture.remote.join("config"))?, remote_config);
        assert_eq!(git(&full, &["count-objects", "-v"])?, full_objects);
        assert_eq!(git(&journal, &["count-objects", "-v"])?, journal_objects);
        assert_eq!(
            git(&fixture.remote, &["count-objects", "-v"])?,
            remote_objects
        );
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
