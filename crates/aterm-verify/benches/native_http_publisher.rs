// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual native publisher measurement using real loopback HTTP Git services.
//! Build this and agh with --profile opt-test; pass the built agh executable.
//! Optional second argument: children, for accepted Git-door timing and nested accounting.
//! With children, pair=0 through pair=5 selects one pair for interleaved artifact comparisons.
//! The interval includes permit construction, journal opening and push_atomic.

#[cfg(all(unix, debug_assertions))]
#[path = "support/git_provenance.rs"]
mod git_provenance;
#[cfg(all(unix, debug_assertions))]
#[path = "support/journal_fixture.rs"]
mod journal_fixture;

#[cfg(all(unix, debug_assertions))]
#[path = "support/git_children.rs"]
mod git_children;

#[cfg(all(unix, debug_assertions))]
mod native {
    use crate::journal_fixture::{
        git, object_kind, path, phase_observation, retained_history, same_tree_child,
    };
    use agh::config::{self, Dirs};
    use agh::fx::{GitChildTiming, NativePushTiming};
    use agh::git::{GitRun, HelperCommand};
    use agh::github::refs::{AtomicRefBatch, RefUpdate};
    use agh::identity::MapEnv;
    use agh::journal::IntentStore as _;
    use agh::policy::{RouteRequest, RouteTarget, Router};
    use agh::wire::Endpoints;
    use agh::{Fx, Github, GithubBuilder, Kind};
    use agh_fake::git_backend::{GitHttpBackend, Identity, Service};
    use agh_fake::{FakeGithub, RepoSpec};
    use aterm_json::json;
    use std::error::Error;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Instant;

    const SLUG: &str = "fixture-dev/app";
    const REFS: [&str; 2] = ["refs/heads/main", "refs/aterm/judged"];
    type Probe = Arc<Mutex<Vec<NativePushTiming>>>;

    struct Case {
        name: &'static str,
        before: String,
        after: String,
    }

    struct Fixture {
        client: Github,
        backend: GitHttpBackend,
        fake: FakeGithub,
        dirs: Dirs,
        source: PathBuf,
        batch: AtomicRefBatch,
        before_objects: String,
        spans: Probe,
        children: Arc<Mutex<Vec<GitChildTiming>>>,
        _root: aterm_tempfile::TempDir,
    }

    impl Fixture {
        fn new(
            helper: &Path,
            source: &Path,
            seed: &Path,
            case: &Case,
            observe_children: bool,
        ) -> Result<Self, Box<dyn Error>> {
            let root = aterm_tempfile::Builder::new()
                .prefix("native-http-call-")
                .suffix(".noindex")
                .tempdir()?;
            let base = root.path();
            let dirs = Dirs::under(base);
            let reader = agh_fake::token("reader");
            let writer = agh_fake::token("dev");
            let backend = GitHttpBackend::start(SLUG, seed, &reader, &writer)?;
            for reference in REFS {
                git(
                    backend.repository(),
                    &["update-ref", reference, &case.before],
                )?;
            }
            assert_eq!(
                git(
                    backend.repository(),
                    &["for-each-ref", "--format=%(refname)"]
                )?
                .lines()
                .count(),
                2
            );
            assert_eq!(
                object_kind(backend.repository(), &case.after)?,
                format!("{} missing", case.after)
            );
            let before_objects = git(backend.repository(), &["count-objects", "-v"])?;
            assert_eq!(before_objects, git(seed, &["count-objects", "-v"])?);
            let fake = FakeGithub::start();
            fake.add_token(&reader, "fixture-reader");
            fake.add_token(&writer, "fixture-dev-bot");
            fake.add_repo(
                SLUG,
                RepoSpec::private()
                    .push("fixture-dev-bot")
                    .pull("fixture-reader"),
            );
            fake.set_branch(SLUG, "main", &case.before);
            let tokens = base.join("tokens");
            aterm_uds::private_file::create_private_dir(&tokens)?;
            for (name, token) in [("reader.token", reader), ("dev.token", writer)] {
                aterm_uds::private_file::write_private(
                    &tokens,
                    name,
                    format!("{token}\n").as_bytes(),
                )?;
            }
            let escaped = path(base)?.replace('\\', "\\\\").replace('"', "\\\"");
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
read = ["reader", "dev"]
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
                .fold(MapEnv::new(), |env, key| {
                    std::env::var(key).map_or(env.clone(), |value| env.with(key, &value))
                });
            let spans = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&spans);
            let fx = Fx::simulated(Fx::live().wall_secs()?).with_native_push_observer_for_tests(
                move |span| {
                    captured
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(span);
                },
            );
            let children = Arc::new(Mutex::new(Vec::new()));
            let fx = if observe_children {
                let captured = Arc::clone(&children);
                fx.with_git_child_observer_for_tests(move |span| {
                    captured
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(span);
                })
            } else {
                fx
            };
            let ports = fake.ports();
            let client = GithubBuilder::new(Router::from_validated(validated))
                .transport(Arc::new(agh::wire::http::HttpTransport::direct(Arc::new(
                    env.clone(),
                ))))
                .endpoints(Endpoints::loopback(
                    ports.api,
                    ports.uploads,
                    backend.port(),
                    ports.cdn,
                ))
                .env(Arc::new(env))
                .fx(Arc::new(fx))
                .caller("native-http-publisher-benchmark")
                .retry(agh::retry::RetryPolicy::NONE)
                .journal(dirs.clone())
                .git_helper_for_tests(HelperCommand {
                    exe: helper.to_owned(),
                    args: Vec::new(),
                })
                .build();
            let batch = AtomicRefBatch::new(
                REFS.into_iter()
                    .map(|reference| RefUpdate {
                        reference: reference.into(),
                        before: case.before.clone(),
                        after: case.after.clone(),
                    })
                    .collect(),
            )?;
            Ok(Self {
                client,
                backend,
                fake,
                dirs,
                source: source.to_owned(),
                batch,
                before_objects,
                spans,
                children,
                _root: root,
            })
        }

