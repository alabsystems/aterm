// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real frozen-tool fixture used by the dry-run controller's foreign-cutter test.

use super::*;

/// A prepared record beside a live supervisor and its exclusively held workspace.
/// The controller releases both claims only after the child reports entering the wait.
#[cfg(unix)]
pub(crate) struct PrefreezeFixture {
    pub fixture: Fixture,
    owners: Option<(crate::release::RunClaim, crate::release::RunClaim)>,
}

#[cfg(unix)]
impl PrefreezeFixture {
    pub fn new() -> Self {
        let fixture = Fixture::new();
        let real = Self::real(fixture.root.path());
        let cache = cutter_cache_dir(&real.release);
        let source = fixture.root.path().join("prepared");
        aterm_tempfile::stand_in(&source, &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '%s\\n' '{}'; else printf '%s\\n' 'R policy passes'; fi\n",
            crate::release_tools::CUTTER_VERSION
        )).unwrap();
        let source = std::fs::canonicalize(source).unwrap();
        let inputs = cfg!(target_os = "macos").then(|| crate::cutter_inputs::Inputs {
            digest: "1".repeat(64),
            sources: "2".repeat(64),
            seal: "3".repeat(64),
        });
        let prepared = crate::release_tools::stage_cutter(
            &cache,
            &fixture.candidate,
            &source,
            &real.step_log(real.step),
            inputs,
        )
        .unwrap();
        crate::release_tools::publish_cutter(&cache, &prepared).unwrap();
        let pid = std::process::id();
        let started = started_of(pid).unwrap();
        let take = |name| {
            crate::release::RunClaim::take(
                &cache.join(name),
                (pid, &started),
                &real.logs,
                &mut started_of,
            )
            .unwrap()
        };
        let owners = Some((take("prefreeze-running"), take("workspace-running")));
        Self { fixture, owners }
    }

    pub fn release(&mut self) {
        self.owners.take();
    }

    pub fn real(root: &Path) -> Real {
        let repo = root.join("repo");
        let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
        real.release = root.join("release");
        real.state = root.join("state");
        real.prefrozen_inputs = |state, _| Some(state.join("kept-seal"));
        real.prefrozen_step1 = Some(PrefrozenStep1 {
            commit: crate::ledger::git_ok(&GitCli::new(&repo), &["rev-parse", "--branches"])
                .unwrap()
                .stdout_utf8()
                .trim()
                .into(),
            seal: "3".repeat(64),
        });
        real.handed_inputs = || Some(PathBuf::from("/fixture/kept-seal"));
        real.argv = vec!["release".into(), "--dry-run".into()];
        real
    }

    pub fn judge(real: &Real, commit: &str) -> Result<(), Refusal> {
        let unused = Prover {
            bin: PathBuf::from("/no-compiler"),
            commit: "f".repeat(40),
            source: "unused".into(),
        };
        let cutter = real
            .frozen_cutter(commit, &unused)
            .map_err(|why| Refusal::new(why, "fixture"))?;
        let output = aterm_uds::deadline::output_within(
            &mut real.cutter_command(&cutter.tool.path),
            std::time::Duration::from_secs(60),
        )
        .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("R policy passes"));
        print!("{}", String::from_utf8_lossy(&output.stdout));
        Ok(())
    }
}

#[cfg(unix)]
#[test]
fn repeated_prefreeze_uses_the_record_without_taking_the_held_workspace() {
    let fixture = PrefreezeFixture::new();
    let mut real = PrefreezeFixture::real(fixture.fixture.root.path());
    let cache = cutter_cache_dir(&real.release);
    let before = std::fs::read(cache.join("workspace-running")).unwrap();
    let prepared = prepare_cached_cutter(&mut real, &fixture.fixture.candidate).unwrap();
    assert_eq!(prepared.commit, fixture.fixture.candidate);
    assert!(real.prefreeze_claim.is_none());
    assert_eq!(
        std::fs::read(cache.join("workspace-running")).unwrap(),
        before
    );
}

pub(crate) struct Fixture {
    pub root: aterm_tempfile::TempDir,
    pub repo: PathBuf,
    pub checkout: String,
    pub candidate: String,
}

