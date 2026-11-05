// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Real Git, held manager leases, local admission and Producer selection. The
//! artifact payloads are inert format-marker bytes and never execute. A native
//! cat stand-in supplies the original guardian's actual process lifetime.

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::artifact::pack::{HarnessIdentity, Metadata};
use crate::artifact::tools::{HANG, HostTools};
use crate::harness_local::{SelectionError, fs::Session};
use crate::host::{Hosts, fake::FakeHost};

const COMMAND: &str = "claude-alab";

fn git(checkout: &Path, args: &[&str]) -> String {
    let mut command = crate::testkit::git();
    command.arg("-C").arg(checkout).args(args);
    let output = aterm_uds::deadline::output_until(&mut command, Instant::now() + HANG)
        .expect("bounded actual fixture Git");
    assert!(
        output.status.success(),
        "fixture Git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("fixture Git text")
        .trim_end_matches('\n')
        .to_owned()
}

struct World {
    temporary: Option<aterm_tempfile::TempDir>,
    home: PathBuf,
    config: Config,
    ctx: Ctx,
    session: Session,
    passed: bool,
}

impl World {
    fn new() -> Self {
        let temporary = aterm_tempfile::Builder::new()
            .prefix("harness-select-source-")
            .tempdir()
            .expect("private selection world");
        let home = temporary.path().canonicalize().expect("canonical home");
        let config = Config::parse(&[
            "claude-code-alab".into(),
            "--checkout".into(),
            home.join("source").display().to_string(),
            "--out".into(),
            home.join("out").display().to_string(),
            "--prefix".into(),
            home.join("pkg").display().to_string(),
            "--native".into(),
            "--unverified".into(),
        ])
        .expect("explicit local configuration");
        fs::create_dir(&config.checkout).expect("owned source directory");
        git(&config.checkout, &["init", "--initial-branch=main"]);
        git(
            &config.checkout,
            &["config", "user.name", "Selection Fixture"],
        );
        git(
            &config.checkout,
            &["config", "user.email", "selection-fixture@invalid.example"],
        );
        git(&config.checkout, &["config", "commit.gpgsign", "false"]);
        git(
            &config.checkout,
            &["config", "core.excludesFile", "/dev/null"],
        );
        git(
            &config.checkout,
            &[
                "remote",
                "add",
                "origin",
                home.join("unused-local-origin.git").to_str().unwrap(),
            ],
        );
        fs::write(config.checkout.join("source.rs"), b"first actual source\n")
            .expect("source file");
        fs::write(config.checkout.join(".gitignore"), b"ignored.rs\n")
            .expect("tracked ignore policy");
        git(&config.checkout, &["add", "."]);
        git(&config.checkout, &["commit", "-m", "first source"]);
        git(
            &config.checkout,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        git(
            &config.checkout,
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        let session = Session::take(&home, &config).expect("actual held manager leases");
        let ctx = Ctx::new(
            &home,
            None,
            None,
            crate::testkit::NOW,
            Hosts::single(Arc::new(FakeHost::new())),
        );
        Self {
            temporary: Some(temporary),
            home,
            config,
            ctx,
            session,
            passed: false,
        }
    }

    fn producer(&self) -> Producer<'_> {
        Producer::new(
            &self.ctx,
            &HostTools,
            QualifiedGuardian {
                package: PackageScope {
                    cache: cache_root(&self.config),
                    attempt: "1".repeat(32),
                    guardian: std::process::id(),
                    group: std::process::id(),
                    registrar: self.home.join("unused-registrar"),
                    budget: 1 << 20,
                    jobs: 1,
                },
                source: SourceScope {
                    home: self.home.clone(),
                    config: self.config.clone(),
                    lease: self.session.lease().expect("original held lease proof"),
                    guardian: aterm_uds::process::read_identity(
                        i32::try_from(std::process::id()).unwrap(),
                    )
                    .expect("actual live fixture guardian identity"),
                },
            },
        )
    }

    fn advance(&self) {
        let previous = git(&self.config.checkout, &["rev-parse", "HEAD"]);
        fs::write(
            self.config.checkout.join("source.rs"),
            format!("actual source after {previous}\n"),
        )
        .expect("next source");
        git(&self.config.checkout, &["add", "source.rs"]);
        git(&self.config.checkout, &["commit", "-m", "advance source"]);
        git(
            &self.config.checkout,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
    }

    fn artifact(&self) -> (Plan, PathBuf) {
        let source = git(&self.config.checkout, &["rev-parse", "HEAD"]);
        let build_number = git(&self.config.checkout, &["rev-list", "--count", "HEAD"])
            .parse()
            .expect("actual complete-history build count");
        let identity = HarnessIdentity {
            version: format!("0.1.0+g{}", &source[..9]),
            source: source.clone(),
            build_number,
        };
        identity
            .recheck(&self.config.checkout)
            .expect("fresh original source identity");
        let plan = Plan {
            command: Run::new("unused-fixture-compiler", std::iter::empty::<&str>()),
            source,
            harness: Some(identity.clone()),
            cache_freshness: build_local::CacheFreshness::ExactSource,
            analysis_session: false,
            compiler: "b".repeat(40),
            compiler_text: "never executed".into(),
            sysroot: self.home.join("unused-sysroot"),
            triple: crate::triple::Triple::host().expect("native fixture host"),
            metadata: Metadata::default(),
            bins: vec![COMMAND.into()],
            out: self.config.out.clone(),
        };
        let artifact = build_local::harness_artifact_path(&self.config.request(), &plan)
            .expect("real immutable artifact address");
        fs::create_dir_all(artifact.join("bin")).expect("artifact directories");
        fs::set_permissions(&artifact, fs::Permissions::from_mode(0o700))
            .expect("private artifact");
        fs::set_permissions(artifact.join("bin"), fs::Permissions::from_mode(0o700))
            .expect("private bin");
        #[cfg(target_os = "macos")]
        let mut payload = b"\xcf\xfa\xed\xfe".to_vec();
        #[cfg(not(target_os = "macos"))]
        let mut payload = b"\x7fELF".to_vec();
        payload.extend_from_slice(b"inert admission fixture, never executed\n");
        payload.extend_from_slice(identity.source.as_bytes());
        let executable = artifact.join("bin").join(COMMAND);
        fs::write(&executable, &payload).expect("inert artifact payload");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("artifact file mode");
        let digest: String = aterm_digest::Sha256::digest(&payload)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let receipt = aterm_json::json!({
            "schema":2,"origin":"local-build","program":self.config.program,
            "source":identity.source,"build_number":identity.build_number,
            "version":identity.version,"source_policy":"clean-main","command":COMMAND,
            "compiler":plan.compiler,"sysroot":plan.sysroot.display().to_string(),
            "target":plan.triple.as_str(),"policy":"unverified","profile":"release",
            "checkout":self.config.checkout.display().to_string(),
            "logs":self.config.out.join("unused-logs").display().to_string(),
            "exposes":[COMMAND],"executables":[{"path":format!("bin/{COMMAND}"),
                "size":payload.len(),"sha256":digest}]
        });
        fs::write(
            artifact.join("local-build.json"),
            aterm_json::to_vec(&receipt).expect("fixture receipt"),
        )
        .expect("local artifact receipt");
        (plan, artifact)
    }

    fn original_current(&self, producer: &mut Producer<'_>) -> Version {
        let (plan, artifact) = self.artifact();
        let staged = producer
            .stage(&self.config, &plan, &artifact)
            .expect("real original local stage");
        producer
            .select(&self.config, &plan, &staged, ExpectedCurrent::Absent)
            .expect("fresh source selection");
        Version::from(staged.identity())
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if !self.passed {
            let path = self.temporary.take().unwrap().keep();
            eprintln!(
                "preserved failed producer source-selection world: {}",
                path.display()
            );
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Node {
    path: PathBuf,
    identity: [u64; 2],
    mode: u32,
    modified: [i64; 2],
    bytes: Vec<u8>,
    link: Option<PathBuf>,
}

fn tree(root: &Path) -> Vec<Node> {
    fn visit(path: &Path, root: &Path, nodes: &mut Vec<Node>) {
        let meta = fs::symlink_metadata(path).expect("original selection node");
        nodes.push(Node {
            path: path.strip_prefix(root).unwrap().to_owned(),
            identity: [meta.dev(), meta.ino()],
            mode: meta.mode(),
            modified: [meta.mtime(), meta.mtime_nsec()],
            bytes: if meta.is_file() {
                fs::read(path).expect("original node bytes")
            } else {
                Vec::new()
            },
            link: meta
                .file_type()
                .is_symlink()
                .then(|| fs::read_link(path).expect("original link target")),
        });
        if meta.is_dir() {
            let mut entries: Vec<_> = fs::read_dir(path)
                .expect("original directory")
                .map(|entry| entry.expect("original entry").path())
                .collect();
            entries.sort();
            for entry in entries {
                visit(&entry, root, nodes);
            }
        }
    }
    let mut nodes = Vec::new();
    visit(root, root, &mut nodes);
    nodes
}

#[test]
fn fresh_and_recovered_artifacts_select_only_their_actual_clean_source() {
    for recovered in [false, true] {
        let mut world = World::new();
        let mut producer = world.producer();
        let original = world.original_current(&mut producer);
        world.advance();
        let (plan, artifact) = world.artifact();
        if recovered {
            assert_eq!(producer.build(&world.config, &plan).unwrap(), artifact);
            assert!(producer.recovered.is_some());
        }
        let staged = producer.stage(&world.config, &plan, &artifact).unwrap();
        assert!(producer.recovered.is_none());
        producer
            .select(
                &world.config,
                &plan,
                &staged,
                ExpectedCurrent::Build(original.build),
            )
            .expect("matching fresh/recovered source selects");
        assert_eq!(
            producer.current(&world.config).unwrap(),
            CurrentLocal::Selected(staged.identity())
        );
        assert_eq!(
            fs::read_link(
                Layout {
                    prefix: world.config.prefix.clone()
                }
                .program_current(&world.config.program)
            )
            .unwrap(),
            staged.directory()
        );
        drop(producer);
        world.passed = true;
    }
}

#[test]
fn final_source_rejection_preserves_original_current_and_the_complete_stale_stage() {
    for fault in [
        "advanced",
        "dirty",
        "hidden-index",
        "ignored-input",
        "replaced-inode",
        "recovered-advanced",
        "guardian-exited",
        "guardian-unreaped",
    ] {
        let mut world = World::new();
        let mut guardian = fault.starts_with("guardian-").then(|| {
            aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/cat")))
                .stdin(aterm_uds::spawnfd::Stdio::piped())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("actual owned guardian child")
        });
        let mut guardian_input = guardian.as_mut().map(aterm_uds::deadline::cat_echoes);
        let mut producer = world.producer();
        if let Some(child) = &guardian {
            producer._guardian.package.guardian = child.id();
            producer._guardian.source.guardian =
                aterm_uds::process::read_identity(i32::try_from(child.id()).unwrap())
                    .expect("echo proved original guardian executes");
        }
        let original = world.original_current(&mut producer);
        world.advance();
        let (plan, artifact) = world.artifact();
        if fault == "recovered-advanced" {
            assert_eq!(producer.build(&world.config, &plan).unwrap(), artifact);
            assert!(producer.recovered.is_some());
        }
        let staged = producer.stage(&world.config, &plan, &artifact).unwrap();
        let before_store = tree(&world.config.prefix);
        let before_artifact = tree(&artifact);
        match fault {
            "advanced" | "recovered-advanced" => world.advance(),
            "dirty" => fs::write(
                world.config.checkout.join("source.rs"),
                b"uncommitted input\n",
            )
            .unwrap(),
            "hidden-index" => {
                git(
                    &world.config.checkout,
                    &["update-index", "--assume-unchanged", "source.rs"],
                );
                fs::write(
                    world.config.checkout.join("source.rs"),
                    b"hidden uncommitted input\n",
                )
                .unwrap();
                assert!(git(&world.config.checkout, &["status", "--porcelain"]).is_empty());
            }
            "ignored-input" => {
                fs::write(
                    world.config.checkout.join("ignored.rs"),
                    b"ignored build input\n",
                )
                .unwrap();
                assert!(git(&world.config.checkout, &["status", "--porcelain"]).is_empty());
            }
            "replaced-inode" => {
                let replacement = world.home.join("replacement-source");
                git(
                    &world.home,
                    &[
                        "-c",
                        "protocol.file.allow=always",
                        "clone",
                        "--no-local",
                        "--no-hardlinks",
                        world.config.checkout.to_str().unwrap(),
                        replacement.to_str().unwrap(),
                    ],
                );
                assert_eq!(git(&replacement, &["rev-parse", "HEAD"]), plan.source);
                assert!(git(&replacement, &["status", "--porcelain"]).is_empty());
                fs::rename(&world.config.checkout, world.home.join("original-source")).unwrap();
                fs::rename(replacement, &world.config.checkout).unwrap();
                let meta = fs::metadata(&world.config.checkout).unwrap();
                assert_ne!([meta.dev(), meta.ino()], world.session.source_identity());
                assert_eq!(
                    git(&world.config.checkout, &["symbolic-ref", "--short", "HEAD"]),
                    "main"
                );
            }
            "guardian-exited" => {
                // The Session still holds the original program/source lock
                // inodes. Contention alone must not substitute for this exact
                // guardian after normal EOF and actual owned-child reaping.
                guardian_input.take();
                let child = guardian.as_mut().unwrap();
                let mut exit = aterm_uds::deadline::ExitWait::new(child);
                let deadline = Instant::now() + HANG;
                let outcome = loop {
                    if let Some(outcome) = child.try_wait().expect("reap original guardian") {
                        break outcome;
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    assert!(
                        !remaining.is_zero(),
                        "original guardian did not exit after EOF"
                    );
                    exit.wait(remaining);
                };
                assert!(outcome.success(), "original guardian failed: {outcome:?}");
                let original = producer._guardian.source.guardian;
                assert_ne!(
                    aterm_uds::process::read_identity(original.pid),
                    Some(original)
                );
                crate::harness_local::fs::verify_lease(
                    &world.home,
                    &world.config,
                    &producer._guardian.source.lease,
                )
                .expect("original locks remain actually held after guardian exit");
            }
            "guardian-unreaped" => {
                guardian_input.take();
                let child = guardian.as_mut().unwrap();
                let mut exit = aterm_uds::deadline::ExitWait::new(child);
                let deadline = Instant::now() + HANG;
                loop {
                    if aterm_uds::process::process_presence(i32::try_from(child.id()).unwrap())
                        == aterm_uds::process::ProcessPresence::Exited
                    {
                        break;
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    assert!(
                        !remaining.is_zero(),
                        "original guardian did not become exited before reap"
                    );
                    exit.wait(remaining);
                }
                crate::harness_local::fs::verify_lease(
                    &world.home,
                    &world.config,
                    &producer._guardian.source.lease,
                )
                .expect("original locks remain held with the guardian unreaped");
            }
            _ => unreachable!(),
        }
        let result = producer.select(
            &world.config,
            &plan,
            &staged,
            ExpectedCurrent::Build(original.build),
        );
        assert!(
            matches!(result, Err(SelectionError::Rejected(_))),
            "{fault} must reject before link mutation"
        );
        assert_eq!(
            tree(&world.config.prefix),
            before_store,
            "{fault} changed original current inode/target, stage, receipt or ready record"
        );
        assert_eq!(
            tree(&artifact),
            before_artifact,
            "{fault} changed immutable source artifact"
        );
        assert_eq!(
            producer.current(&world.config).unwrap(),
            CurrentLocal::Selected(atpkg::local_harness::LocalIdentity {
                program: original.program,
                command: original.command,
                source: original.source,
                compiler: original.compiler,
                version: original.version,
                build: original.build,
                target: original.target,
                policy: original.policy,
                profile: original.profile,
            })
        );
        if fault == "guardian-unreaped" {
            let child = guardian.as_mut().unwrap();
            let mut exit = aterm_uds::deadline::ExitWait::new(child);
            let deadline = Instant::now() + HANG;
            loop {
                if let Some(outcome) = child.try_wait().expect("reap exited original guardian") {
                    assert!(outcome.success());
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "exited original guardian was not reapable"
                );
                exit.wait(remaining);
            }
        }
        drop(producer);
        world.passed = true;
    }
}