        fn publish(&self) -> agh::Result<()> {
            let slug = agh::Slug::parse(SLUG)?;
            let request = RouteRequest::write(RouteTarget::Repo(slug.clone()));
            let permit = self
                .client
                .router()
                .permit(&request, &|profile| self.client.presence(profile))?;
            let journal = self.client.journal(&slug)?;
            let ensured = self
                .client
                .refs(&slug)
                .push_atomic(&permit, &self.source, &self.batch, &journal)?
                .into_result()?;
            if ensured.effect != agh::write::Effect::Created {
                return Err(agh::Error::new(
                    Kind::CouldNotRun,
                    "fixture did not acknowledge a new publication",
                ));
            }
            Ok(())
        }

        fn open_intents(&self) -> agh::Result<Vec<agh::journal::Intent>> {
            self.client.journal(&agh::Slug::parse(SLUG)?)?.open()
        }

        fn evidence(
            &self,
            case: &Case,
            expected_after: bool,
            unknown: bool,
        ) -> Result<(aterm_json::Value, bool), Box<dyn Error>> {
            let (actual_git, git_valid) = crate::git_provenance::recorded_git(&self.client)?;
            let refs = git(self.backend.repository(), &["show-ref"])?;
            let expected = if expected_after {
                &case.after
            } else {
                &case.before
            };
            let exact_refs = REFS.iter().all(|reference| {
                refs.lines()
                    .any(|line| line == format!("{expected} {reference}"))
            }) && refs.lines().count() == 2;
            let object_exact = if expected_after {
                Some(
                    git(
                        self.backend.repository(),
                        &["cat-file", "commit", &case.after],
                    )? == git(&self.source, &["cat-file", "commit", &case.after])?,
                )
            } else {
                None
            };
            let grants = match std::fs::read_dir(self.dirs.grants()) {
                Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e.into()),
            };
            let intents = self.open_intents()?.len();
            let requests = self.backend.seen();
            let writer_only = requests
                .iter()
                .all(|r| !r.backend_attempted || r.identity == Identity::Writer);
            let challenge = requests.iter().any(|r| {
                r.identity == Identity::Anonymous && r.status == 401 && !r.backend_attempted
            });
            let posts = requests
                .iter()
                .filter(|r| {
                    r.post
                        && r.service == Some(Service::Receive)
                        && r.backend_attempted
                        && r.identity == Identity::Writer
                })
                .count();
            let dropped = requests.iter().filter(|r| r.response_dropped).count();
            let rows = request_rows(&requests);
            let api = self.fake.seen();
            let api_writer = !api.is_empty()
                && api
                    .iter()
                    .all(|r| r.as_login.as_deref() == Some("fixture-dev-bot"));
            let valid = git_valid
                && exact_refs
                && object_exact != Some(false)
                && grants == 0
                && intents == usize::from(unknown)
                && writer_only
                && challenge
                && posts == 1
                && dropped == usize::from(unknown)
                && api_writer;
            Ok((
                json!({"actual_git":actual_git,"backend_git_version":self.backend.git_version(),"exact_final_refs":exact_refs,
                "new_commit_bytes_exact":object_exact,"initial_remote_objects":self.before_objects,"grant_files_after":grants,"open_intents":intents,
                "writer_only_backend":writer_only,"anonymous_challenge":challenge,"receive_posts":posts,
                "dropped_responses":dropped,"api_requests":api.len(),"api_writer_only":api_writer,"requests":rows,
                "accepted_request_bound":"Content-Length <=16MiB, enforced by backend parser; no chunked framing",
                "valid":valid}),
                valid,
            ))
        }
    }

    fn request_rows(requests: &[agh_fake::git_backend::Observation]) -> aterm_json::Value {
        let rows: Vec<_> = requests.iter().map(|r| json!({"post":r.post,"service":match r.service {Some(Service::Upload)=>"upload",Some(Service::Receive)=>"receive",None=>"other"},"identity":match r.identity {Identity::Anonymous=>"anonymous",Identity::Reader=>"reader",Identity::Writer=>"writer",Identity::Unknown=>"unknown"},"status":r.status,"backend_attempted":r.backend_attempted,"response_dropped":r.response_dropped})).collect();
        json!(rows)
    }

    fn emit(label: &str, value: aterm_json::Value) -> Result<(), Box<dyn Error>> {
        println!("native_http_publisher_{label}: {value}");
        std::io::stdout().flush()?;
        Ok(())
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let args: Vec<_> = std::env::args_os()
            .skip(1)
            .filter(|a| a != "--bench")
            .collect();
        const USAGE: &str = "usage: native_http_publisher AGH_HELPER [children [pair=0..5]]";
        if args.len() == 1 && args[0] == "--help" {
            println!("{USAGE}");
            return Ok(());
        }
        let observe_children = args.get(1).is_some_and(|arg| arg == "children");
        let only_pair = args
            .get(2)
            .and_then(|arg| arg.to_str())
            .and_then(|arg| arg.strip_prefix("pair="))
            .and_then(|pair| pair.parse::<usize>().ok())
            .filter(|pair| *pair < 6);
        if !(args.len() == 1
            || (args.len() == 2 && observe_children)
            || (args.len() == 3 && observe_children && only_pair.is_some()))
        {
            return Err(USAGE.into());
        }
        let pairs = if only_pair.is_some() { 1 } else { 6 };
        let helper = std::fs::canonicalize(&args[0])?;
        let root = aterm_tempfile::Builder::new()
            .prefix("native-http-study-")
            .suffix(".noindex")
            .tempdir()?;
        let source = root.path().join("source");
        std::fs::create_dir(&source)?;
        git(&source, &["init", "--quiet", "-b", "journal"])?;
        retained_history(&source)?;
        let tiny_before = git(&source, &["rev-parse", "journal"])?;
        let retained_before = git(&source, &["rev-parse", "retained"])?;
        // Copy the entire identical BEFORE store before creating either new object.
        let seed = root.path().join("seed.git");
        git(
            root.path(),
            &[
                "clone",
                "--quiet",
                "--mirror",
                "--local",
                path(&source)?,
                path(&seed)?,
            ],
        )?;
        for reference in ["refs/heads/journal", "refs/heads/retained"] {
            git(&seed, &["update-ref", "-d", reference])?;
        }
        for reference in REFS {
            git(&seed, &["update-ref", reference, &tiny_before])?;
        }
        git(&seed, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
        let tiny_after = same_tree_child(&source, &tiny_before)?;
        let retained_after = same_tree_child(&source, &retained_before)?;
        git(&source, &["update-ref", "refs/heads/journal", &tiny_after])?;
        git(
            &source,
            &["update-ref", "refs/heads/retained", &retained_after],
        )?;
        let cases = [
            Case {
                name: "tiny",
                before: tiny_before,
                after: tiny_after,
            },
            Case {
                name: "retained",
                before: retained_before,
                after: retained_after,
            },
        ];
        let tree = git(
            &source,
            &["rev-parse", &format!("{}^{{tree}}", cases[0].after)],
        )?;
        assert_eq!(
            tree,
            git(
                &source,
                &["rev-parse", &format!("{}^{{tree}}", cases[1].after)]
            )?
        );
        assert_eq!(
            git(&source, &["rev-list", "--count", &cases[0].after])?,
            "2"
        );
        assert_eq!(
            git(&source, &["rev-list", "--count", &cases[1].after])?,
            "2573"
        );
        let entries = git(&source, &["ls-tree", "-r", "--name-only", &cases[0].after])?;
        assert_eq!(entries.lines().count(), 73);
        assert_eq!(entries.lines().filter(|p| p.ends_with(".toml")).count(), 72);
        let bytes = git(&source, &["cat-file", "-s", &cases[0].after])?;
        assert_eq!(bytes, git(&source, &["cat-file", "-s", &cases[1].after])?);
        for case in &cases {
            assert_eq!(
                object_kind(&seed, &case.after)?,
                format!("{} missing", case.after)
            );
        }
        let source_refs = git(&source, &["show-ref"])?;
        let source_config = std::fs::read(source.join(".git/config"))?;
        let source_objects = git(&source, &["count-objects", "-v"])?;
        let seed_refs = git(&seed, &["show-ref"])?;
        let seed_config = std::fs::read(seed.join("config"))?;
        let seed_objects = git(&seed, &["count-objects", "-v"])?;
        emit(
            "metadata",
            json!({"child_observation":observe_children,"calls":pairs*2,"pairs":pairs,"selected_pair":only_pair,"cases":cases.iter().map(|c|json!({"case":c.name,"before":c.before,"after":c.after})).collect::<Vec<_>>(),"updates_per_call":2,"new_objects_each":1,"new_commit_bytes_each":bytes.parse::<u64>()?,
            "same_current_tree":true,"current_tree":tree,"current_tree_entries":73,"current_toml_paths":72,
            "tiny_commits_after":2,"retained_commits_after":2573,"tiny_closure_objects":git(&source,&["rev-list","--objects",&cases[0].after])?.lines().count(),
            "retained_closure_objects":git(&source,&["rev-list","--objects",&cases[1].after])?.lines().count(),
            "source_objects":source_objects,"seed_objects":seed_objects,"fixture_git_version":git(&source,&["--version"])?,
            "controls":3,"warmups":0,"metric_retries":0,"boundary":"permit construction + journal opening + push_atomic; fresh client/backend/API/config setup and evidence excluded",
            "scope":"synthetic private HTTP loopback/CGI publisher, not app watchdog/TLS/WAN/GitHub latency"}),
        )?;

        for control in ["known", "refused", "unknown"] {
            let fixture = Fixture::new(&helper, &source, &seed, &cases[0], observe_children)?;
            if control == "refused" {
                std::fs::write(
                    fixture.backend.repository().join("refs/heads/main.lock"),
                    b"owned fixture lock\n",
                )?;
            }
            if control == "unknown" {
                fixture.backend.lose_next_receive_response();
            }
            let result = fixture.publish();
            let evidence = fixture.evidence(&cases[0], control != "refused", control == "unknown");
            let expected = match control {
                "known" => result.is_ok(),
                "refused" => result.as_ref().is_err_and(|e| e.kind == Kind::Refused),
                _ => result
                    .as_ref()
                    .is_err_and(|e| e.kind == Kind::OutcomeUnknown),
            };
            emit(
                "control",
                json!({"control":control,"result_expected":expected,"evidence":evidence.as_ref().ok().map(|(v,_)|v)}),
            )?;
            if !expected || !evidence?.1 {
                return Err("untimed publisher control failed; attempt retained".into());
            }
            if control == "unknown" {
                let journal = fixture.client.journal(&agh::Slug::parse(SLUG)?)?;
                let intent = fixture.open_intents()?[0].id.clone();
                let posts = fixture
                    .backend
                    .seen()
                    .iter()
                    .filter(|r| r.post && r.service == Some(Service::Receive))
                    .count();
                let retry = fixture.publish();
                let retry_blocked = retry
                    .as_ref()
                    .is_err_and(|e| e.kind == Kind::OutcomeUnknown);
                let discovered = fixture
                    .client
                    .run_git(&GitRun::ls_remote(&agh::Slug::parse(SLUG)?, None))?;
                let actual: std::collections::BTreeMap<_, _> = discovered
                    .stdout
                    .lines()
                    .filter_map(|line| line.split_once('\t').map(|(oid, name)| (name, oid)))
                    .collect();
                let all_after =
                    fixture.batch.updates().iter().all(|u| {
                        actual.get(u.reference.as_str()).copied() == Some(u.after.as_str())
                    });
                let no_replay = posts
                    == fixture
                        .backend
                        .seen()
                        .iter()
                        .filter(|r| r.post && r.service == Some(Service::Receive))
                        .count();
                let reader = discovered.as_.as_deref() == Some("reader")
                    && fixture.backend.seen().iter().any(|r| {
                        r.service == Some(Service::Upload)
                            && r.identity == Identity::Reader
                            && r.backend_attempted
                    });
                if all_after && no_replay && retry_blocked && reader {
                    journal.observe(&intent, Some(0))?;
                }
                let valid =
                    all_after && no_replay && retry_blocked && reader && journal.open()?.is_empty();
                emit(
                    "reconciliation",
                    json!({"all_after":all_after,"no_receive_replay":no_replay,"open_intents_after":journal.open()?.len(),"reader":reader,"retry_blocked":retry_blocked,"requests":request_rows(&fixture.backend.seen()),"valid":valid}),
                )?;
                if !valid {
                    return Err("unknown reconciliation failed".into());
                }
            }
        }
        for pair in (0..6).filter(|pair| only_pair.is_none_or(|only| only == *pair)) {
            let order = if pair % 2 == 0 { [0, 1] } else { [1, 0] };
            for (position, index) in order.into_iter().enumerate() {
                let case = &cases[index];
                let fixture = Fixture::new(&helper, &source, &seed, case, observe_children)?;
                let began = Instant::now();
                let result = fixture.publish();
                let finished = Instant::now();
                let spans = fixture.spans.lock().unwrap_or_else(PoisonError::into_inner);
                let (phases, phases_valid) = phase_observation(&spans, began, finished);
                let (children, children_valid) = if observe_children {
                    let children = fixture
                        .children
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    crate::git_children::observation(
                        &fixture.client,
                        &children,
                        &spans,
                        began,
                        finished,
                    )
                } else {
                    (json!(null), true)
                };

                drop(spans);
                let evidence = fixture.evidence(case, true, false);
                emit(
                    "sample",
                    json!({"pair":pair,"position":position,"case":case.name,"wall_ns":finished.duration_since(began).as_nanos(),
                    "ok":result.is_ok(),"error_kind":result.as_ref().err().map(|e|format!("{:?}",e.kind)),"native_phases":phases,"phases_valid":phases_valid,"git_children":children,"children_valid":children_valid,
                    "evidence":evidence.as_ref().ok().map(|(v,_)|v)}),
                )?;
                if result.is_err() || !phases_valid || !children_valid || !evidence?.1 {
                    return Err("publisher sample failed; attempted row retained".into());
                }
            }
        }
        let unchanged = source_refs == git(&source, &["show-ref"])?
            && source_config == std::fs::read(source.join(".git/config"))?
            && source_objects == git(&source, &["count-objects", "-v"])?
            && seed_refs == git(&seed, &["show-ref"])?
            && seed_config == std::fs::read(seed.join("config"))?
            && seed_objects == git(&seed, &["count-objects", "-v"])?;
        emit(
            "probe",
            json!({"source_and_seed_refs_configs_inventories_unchanged":unchanged,"complete":true,"samples":pairs*2}),
        )?;
        if !unchanged {
            return Err("source/seed mutated".into());
        }
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
        Err("requires Unix with debug assertions: use --profile opt-test".into())
    }
}