impl Fixture {
    pub fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        crate::ledger::testing::init(
            &repo,
            &[
                ("user.name", "Fixture"),
                ("user.email", "fixture@example.invalid"),
            ],
        );
        std::fs::create_dir_all(repo.join("crates/aterm-release/src")).unwrap();
        std::fs::write(repo.join(".gitignore"), "/target/\n").unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers=['crates/aterm-release']\nresolver='2'\n[workspace.package]\nversion='0.6.0'\n[profile.gate]\ninherits='dev'\n").unwrap();
        std::fs::write(
            repo.join("crates/aterm-release/Cargo.toml"),
            "[package]\nname='aterm-release'\nversion='0.0.0'\nedition='2021'\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("Cargo.lock"),
            "version=4\n[[package]]\nname='aterm-release'\nversion='0.0.0'\n",
        )
        .unwrap();
        let git = GitCli::new(&repo);
        let revision = |verdict, label| {
            std::fs::write(repo.join("crates/aterm-release/src/main.rs"), format!(
                "fn main() {{ if std::env::args().any(|a| a == \"--version\") {{ println!(\"aterm-release tool 1\"); }} else {{ println!(\"{label}\"); std::process::exit({verdict}); }} }}\n"
            )).unwrap();
            crate::ledger::git_ok(&git, &["add", "."]).unwrap();
            crate::ledger::git_ok(&git, &["commit", "-qm", label]).unwrap();
            crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
                .unwrap()
                .stdout_utf8()
                .trim()
                .to_owned()
        };
        let checkout = revision(17, "checkout policy refuses");
        let candidate = revision(0, "R policy passes");
        crate::ledger::git_ok(&git, &["checkout", "-q", "--detach", &checkout]).unwrap();
        Self {
            root,
            repo,
            checkout,
            candidate,
        }
    }

    /// Production compiler checkout, frozen proof, and handover command. The controller
    /// supplies only the chosen revision; no release credentials or remote writes exist.
    pub fn verdict(&self, commit: &str) -> (i32, String) {
        let mut real = Real::new(&self.repo, Some(&self.repo), true, false).unwrap();
        real.release = self.root.path().join("release");
        real.state = self.root.path().join("state");
        real.build_inputs = || Ok("f".repeat(64));
        real.arm_inputs = |_, _| {};
        real.argv = vec!["release".into(), "--dry-run".into()];
        let prover = Prover {
            bin: crate::gates::trust_stage2_bin().unwrap(),
            commit: "0".repeat(40),
            source: "fixture Trust".into(),
        };
        let cutter = real.frozen_cutter(commit, &prover).unwrap();
        let mut command = real.cutter_command(&cutter.tool.path);
        let output =
            aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
                .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
}

/// Each take is a fresh proved cutter process, so no process-local seal crosses modes.
#[test]
#[ignore = "explicit credential-free, source-only step 1 measurement"]
fn measure_same_main_step_one() {
    let repo = PathBuf::from(std::env::var_os("ATERM_DRY_TOOLS_REPO").expect("owned clean clone"));
    let engine =
        PathBuf::from(std::env::var_os("ATERM_DRY_TOOLS_ENGINE").expect("read-only engine source"));
    let base = std::env::var_os("ATERM_DRY_TOOLS_BASE").expect("input base");
    let mode = std::env::var("ATERM_DRY_TOOLS_MODE").expect("dry or real");
    assert!(matches!(mode.as_str(), "dry" | "real"));
    let mut real = Real::new(&repo, Some(&engine), true, false).unwrap();
    let commit = real.head();
    crate::release_tools::clean_at(&repo, &commit).unwrap();
    let prover = real.prover(None).unwrap();
    let started = std::time::Instant::now();
    let cutter = real.frozen_cutter(&commit, &prover).unwrap();
    let freeze = started.elapsed().as_secs_f64();
    real.argv = vec![
        "--step-one-source-measure".into(),
        mode.clone().into(),
        base,
        engine.into_os_string(),
    ];
    let mut command = real.cutter_command(&cutter.tool.path);
    for name in [
        "ATERM_DRY_TOOLS_REPO",
        "ATERM_DRY_TOOLS_ENGINE",
        "ATERM_DRY_TOOLS_BASE",
        "ATERM_DRY_TOOLS_MODE",
    ] {
        command.env_remove(name);
    }
    let output = real.output_forwarded(&mut command).unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success());
    println!(
        "step-one-launch {}",
        aterm_json::json!({
            "mode":mode, "commit":commit, "freeze_s":freeze,
            "wall_s":started.elapsed().as_secs_f64(),
        })
    );
}

/// A real private bump, using local release history and the production editor pipeline.
/// The only stand-in is the publication engine's manifest editor.
#[cfg(unix)]
pub(crate) struct BumpFixture {
    pub root: aterm_tempfile::TempDir,
    pub repo: PathBuf,
    pub base: String,
    real: Real,
    origin: crate::github_remote::Remote,
}

#[cfg(unix)]
impl BumpFixture {
    pub fn new(red: bool) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        crate::ledger::testing::init(
            &repo,
            &[
                ("user.name", "Fixture"),
                ("user.email", "fixture@example.invalid"),
            ],
        );
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn value() {}\n").unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\n[workspace.package]\nversion = \"0.5.0\"\n[package]\nname = \"fixture\"\nversion.workspace = true\nedition = \"2021\"\n").unwrap();
        std::fs::write(
            repo.join("Cargo.lock"),
            "version = 4\n[[package]]\nname = \"fixture\"\nversion = \"0.5.0\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("RELEASES.ledger"), "1 0.5.0\n").unwrap();
        std::fs::write(
            repo.join("CHANGELOG.md"),
            if red {
                "# Changelog\n\n## [Unreleased]\n\n## [0.5.0] - date\n\n- Previous release.\n"
            } else {
                "# Changelog\n\n## [Unreleased]\n\n- A useful change.\n\n## [0.5.0] - date\n\n- Previous release.\n"
            },
        )
        .unwrap();
        let harness = repo.join("crates/aterm-phase/src/phase.rs");
        std::fs::create_dir_all(harness.parent().unwrap()).unwrap();
        for h in aterm_phase::CliHarness::ALL {
            std::fs::write(repo.join(h.version_file()), "pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(0, 1, \"0.1\");\n").unwrap();
        }
        let git = GitCli::new(&repo);
        let run = |args: &[&str]| {
            crate::ledger::git_ok(&git, args)
                .unwrap()
                .stdout_utf8()
                .trim()
                .to_owned()
        };
        run(&["add", "."]);
        run(&["commit", "-qm", "release baseline"]);
        run(&["tag", "v0.5.0"]);
        if !red {
            let text = std::fs::read_to_string(&harness).unwrap();
            std::fs::write(&harness, format!("{text}\npub fn new_reader() {{}}\n")).unwrap();
            run(&["add", "."]);
            run(&["commit", "-qm", "reader change owes a harness bump"]);
        }
        let base = run(&["rev-parse", "HEAD"]);
        let bare = root.path().join("origin.git");
        run(&[
            "clone",
            "-q",
            "--bare",
            repo.to_str().unwrap(),
            bare.to_str().unwrap(),
        ]);
        run(&[
            "remote",
            "add",
            "origin",
            "https://github.com/fixture/app.git",
        ]);
        let origin = crate::github_remote::claim_fixture(&repo, &bare);
        let engine = root.path().join("engine");
        std::fs::create_dir_all(engine.join("bin")).unwrap();
        aterm_tempfile::stand_in(&engine.join("bin/pub-bump-edit"), "#!/bin/sh\n/usr/bin/sed \"s/$3/$2/g\" \"$1/Cargo.toml\" > \"$1/Cargo.toml.next\" && /bin/mv \"$1/Cargo.toml.next\" \"$1/Cargo.toml\"\n").unwrap();
        let mut real = Real::new(&repo, Some(&engine), true, false).unwrap();
        real.release = root.path().join("release");
        Self {
            root,
            repo,
            base,
            real,
            origin,
        }
    }

    pub fn preview(&mut self, c: &Candidate) -> Result<String, Refusal> {
        let prover = Prover {
            bin: crate::gates::trust_stage2_bin().unwrap(),
            commit: "0".repeat(40),
            source: "fixture Trust".into(),
        };
        let mut c = c.clone();
        c.commit = self.base.clone();
        self.real
            .preview_bump_from_origin(&c, &prover, &self.origin)
    }
}

#[cfg(unix)]
#[test]
fn private_bump_tree_matches_the_real_bump_edits() {
    let mut fixture = BumpFixture::new(false);
    let c = Candidate {
        commit: fixture.base.clone(),
        version: "0.6.0".into(),
        why: "fixture".into(),
        bump_from: Some("0.5.0".into()),
        carry: Vec::new(),
    };
    let preview = fixture.preview(&c).unwrap();
    let prover = Prover {
        bin: crate::gates::trust_stage2_bin().unwrap(),
        commit: "0".repeat(40),
        source: "fixture Trust".into(),
    };
    fixture.real.repo = fixture.repo.canonicalize().unwrap();
    let (_, paths) = fixture
        .real
        .edit_bump("0.5.0", "0.6.0", &prover, &fixture.origin)
        .unwrap();
    let git = GitCli::new(&fixture.repo);
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    crate::ledger::git_ok(&git, &add).unwrap();
    let actual = crate::ledger::git_ok(&git, &["write-tree"])
        .unwrap()
        .stdout_utf8();
    let planned = crate::ledger::git_ok(&git, &["rev-parse", &format!("{preview}^{{tree}}")])
        .unwrap()
        .stdout_utf8();
    assert_eq!(actual, planned);
}
