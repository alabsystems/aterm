// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Handoff fixture staging and landing integration tests.

use aterm_uds::spawnfd::Command;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::fixtures::{self, Answers, Landed, Pins};
use crate::gates::{HANDOFF_FIXTURE_GUARD, HANDOFF_FIXTURE_ROOT};
use crate::ledger::testing::Git;
use crate::ledger::{GitRunner, Origin as OriginTransport, RefWrite, RunOut};

const GUARD: &str = "// the guard\nconst PINNED_DESKS: &[(&str, &str)] = &[\n    (\"v0.5.0\", \"history\"),\n];\n\nconst REQUIRED_DESKS: &[(&str, &str)] = &[\n    (\"v0.5.0\", \"history\"),\n];\n";

const RELEASE: &str = "cccccccccccccccccccccccccccccccccccccccc";

static ORIGIN_BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

const HEADER: &str = "# serde corpus `shipped-state`: one case per line.\n# ok: the value.\n# escapes.\n# FNV-1a digest and length.\n";

fn corpus() -> String {
    format!(
        "{HEADER}v0.5.0/history/manifest\tok\tfnv1a64:5 len=5\n\
         negative-control/torn-manifest\terr\tTOML parse error\n"
    )
}

fn pins(build: u64) -> Pins {
    Pins {
        version: "0.6.0".into(),
        build,
        commit: RELEASE.into(),
    }
}

fn reader(staged: &fixtures::Staged) -> impl FnMut(&str) -> fixtures::Reading + '_ {
    |preview| fixtures::Reading {
        checked: Some(preview.to_owned()),
        answers: Ok(fs::read_to_string(&staged.answers).unwrap()),
        checks: "PASS (test reader)".into(),
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = crate::ledger::testing::git()
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn identity(repo: &Path) {
    crate::ledger::testing::config(
        repo,
        &[
            ("user.email", "ship-test@aterm.invalid"),
            ("user.name", "aterm ship test"),
            ("commit.gpgsign", "false"),
        ],
    );
}

struct Origin {
    dir: aterm_tempfile::TempDir,
    work: PathBuf,
    rival: PathBuf,
}

impl Origin {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        static BUILT: crate::template::Template<()> = crate::template::Template::new();
        BUILT.lay(root, Self::build);
        let work = root.join("work").canonicalize().unwrap();
        let rival = root.join("rival");
        // The index binds this copy's paths and compiler, so it is never part of the template.
        seed_index(&work, b"binary");
        Origin { dir, work, rival }
    }

    fn build(root: &Path) {
        ORIGIN_BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        git(root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        crate::ledger::testing::config(&root.join("origin.git"), &[]);
        git(root, &["clone", "-q", "origin.git", "rival"]);
        let rival = root.join("rival");
        identity(&rival);
        git(&rival, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        let guard = rival.join(HANDOFF_FIXTURE_GUARD);
        fs::create_dir_all(guard.parent().unwrap()).unwrap();
        fs::write(&guard, GUARD).unwrap();
        let old = rival.join(HANDOFF_FIXTURE_ROOT).join("v0.5.0/history");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("parent.toml"), "producer_version = \"0.5.0\"\n").unwrap();
        fs::write(rival.join("README.md"), "aterm\n").unwrap();
        fs::write(rival.join(".gitignore"), "target/\n").unwrap();
        let answers = rival.join(fixtures::SHIPPED_STATE_CORPUS);
        fs::create_dir_all(answers.parent().unwrap()).unwrap();
        fs::write(&answers, corpus()).unwrap();
        git(&rival, &["add", "-A"]);
        git(&rival, &["commit", "-q", "-m", "seed"]);
        git(&rival, &["push", "-q", "-u", "origin", "main"]);
        git(root, &["clone", "-q", "origin.git", "work"]);
        let work = root.join("work").canonicalize().unwrap();
        identity(&work);
    }

    fn stage_root(&self) -> PathBuf {
        self.dir.path().join("cache/handoff-fixtures")
    }

    fn on_main(&self, path: &str) -> Vec<u8> {
        crate::ledger::testing::git()
            .current_dir(self.dir.path().join("origin.git"))
            .args(["show", &format!("main:{path}")])
            .output()
            .unwrap()
            .stdout
    }
}

/// NEGATIVE CONTROLS: rebuilding every seed increments the count; sharing a mutable origin
/// moves the second copy's main when the first pushes; retaining template paths misroutes it.
#[test]
fn cached_origins_build_once_and_keep_independent_git_and_index_paths() {
    let a = Origin::new();
    let b = Origin::new();
    let before = git(&b.work, &["rev-parse", "origin/main"]);
    fs::write(a.work.join("README.md"), "only this copy\n").unwrap();
    git(&a.work, &["add", "README.md"]);
    git(&a.work, &["commit", "-q", "-m", "private copy"]);
    git(&a.work, &["push", "-q", "origin", "main"]);
    git(&b.work, &["fetch", "-q", "origin"]);
    assert_eq!(git(&b.work, &["rev-parse", "origin/main"]), before);
    assert_eq!(
        fs::read_to_string(b.work.join("README.md")).unwrap(),
        "aterm\n"
    );
    let c = Origin::new();
    assert_eq!(git(&c.work, &["rev-parse", "HEAD"]), before);
    for origin in [&a, &b, &c] {
        let index =
            fs::read_to_string(origin.work.join("target/verify/test-executables.json")).unwrap();
        assert!(
            index.contains(&origin.work.to_string_lossy().to_string()),
            "{index}"
        );
    }
    assert_eq!(ORIGIN_BUILDS.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// The actual native adapter uses only this owned bare repository. Fixture
/// actors retain their separate local Git setup route; the algorithm gets no
/// raw transport through its GitRunner.
fn native(o: &Origin) -> crate::github_remote::Remote {
    git(
        &o.work,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    crate::github_remote::claim_fixture(&o.work, &o.dir.path().join("origin.git"))
}

fn artifact(root: &Path, binary: &Path) -> String {
    aterm_json::json!({"reason":"compiler-artifact", "package_id":"aterm-gui", "manifest_path":root.join("crates/aterm-gui/Cargo.toml"), "target":{"kind":["lib"],"name":"aterm_gui"}, "profile":{"test":true}, "executable":binary}).to_string()
}

/// The `targo` a cut drives, found as the cutter finds its own ([`crate::gates::resolve_targo`]):
/// the index a seed writes and the staging that reads it name their compiler by the `trustc`
/// beside it (`trustc -vV`). A host with no Trust toolchain could not run the test.
fn targo() -> PathBuf {
    crate::gates::resolve_targo().unwrap_or_else(|e| {
        aterm_spec::host::could_not_run(format!(
            "the index names its compiler by a Trust toolchain, and none resolves here: {e}\n\
             get one: {}",
            crate::gates::TRUST_TOOLCHAIN_REMEDIES
        ))
    })
}

fn seed_index(root: &Path, bytes: &[u8]) {
    seed_index_named(root, bytes, "aterm_gui-0123456789abcdef");
}

fn seed_index_named(root: &Path, bytes: &[u8], name: &str) {
    let binary = root.join("target/debug/deps").join(name);
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::write(&binary, bytes).unwrap();
    let (tree, compiler) = aterm_verify::test_index::identities(root, &targo()).unwrap();
    aterm_verify::test_index::Index::from_messages(tree, compiler, &artifact(root, &binary))
        .unwrap()
        .write(&root.join("target/verify/test-executables.json"))
        .unwrap();
}

fn exit(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt as _;
    std::process::ExitStatus::from_raw(code << 8)
}

fn env_of(cmd: &Command, key: &str) -> Option<String> {
    cmd.get_envs()
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
}

fn field_hold_pass(cmd: &Command) -> Option<Output> {
    cmd.get_args()
        .any(|arg| arg == fixtures::FIELD_HOLD_TEST)
        .then(|| Output {
            status: exit(0),
            stdout: b"test result: ok. 1 passed; 0 failed; 0 ignored\n".to_vec(),
            stderr: Vec::new(),
        })
}

fn passed() -> String {
    format!(
        "test result: ok. {} passed; 0 failed; 0 ignored; 0 measured; 9 filtered out\n",
        if cfg!(target_os = "macos") { 2 } else { 1 }
    )
}

fn staged_answers(desks: &[&str]) -> String {
    let cases: String = desks
        .iter()
        .map(|desk| format!("v0.6.0/{desk}/manifest\tok\tfnv1a64:6 len=6\n"))
        .collect();
    corpus().replacen("negative-control/", &format!("{cases}negative-control/"), 1)
}

fn generator<'a>(
    desks: &'a [&'a str],
    runs: &'a Cell<usize>,
) -> impl FnMut(&mut Command) -> std::io::Result<Output> + 'a {
    move |cmd: &mut Command| {
        assert_eq!(
            Path::new(cmd.get_program()).parent().unwrap(),
            cmd.get_current_dir().unwrap().join("target/debug/deps")
        );
        assert_eq!(
            PathBuf::from(env_of(cmd, "CARGO_MANIFEST_DIR").unwrap()),
            cmd.get_current_dir().unwrap().join("crates/aterm-gui")
        );
        let answers = PathBuf::from(env_of(cmd, fixtures::ANSWERS_OUT).unwrap());
        fs::write(answers, staged_answers(desks)).unwrap();
        runs.set(runs.get() + 1);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            fixtures::GENERATOR_TESTS
                .iter()
                .all(|t| args.iter().any(|a| a == t)),
            "{args:?}"
        );
        let out = PathBuf::from(env_of(cmd, "ATERM_HANDOFF_FIXTURE_OUT").unwrap());
        let build = env_of(cmd, "SOURCE_DATE_EPOCH").unwrap();
        let short = env_of(cmd, "ATERM_BUILD_GIT_COMMIT").unwrap();
        // The generator is a library test binary: it compiles no product version, so the
        // release names the one it writes as.
        let version = env_of(cmd, "ATERM_HANDOFF_FIXTURE_VERSION").unwrap();
        assert_eq!(
            env_of(cmd, "ATERM_BUILD_GIT_COMMIT_FULL").as_deref(),
            Some(RELEASE)
        );
        for desk in desks {
            fs::create_dir_all(out.join(desk)).unwrap();
            fs::write(
                out.join(desk).join("parent.toml"),
                format!(
                    "producer_version = \"{version}\"\nproducer_commit = \"{short}\"\n\
                     outgoing_build = {build}\n"
                ),
            )
            .unwrap();
            fs::write(out.join(desk).join("s0.grid"), [0u8, 159, 146, 150, 10]).unwrap();
        }
        fs::write(
            out.join("rendezvous.toml"),
            format!("producer_version = \"{version}\"\n"),
        )
        .unwrap();
        Ok(Output {
            status: exit(0),
            stdout: passed().into_bytes(),
            stderr: Vec::new(),
        })
    }
}

#[test]
fn the_indexed_generator_runs_once_per_reserved_identity() {
    let o = Origin::new();
    let runs = Cell::new(0);
    let mut run = generator(&["history"], &runs);
    let root = o.stage_root();
    let first = fixtures::stage(&targo(), &o.work, &root, &pins(7), &mut run).unwrap();
    assert!(first.fresh && first.rendezvous);
    let again = fixtures::stage(&targo(), &o.work, &root, &pins(7), &mut run).unwrap();
    assert!(!again.fresh);
    assert_eq!(runs.get(), 1);
    let next = fixtures::stage(&targo(), &o.work, &root, &pins(8), &mut run).unwrap();
    assert!(next.fresh);
    assert_eq!(runs.get(), 2);
    assert!(!pins(7).staged_in(&root).exists());
    assert!(
        fs::read_to_string(next.dir.join("history/parent.toml"))
            .unwrap()
            .contains("outgoing_build = 8")
    );
}

#[test]
fn a_missing_or_changed_binary_compiles_only_the_generator_before_running_it() {
    for changed in [false, true] {
        let o = Origin::new();
        let binary = o.work.join("target/debug/deps/aterm_gui-0123456789abcdef");
        if changed {
            fs::write(&binary, b"changed").unwrap();
        } else {
            fs::remove_file(&binary).unwrap();
        }
        let runs = Cell::new(0);
        let compiles = Cell::new(0);
        let mut generate = generator(&["history"], &runs);
        let mut run = |cmd: &mut Command| {
            if cmd.get_args().any(|arg| arg == "--no-run") {
                let args: Vec<_> = cmd.get_args().collect();
                assert!(args.iter().any(|a| *a == "--lib"));
                assert!(args.iter().any(|a| *a == "aterm-gui"));
                assert!(!args.iter().any(|a| *a == "--workspace"));
                compiles.set(compiles.get() + 1);
                fs::write(&binary, b"binary").unwrap();
                return Ok(Output {
                    status: exit(0),
                    stdout: artifact(&o.work, &binary).into_bytes(),
                    stderr: Vec::new(),
                });
            }
            generate(cmd)
        };
        fixtures::stage(&targo(), &o.work, &o.stage_root(), &pins(7), &mut run).unwrap();
        assert_eq!(compiles.get(), 1);
        assert_eq!(runs.get(), 1);
    }
}

#[test]
fn an_interrupted_binary_holds_only_its_own_execution() {
    use std::os::unix::process::ExitStatusExt as _;
    let o = Origin::new();
    let mut killed = |_: &mut Command| {
        Ok(Output {
            status: std::process::ExitStatus::from_raw(9),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    };
    fixtures::stage(&targo(), &o.work, &o.stage_root(), &pins(7), &mut killed).unwrap_err();
    let runs = Cell::new(0);
    let mut run = generator(&["history"], &runs);
    let error =
        fixtures::stage(&targo(), &o.work, &o.stage_root(), &pins(7), &mut run).unwrap_err();
    assert!(error.to_string().contains("never showed it ran"));
    assert_eq!(runs.get(), 0);
    // A stale digest must not let a fallback compiler replace this held path.
    fs::write(
        o.work.join("target/debug/deps/aterm_gui-0123456789abcdef"),
        b"changed",
    )
    .unwrap();
    assert!(fixtures::stage(&targo(), &o.work, &o.stage_root(), &pins(7), &mut run).is_err());
    assert_eq!(runs.get(), 0);
    // Another executable path has its own hold, even with identical bytes in the
    // same target directory.
    seed_index_named(&o.work, b"binary", "aterm_gui-fedcba9876543210");
    fixtures::stage(&targo(), &o.work, &o.stage_root(), &pins(7), &mut run).unwrap();
    assert_eq!(runs.get(), 1);
}

#[test]
fn a_generator_that_refuses_or_leaves_a_required_desk_out_stages_nothing() {
    let o = Origin::new();
    let root = o.stage_root();
    let mut failing = |_: &mut Command| -> std::io::Result<Output> {
        Ok(Output {
            status: exit(101),
            stdout: b"test seamless::carry_tests::fixture_generator::generate_handoff_fixtures ... FAILED\n\
                      test result: FAILED. 1 passed; 1 failed; 0 ignored\n"
                .to_vec(),
            stderr: Vec::new(),
        })
    };
    fixtures::stage(&targo(), &o.work, &root, &pins(7), &mut failing).unwrap_err();
    assert!(fixtures::staged(&root, &pins(7)).is_none());

    let runs = Cell::new(0);
    let mut partial = generator(&["held-pane"], &runs);
    fixtures::stage(&targo(), &o.work, &root, &pins(7), &mut partial).unwrap_err();
    assert!(fixtures::staged(&root, &pins(7)).is_none());

    let mut unanswered = |cmd: &mut Command| {
        let out = generator(&["history"], &runs)(cmd);
        assert_eq!(
            cmd.get_program(),
            cmd.get_current_dir()
                .unwrap()
                .join("target/debug/deps/aterm_gui-0123456789abcdef")
        );
        assert_eq!(
            PathBuf::from(env_of(cmd, "CARGO_MANIFEST_DIR").unwrap()),
            cmd.get_current_dir().unwrap().join("crates/aterm-gui")
        );
        let answers = PathBuf::from(env_of(cmd, fixtures::ANSWERS_OUT).unwrap());
        fs::write(answers, corpus()).unwrap();
        out
    };
    let refused = fixtures::stage(&targo(), &o.work, &root, &pins(7), &mut unanswered).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("it wrote no shipped-state answers for v0.6.0's desks"),
        "{refused}"
    );
    assert!(fixtures::staged(&root, &pins(7)).is_none());
}

#[test]
fn a_landing_commits_the_staging_and_its_rows_on_main_once() {
    let o = Origin::new();
    let runs = Cell::new(0);
    let mut run = generator(&["history", "twelve-panes"], &runs);
    let root = o.stage_root();
    let staged = fixtures::stage(&targo(), &o.work, &root, &pins(7), &mut run).unwrap();
    let work = Git::new(&o.work);
    let before = git(&o.work, &["rev-parse", "origin/main"])
        .trim()
        .to_string();

    let landed =
        fixtures::land(&work, &native(&o), &staged, &pins(7), &mut reader(&staged)).unwrap();
    let Landed::Pushed {
        commit,
        attempt,
        answers,
        ..
    } = &landed
    else {
        panic!("{landed:?}");
    };
    assert_eq!(*attempt, 1);
    assert_eq!(*answers, Answers::Recorded(2));
    assert_eq!(
        String::from_utf8(o.on_main(fixtures::SHIPPED_STATE_CORPUS)).unwrap(),
        staged_answers(&["history", "twelve-panes"])
    );
    assert!(
        landed
            .words(&pins(7))
            .contains("with its 2 shipped-state answers"),
        "{}",
        landed.words(&pins(7))
    );
    let grid = format!("{HANDOFF_FIXTURE_ROOT}/v0.6.0/history/s0.grid");
    assert_eq!(
        o.on_main(&grid),
        [0u8, 159, 146, 150, 10],
        "binary bytes as staged"
    );
    let guard = String::from_utf8(o.on_main(HANDOFF_FIXTURE_GUARD)).unwrap();
    assert!(
        guard.contains(
            "    (\"v0.5.0\", \"history\"),\n    (\"v0.6.0\", \"history\"),\n    \
             (\"v0.6.0\", \"twelve-panes\"),\n];"
        ),
        "{guard}"
    );
    native(&o).refresh_main().unwrap();
    assert_eq!(
        git(&o.work, &["rev-parse", "origin/main"]).trim(),
        commit.as_str()
    );
    assert_eq!(
        git(&o.work, &["rev-parse", &format!("{commit}^")]).trim(),
        before
    );
    let moved = git(&o.work, &["diff", "--name-only", &before, commit]);
    assert!(
        moved.lines().all(|path| path == HANDOFF_FIXTURE_GUARD
            || path == fixtures::SHIPPED_STATE_CORPUS
            || path.starts_with(&format!("{HANDOFF_FIXTURE_ROOT}/v0.6.0/"))),
        "{moved}"
    );
    assert!(
        moved
            .lines()
            .any(|path| path == fixtures::SHIPPED_STATE_CORPUS),
        "{moved}"
    );
    assert_eq!(
        fixtures::on_commit(&work, "origin/main", "0.6.0")
            .unwrap()
            .map(|found| found.desks),
        Some(vec!["history".to_string(), "twelve-panes".to_string()])
    );

    let again =
        fixtures::land(&work, &native(&o), &staged, &pins(7), &mut reader(&staged)).unwrap();
    assert!(matches!(again, Landed::Already { .. }), "{again:?}");
    native(&o).refresh_main().unwrap();
    assert_eq!(
        git(&o.work, &["rev-parse", "origin/main"]).trim(),
        commit.as_str()
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Movement {
    Edit,
    Revert,
    Rewrite,
}

struct Raced<'a> {
    inner: Git,
    remote: crate::github_remote::Remote,
    rival: Option<&'a Path>,
    remaining: Cell<u32>,
    pushes: RefCell<Vec<String>>,
    refreshes: Cell<u32>,
    lose_reply: bool,
    path: &'static str,
    movement: Movement,
}

#[test]
fn main_reader_answers_replace_line_answers_and_a_named_red_still_lands() {
    let o = Origin::new();
    let runs = Cell::new(0);
    let staged = fixtures::stage(
        &targo(),
        &o.work,
        &o.stage_root(),
        &pins(7),
        &mut generator(&["history"], &runs),
    )
    .unwrap();
    let line = fs::read_to_string(&staged.answers).unwrap();
    let main = line.replace(
        "v0.6.0/history/manifest\tok\t",
        "v0.6.0/history/manifest\tok\tMAIN-READER-",
    );
    assert_ne!(
        main, line,
        "negative control: main and line must answer differently"
    );
    let mut reads = 0;
    let landed = fixtures::land(
        &Git::new(&o.work),
        &native(&o),
        &staged,
        &pins(7),
        &mut |preview| {
            reads += 1;
            assert_eq!(
                git(
                    &o.work,
                    &[
                        "show",
                        &format!("{preview}:{HANDOFF_FIXTURE_ROOT}/v0.6.0/history/parent.toml")
                    ]
                )
                .as_bytes(),
                fs::read(staged.dir.join("history/parent.toml")).unwrap()
            );
            assert_eq!(
                git(
                    &o.work,
                    &[
                        "show",
                        &format!("{preview}:{}", fixtures::SHIPPED_STATE_CORPUS)
                    ]
                ),
                corpus()
            );
            fixtures::Reading {
                checked: Some(preview.to_owned()),
                answers: Ok(main.clone()),
                checks: "FAIL: restore_claude_auth_tests::new_route".into(),
            }
        },
    )
    .unwrap();
    assert_eq!(reads, 1, "a FAIL is never re-rolled");
    assert_eq!(o.on_main(fixtures::SHIPPED_STATE_CORPUS), main.as_bytes());
    assert!(
        landed
            .words(&pins(7))
            .contains("FAIL: restore_claude_auth_tests::new_route")
    );
    assert_eq!(
        fs::read_to_string(&staged.answers).unwrap(),
        line,
        "producer staging is immutable"
    );
}

#[test]
fn an_unavailable_main_reader_never_falls_back_to_the_line_reader() {
    let o = Origin::new();
    let runs = Cell::new(0);
    let staged = fixtures::stage(
        &targo(),
        &o.work,
        &o.stage_root(),
        &pins(7),
        &mut generator(&["history"], &runs),
    )
    .unwrap();
    let landed = fixtures::land(
        &Git::new(&o.work),
        &native(&o),
        &staged,
        &pins(7),
        &mut |_| fixtures::Reading {
            checked: None,
            answers: Err("main compile failed".into()),
            checks: "COULD NOT RUN: main compile failed".into(),
        },
    )
    .unwrap();
    assert_eq!(
        o.on_main(fixtures::SHIPPED_STATE_CORPUS),
        corpus().as_bytes()
    );
    assert!(matches!(
        landed,
        Landed::Pushed {
            answers: Answers::Unrecorded(_),
            ..
        }
    ));
    assert!(landed.words(&pins(7)).contains("main compile failed"));
    assert!(
        !o.on_main(&format!(
            "{HANDOFF_FIXTURE_ROOT}/v0.6.0/history/parent.toml"
        ))
        .is_empty()
    );
}

#[test]
fn main_reader_field_hold_names_a_stale_row_and_lands_the_facts() {
    let row = "meta CheckpointMeta.program_state_began (listed, but no desk reaches it)";
    for outcome in ["pass", "red", "missing", "unavailable"] {
        let o = Origin::new();
        let staged = fixtures::stage(
            &targo(),
            &o.work,
            &o.stage_root(),
            &pins(7),
            &mut generator(&["history"], &Cell::new(0)),
        )
        .unwrap();
        // The producer has retired the field in every case. The command seam
        // models main with the stale list entry, then with that entry removed.
        let fields = "[meta]\nwritten = []\n";
        fs::write(staged.dir.join("fields.toml"), fields).unwrap();
        let (mut compiles, mut holds, mut consumers) = (0, 0, 0);
        let landed = fixtures::land(&Git::new(&o.work), &native(&o), &staged, &pins(7), &mut |preview| {
            fixtures::MainReader::new(&o.work).unwrap().read(&targo(), preview, &pins(7), &mut |cmd| {
                let tree = cmd.get_current_dir().unwrap();
                let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
                let mut status = exit(0);
                let stdout = if args.iter().any(|a| a == "--no-run") {
                    compiles += 1;
                    artifact(tree, &o.work.join("target/debug/deps/aterm_gui-0123456789abcdef"))
                } else if args.iter().any(|a| a == fixtures::FIELD_HOLD_TEST) {
                    holds += 1;
                    assert!(args.iter().any(|a| a == "--ignored"));
                    assert!(args.iter().any(|a| a == "--exact"));
                    let release = PathBuf::from(env_of(cmd, fixtures::FIELD_HOLD_RELEASE).unwrap());
                    assert_eq!(release, tree.join(HANDOFF_FIXTURE_ROOT).join("v0.6.0"));
                    assert_eq!(fs::read_to_string(release.join("fields.toml")).unwrap(), fields);
                    assert_eq!(Path::new(cmd.get_program()), o.work.join("target/debug/deps/aterm_gui-0123456789abcdef"));
                    if outcome == "unavailable" {
                        return Err(std::io::Error::other("field hold did not start"));
                    } else if outcome == "missing" {
                        "test result: ok. 0 passed; 0 failed; 0 ignored\n".into()
                    } else if outcome == "red" {
                        status = exit(101);
                        format!("{row}\ntest {} ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored\n", fixtures::FIELD_HOLD_TEST)
                    } else {
                        "test result: ok. 1 passed; 0 failed; 0 ignored\n".into()
                    }
                } else if args.iter().any(|a| a == "--ignored") {
                    fs::write(env_of(cmd, fixtures::ANSWERS_OUT).unwrap(), staged_answers(&["history"])).unwrap();
                    "test result: ok. 1 passed; 0 failed; 0 ignored\n".into()
                } else {
                    consumers += 1;
                    "test result: ok. 2 passed; 0 failed; 0 ignored\n".into()
                };
                Ok(Output { status, stdout: stdout.into_bytes(), stderr: Vec::new() })
            })
        }).unwrap();
        assert_eq!((compiles, holds, consumers), (1, 1, 1));
        let words = landed.words(&pins(7));
        println!(
            "field-hold {outcome}: {compiles} compile, {holds} hold, {consumers} consumer; {words}"
        );
        assert!(
            words.contains(match outcome {
                "pass" => "field hold: PASS",
                "unavailable" => "field hold: COULD NOT RUN",
                _ => "field hold: FAIL",
            }),
            "{words}"
        );
        assert_eq!(words.contains(row), outcome == "red", "{words}");
        let message = git(&o.work, &["show", "-s", "--format=%B", "origin/main"]);
        assert_eq!(message.contains(row), outcome == "red", "{message}");
        assert_eq!(
            o.on_main(&format!("{HANDOFF_FIXTURE_ROOT}/v0.6.0/fields.toml")),
            fields.as_bytes()
        );
        assert_eq!(
            o.on_main(fixtures::SHIPPED_STATE_CORPUS),
            staged_answers(&["history"]).as_bytes()
        );
    }
}

#[test]
fn main_reader_records_then_checks_the_splice_without_blessing_old_answers() {
    for red in [false, true] {
        let o = Origin::new();
        let runs = Cell::new(0);
        let staged = fixtures::stage(
            &targo(),
            &o.work,
            &o.stage_root(),
            &pins(7),
            &mut generator(&["history"], &runs),
        )
        .unwrap();
        let mut calls = 0;
        let rendered = staged_answers(&["history"]).replace("fnv1a64:6", "fnv1a64:MAIN");
        let expected = rendered.clone();
        let landed = fixtures::land(&Git::new(&o.work), &native(&o), &staged, &pins(7), &mut |preview| {
            fixtures::MainReader::new(&o.work).unwrap().read(&targo(), preview, &pins(7), &mut |cmd| {
                if let Some(out) = field_hold_pass(cmd) { return Ok(out); }
                calls += 1;
                let tree = cmd.get_current_dir().unwrap();
                let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
                let stdout = match calls {
                    1 => {
                        assert!(args.iter().any(|a| a == "--no-run"));
                        artifact(tree, &o.work.join("target/debug/deps/aterm_gui-0123456789abcdef"))
                    }
                    2 => {
                        assert!(args.iter().any(|a| a == "--ignored"));
                        assert!(args.iter().any(|a| a == "serde_corpus_tests::record_shipped_state_for_landing"));
                        assert_eq!(fs::read_to_string(tree.join(fixtures::SHIPPED_STATE_CORPUS)).unwrap(), corpus());
                        // Main also changed an old answer. It must remain an oracle,
                        // not be silently blessed along with the new producer's cases.
                        fs::write(env_of(cmd, fixtures::ANSWERS_OUT).unwrap(), rendered.replace("fnv1a64:5", "fnv1a64:OLD-MOVED")).unwrap();
                        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n".into()
                    }
                    3 => {
                        assert_eq!(args, fixtures::consumer_args());
                        assert_eq!(fs::read_to_string(tree.join(fixtures::SHIPPED_STATE_CORPUS)).unwrap(), expected);
                        assert!(cmd.get_envs().any(|(k, v)| k == "ATERM_BLESS_SERDE_CORPUS" && v.is_none()));
                        if red {
                            "test restore_claude_auth_tests::new_route ... FAILED\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n".into()
                        } else {
                            "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n".into()
                        }
                    }
                    _ => panic!("a consumer FAIL must not be re-rolled"),
                };
                Ok(Output { status: exit(i32::from(red && calls == 3)), stdout: stdout.into_bytes(), stderr: Vec::new() })
            })
        }).unwrap();
        assert_eq!(calls, 3, "{}", landed.words(&pins(7)));
        assert_eq!(
            o.on_main(fixtures::SHIPPED_STATE_CORPUS),
            expected.as_bytes()
        );
        assert!(landed.words(&pins(7)).contains(if red {
            "FAIL: restore_claude_auth_tests::new_route"
        } else {
            "PASS (2 desk-reader tests)"
        }));
        assert_eq!(
            git(&o.work, &["worktree", "list", "--porcelain"])
                .lines()
                .filter(|l| l.starts_with("worktree "))
                .count(),
            2
        );
    }
}

/// Exercise the real after-cut reader without publishing or rewriting any producer.
/// Remove one release's answers from a private git tree, then have main restore them.
#[test]
#[ignore = "measures a real GUI compile and consumer sweep; set ATERM_MEASURE_SHIPPED_VERSION"]
fn measure_main_fixture_reader() {
    let version = std::env::var("ATERM_MEASURE_SHIPPED_VERSION")
        .expect("name an existing shipped fixture version");
    let repo = aterm_tempfile::manifest_dir()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let git = Git::new(&repo);
    let capture = aterm_tempfile::tempdir().unwrap();
    let mut run = |cmd: &mut Command| {
        if cmd.get_args().any(|arg| arg == "--no-run")
            && let Some(dir) = std::env::var_os("ATERM_MEASURE_READER_BUILD_DIR")
        {
            cmd.arg("--config")
                .arg(format!("build.build-dir={:?}", PathBuf::from(dir)));
        }
        let stdout = capture.path().join("stdout");
        let stderr = capture.path().join("stderr");
        cmd.stdout(fs::File::create(&stdout)?)
            .stderr(fs::File::create(&stderr)?);
        let started = std::time::Instant::now();
        let mut child = cmd.spawn()?;
        let usage = child.wait_with_rusage()?;
        println!(
            "reader-child {:?}: wall {:.3}s, CPU {:.3}s",
            cmd.get_args().collect::<Vec<_>>(),
            started.elapsed().as_secs_f64(),
            usage.cpu.as_secs_f64()
        );
        Ok(Output {
            status: usage.status,
            stdout: fs::read(stdout)?,
            stderr: fs::read(stderr)?,
        })
    };
    let mut before = std::env::var("ATERM_MEASURE_MAIN_BASE").unwrap_or_else(|_| "HEAD".into());
    let regression = std::env::var_os("ATERM_MEASURE_READER_REGRESSION").is_some();
    let hold_source = "crates/aterm-gui/src/seamless_fixture_generator_tests.rs";
    let mut repaired_hold = None;
    if regression {
        let source = crate::ledger::git_ok(&git, &["show", &format!("{before}:{hold_source}")])
            .unwrap()
            .stdout_utf8();
        let mutant = source.replacen("fn check_landed_handoff_fields() {", "fn check_landed_handoff_fields() {\n    assert!(false, \"planted prebuilt reader regression\");", 1);
        assert_ne!(source, mutant);
        repaired_hold = Some(source);
        let blob = git
            .git_input(&["hash-object", "-w", "--stdin"], mutant.as_bytes())
            .unwrap();
        assert!(blob.success());
        let tree = fixtures::tree_with(
            &git,
            Some(&crate::ledger::rev_parse(&git, &format!("{before}^{{tree}}")).unwrap()),
            &[(hold_source, blob.stdout_utf8().trim())],
        )
        .unwrap();
        before = crate::ledger::git_ok(
            &git,
            &[
                "commit-tree",
                &tree,
                "-p",
                &before,
                "-m",
                "measurement planted reader regression",
            ],
        )
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned();
    }
    let corpus = crate::ledger::git_ok(
        &git,
        &[
            "show",
            &format!("{before}:{}", fixtures::SHIPPED_STATE_CORPUS),
        ],
    )
    .unwrap()
    .stdout_utf8();
    let prefix = format!("v{version}/");
    let without = corpus
        .lines()
        .filter(|l| !l.starts_with(&prefix))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_ne!(without, corpus);
    let blob = git
        .git_input(&["hash-object", "-w", "--stdin"], without.as_bytes())
        .unwrap();
    assert!(blob.success());
    let base = crate::ledger::rev_parse(&git, &format!("{before}^{{tree}}")).unwrap();
    let tree = fixtures::tree_with(
        &git,
        Some(&base),
        &[(fixtures::SHIPPED_STATE_CORPUS, blob.stdout_utf8().trim())],
    )
    .unwrap();
    let preview = crate::ledger::git_ok(
        &git,
        &[
            "commit-tree",
            &tree,
            "-p",
            &before,
            "-m",
            "measure main fixture reader (unreferenced)",
        ],
    )
    .unwrap()
    .stdout_utf8();
    let mut reader = fixtures::MainReader::new(&repo).unwrap();
    if std::env::var_os("ATERM_MEASURE_MAIN_PREBUILD").is_some() {
        let started = std::time::Instant::now();
        let result = reader.prebuild(&targo(), preview.trim(), &mut run);
        println!(
            "prebuild: {:.3}s {result:?}",
            started.elapsed().as_secs_f64()
        );
        assert!(
            result.is_ok(),
            "measurement needs a completed prebuild: {result:?}"
        );
    }
    let mut preview = preview;
    if std::env::var_os("ATERM_MEASURE_READER_CHURN").is_some() {
        // A source-only reader-cone edit: the fixture edit remains disjoint.
        let path = "crates/aterm-gui/src/lib.rs";
        let source = crate::ledger::git_ok(&git, &["show", &format!("{before}:{path}")])
            .unwrap()
            .stdout_utf8();
        let blob = git
            .git_input(
                &["hash-object", "-w", "--stdin"],
                format!("{source}\n// Reader churn measurement.\n").as_bytes(),
            )
            .unwrap();
        assert!(blob.success());
        let mut edits = vec![(path, blob.stdout_utf8().trim().to_owned())];
        if let Some(source) = &repaired_hold {
            let blob = git
                .git_input(&["hash-object", "-w", "--stdin"], source.as_bytes())
                .unwrap();
            assert!(blob.success());
            edits.push((hold_source, blob.stdout_utf8().trim().to_owned()));
        }
        let refs: Vec<_> = edits
            .iter()
            .map(|(path, blob)| (*path, blob.as_str()))
            .collect();
        let moved = fixtures::tree_with(&git, Some(&base), &refs).unwrap();
        let tip = crate::ledger::git_ok(
            &git,
            &[
                "commit-tree",
                &moved,
                "-p",
                &before,
                "-m",
                "measurement reader-cone movement",
            ],
        )
        .unwrap()
        .stdout_utf8();
        let moved_preview = fixtures::tree_with(
            &git,
            Some(&moved),
            &[(
                fixtures::SHIPPED_STATE_CORPUS,
                {
                    let blob = git
                        .git_input(&["hash-object", "-w", "--stdin"], without.as_bytes())
                        .unwrap();
                    assert!(blob.success());
                    blob.stdout_utf8().trim().to_owned()
                }
                .as_str(),
            )],
        )
        .unwrap();
        preview = crate::ledger::git_ok(
            &git,
            &[
                "commit-tree",
                &moved_preview,
                "-p",
                tip.trim(),
                "-m",
                "measurement moved fixture preview",
            ],
        )
        .unwrap()
        .stdout_utf8();
    }
    if std::env::var_os("ATERM_MEASURE_READER_RECOMPILE").is_some() {
        let dir = crate::ledger::git_ok(&git, &["rev-parse", "--absolute-git-dir"])
            .unwrap()
            .stdout_utf8();
        fs::remove_file(Path::new(dir.trim()).join(".ubd/main-reader.noindex/prebuilt")).unwrap();
    }
    let reading = reader.read(
        &targo(),
        preview.trim(),
        &Pins {
            version: version.clone(),
            ..pins(7)
        },
        &mut run,
    );
    println!("cold: {}", reading.checks);
    assert!(reading.answers.is_ok(), "{}", reading.checks);
    assert!(
        if regression {
            reading.checks.contains("field hold: FAIL")
                && reading
                    .checks
                    .contains("planted prebuilt reader regression")
                && reading.checks.contains("no compile")
        } else {
            reading.checks.starts_with("PASS")
        },
        "{}",
        reading.checks
    );
    if std::env::var_os("ATERM_MEASURE_MAIN_ONCE").is_some() {
        return;
    }
    // Reopen on current main, possibly many commits later. Rebuild the preview
    // from that tip's own corpus rather than carrying the older tree's answers.
    let corpus = fs::read_to_string(repo.join(fixtures::SHIPPED_STATE_CORPUS)).unwrap();
    let without = corpus
        .lines()
        .filter(|l| !l.starts_with(&prefix))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let blob = git
        .git_input(&["hash-object", "-w", "--stdin"], without.as_bytes())
        .unwrap();
    assert!(blob.success());
    let moved_tree = fixtures::tree_with(
        &git,
        Some(&crate::ledger::rev_parse(&git, "HEAD^{tree}").unwrap()),
        &[(fixtures::SHIPPED_STATE_CORPUS, blob.stdout_utf8().trim())],
    )
    .unwrap();
    let moved = crate::ledger::git_ok(
        &git,
        &[
            "commit-tree",
            &moved_tree,
            "-p",
            preview.trim(),
            "-m",
            "measure a changed main tip (unreferenced)",
        ],
    )
    .unwrap()
    .stdout_utf8();
    drop(reader);
    let mut reader = fixtures::MainReader::new(&repo).unwrap();
    if std::env::var_os("ATERM_MEASURE_MAIN_PREBUILD").is_some() {
        let started = std::time::Instant::now();
        let result = reader.prebuild(&targo(), moved.trim(), &mut run);
        println!(
            "prebuild: {:.3}s {result:?}",
            started.elapsed().as_secs_f64()
        );
    }
    let reading = reader.read(
        &targo(),
        moved.trim(),
        &Pins { version, ..pins(7) },
        &mut run,
    );
    println!("changed tip, warm source: {}", reading.checks);
    assert!(reading.checks.starts_with("PASS"), "{}", reading.checks);
}

#[test]
fn prebuild_fixture_tree_is_the_tree_landing_then_judges() {
    let o = Origin::new();
    let runs = Cell::new(0);
    let staged = fixtures::stage(
        &targo(),
        &o.work,
        &o.stage_root(),
        &pins(7),
        &mut generator(&["history", "twelve-panes"], &runs),
    )
    .unwrap();
    let base = git(&o.work, &["rev-parse", "origin/main^{tree}"]);
    let mut warmed = None;
    fixtures::MainReader::new(&o.work)
        .unwrap()
        .prebuild_fixtures(&targo(), "origin/main", &staged, &pins(7), &mut |cmd| {
            warmed = Some(git(
                cmd.get_current_dir().unwrap(),
                &["rev-parse", "HEAD^{tree}"],
            ));
            Ok(Output {
                status: exit(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        })
        .unwrap();
    let warmed = warmed.unwrap();
    assert_ne!(warmed, base, "bare main omits the staged fixture inputs");
    let mut judged = false;
    fixtures::land(
        &Git::new(&o.work),
        &native(&o),
        &staged,
        &pins(7),
        &mut |preview| {
            judged = true;
            assert_eq!(
                git(&o.work, &["rev-parse", &format!("{preview}^{{tree}}")]),
                warmed
            );
            reader(&staged)(preview)
        },
    )
    .unwrap();
    assert!(judged);
}

#[test]
fn compiled_reader_timelines_execute_the_judged_tree_and_keep_its_field_red() {
    for timeline in [
        "still",
        "moved",
        "merged",
        "touched",
        "diverged",
        "regression",
        "failed",
        "binary-changed",
        "compiler-changed",
        "producer-changed",
    ] {
        let o = Origin::new();
        let old_branch = git(&o.rival, &["rev-parse", "HEAD"]);
        if matches!(timeline, "merged" | "diverged") {
            if timeline == "merged" {
                fs::write(
                    o.rival.join(HANDOFF_FIXTURE_GUARD),
                    format!("{GUARD}\n// baseline fixture update\n"),
                )
                .unwrap();
            } else {
                fs::write(
                    o.rival.join("crates/aterm-gui/src/lib.rs"),
                    "// X's reader change\n",
                )
                .unwrap();
            }
            git(&o.rival, &["add", "-A"]);
            git(
                &o.rival,
                &["commit", "-am", "fixture update before prebuild"],
            );
            git(&o.rival, &["push", "origin", "main"]);
            git(&o.work, &["fetch", "origin"]);
        }
        let staged = fixtures::stage(
            &targo(),
            &o.work,
            &o.stage_root(),
            &pins(7),
            &mut generator(&["history"], &Cell::new(0)),
        )
        .unwrap();
        let binary = o.work.join("target/debug/deps/aterm_gui-0123456789abcdef");
        let mut warmed = String::new();
        if timeline == "failed" {
            // A failed attempt must invalidate an earlier successful index too.
            fixtures::MainReader::new(&o.work)
                .unwrap()
                .prebuild_fixtures(&targo(), "origin/main", &staged, &pins(7), &mut |cmd| {
                    Ok(Output {
                        status: exit(0),
                        stdout: artifact(cmd.get_current_dir().unwrap(), &binary).into_bytes(),
                        stderr: Vec::new(),
                    })
                })
                .unwrap();
        }
        let result = fixtures::MainReader::new(&o.work)
            .unwrap()
            .prebuild_fixtures(&targo(), "origin/main", &staged, &pins(7), &mut |cmd| {
                let tree = cmd.get_current_dir().unwrap();
                warmed = git(tree, &["rev-parse", "HEAD"]);
                Ok(Output {
                    status: exit(i32::from(timeline == "failed")),
                    stdout: artifact(tree, &binary).into_bytes(),
                    stderr: Vec::new(),
                })
            });
        assert_eq!(result.is_err(), timeline == "failed");
        if timeline == "binary-changed" {
            fs::write(&binary, b"another compile").unwrap();
        }
        if timeline == "producer-changed" {
            fs::write(
                staged.dir.join("history/parent.toml"),
                "changed producer bytes\n",
            )
            .unwrap();
        }
        if timeline != "still" && timeline != "failed" && timeline != "binary-changed" {
            if timeline == "touched" {
                let path = o
                    .rival
                    .join(HANDOFF_FIXTURE_ROOT)
                    .join("v0.6.0/history/other");
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, "intervening writer").unwrap();
                git(&o.rival, &["add", "-A"]);
                git(&o.rival, &["commit", "-m", "touch release fixtures"]);
                fs::remove_file(path).unwrap();
            }
            fs::write(o.rival.join("README.md"), "main moved\n").unwrap();
            if matches!(timeline, "moved" | "regression") {
                fs::write(
                    o.rival.join("crates/aterm-gui/src/lib.rs"),
                    "// reader cone moved\n",
                )
                .unwrap();
            }
            git(&o.rival, &["add", "-A"]);
            git(&o.rival, &["commit", "-m", "advance main"]);
            if timeline == "merged" {
                git(&o.rival, &["checkout", "-b", "peer", old_branch.trim()]);
                fs::write(
                    o.rival.join("crates/aterm-gui/src/lib.rs"),
                    "// peer reader change\n",
                )
                .unwrap();
                git(&o.rival, &["add", "-A"]);
                git(
                    &o.rival,
                    &["commit", "-m", "peer code change on older base"],
                );
                git(
                    &o.rival,
                    &[
                        "merge",
                        "--no-ff",
                        "main",
                        "-m",
                        "merge disjoint older peer",
                    ],
                );
                // Workers merge moving main into their branch, then land that
                // merge: X can be reachable through its second parent.
                git(&o.rival, &["checkout", "main"]);
                git(&o.rival, &["merge", "--ff-only", "peer"]);
            }
            if timeline == "diverged" {
                // A sibling from before X has identical fixture inputs. Unlike
                // an orphan root, its history does not add the fixture files,
                // so only the ancestry guard can disqualify it.
                git(
                    &o.rival,
                    &["checkout", "-b", "rewritten", old_branch.trim()],
                );
                fs::write(o.rival.join("README.md"), "rewritten main\n").unwrap();
                git(&o.rival, &["commit", "-am", "a divergent sibling"]);
                let unrelated = git(&o.rival, &["rev-parse", "HEAD"]);
                git(
                    &o.dir.path().join("origin.git"),
                    &["fetch", "../rival", unrelated.trim()],
                );
                git(
                    &o.dir.path().join("origin.git"),
                    &["update-ref", "refs/heads/main", unrelated.trim()],
                );
            } else {
                git(&o.rival, &["push", "origin", "main"]);
            }
        }
        let mut compiles = 0;
        let mut holds = 0;
        let reuse = matches!(timeline, "still" | "moved" | "merged" | "regression");
        let mut judged = String::new();
        let compiler = if timeline == "compiler-changed" {
            o.work.join("other-targo")
        } else {
            targo()
        };
        let landed = fixtures::land(&Git::new(&o.work), &native(&o), &staged, &pins(7), &mut |preview| {
            // Reopen: the cut and after-cut do not share a MainReader object.
            let mut reader = fixtures::MainReader::new(&o.work).unwrap();
            let mut run = |cmd: &mut Command| {
                let tree = cmd.get_current_dir().unwrap();
                judged = git(tree, &["rev-parse", "HEAD"]);
                if cmd.get_args().any(|a| a == "--no-run") {
                    compiles += 1;
                    return Ok(Output { status: exit(0), stdout: artifact(tree, &binary).into_bytes(), stderr: Vec::new() });
                }
                if let Some(out) = field_hold_pass(cmd) {
                    holds += 1;
                    if timeline == "regression" && holds == 1 {
                        assert_eq!(judged, warmed, "composition must execute X's reader");
                        return Ok(Output { status: exit(1), stdout: b"test planted_reader_regression ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored\n".to_vec(), stderr: Vec::new() });
                    }
                    return Ok(out);
                }
                if let Some(path) = env_of(cmd, fixtures::ANSWERS_OUT) {
                    fs::write(path, staged_answers(&["history"])).unwrap();
                }
                Ok(Output { status: exit(0), stdout: b"test result: ok. 1 passed; 0 failed; 0 ignored\n".to_vec(), stderr: Vec::new() })
            };
            let first = reader.read(&compiler, preview, &pins(7), &mut run);
            if timeline == "regression" {
                let second = reader.read(&compiler, preview, &pins(7), &mut run);
                assert!(second.checks.contains("earlier main reader"), "{}", second.checks);
                second
            } else { first }
        }).unwrap();
        assert_eq!(holds, if timeline == "regression" { 2 } else { 1 });
        assert_eq!(
            compiles,
            usize::from(!reuse),
            "{timeline}: {}",
            landed.words(&pins(7))
        );
        if reuse {
            assert_eq!(judged, warmed, "{timeline}");
            let message = git(&o.work, &["show", "-s", "--format=%B", "origin/main"]);
            assert!(message.contains(&format!("Fixture-Reader-Checked: {}", warmed.trim())));
        }
        assert!(
            landed
                .words(&pins(7))
                .contains(if timeline == "regression" {
                    "field hold: FAIL"
                } else {
                    "field hold: PASS"
                })
        );
        assert!(
            landed
                .words(&pins(7))
                .contains(if reuse { "composed onto" } else { "compiled " })
        );
    }
}

#[test]
fn incomplete_or_failed_prebuild_never_hides_the_preview_compile_red() {
    for prebuild_success in [true, false] {
        let o = Origin::new();
        let mut reader = fixtures::MainReader::new(&o.work).unwrap();
        let mut warmed = false;
        let result = reader.prebuild(&targo(), "HEAD", &mut |cmd| {
            warmed = true;
            assert!(cmd.get_args().any(|arg| arg == "--no-run"));
            Ok(Output {
                status: exit(if prebuild_success { 0 } else { 1 }),
                stdout: Vec::new(),
                stderr: b"warm-up error".to_vec(),
            })
        });
        assert!(warmed);
        assert_eq!(result.is_ok(), prebuild_success);
        fs::write(o.work.join("reader-input"), "preview only").unwrap();
        git(&o.work, &["add", "reader-input"]);
        git(&o.work, &["commit", "-m", "change preview reader inputs"]);
        let preview = git(&o.work, &["rev-parse", "HEAD"]);
        let mut compiled = false;
        let reading = reader.read(&targo(), preview.trim(), &pins(7), &mut |cmd| {
            compiled = true;
            assert_eq!(
                fs::read_to_string(cmd.get_current_dir().unwrap().join("reader-input")).unwrap(),
                "preview only"
            );
            Ok(Output {
                status: exit(1),
                stdout: Vec::new(),
                stderr: b"error: preview compile negative control".to_vec(),
            })
        });
        assert!(
            compiled,
            "an incomplete prebuild cannot replace the preview compile"
        );
        assert!(reading.answers.is_err());
        assert!(
            reading.checks.contains("preview compile negative control"),
            "{}",
            reading.checks
        );
    }
}

#[test]
fn main_reader_reopens_warm_but_never_reuses_previous_answers_or_tree() {
    let o = Origin::new();
    let mut previous = None;
    for take in 0..2 {
        fs::write(o.work.join("reader-input"), format!("tip {take}\n")).unwrap();
        git(&o.work, &["add", "reader-input"]);
        git(&o.work, &["commit", "-m", "move reader input"]);
        let tip = git(&o.work, &["rev-parse", "HEAD"]);
        let mut reader = fixtures::MainReader::new(&o.work).unwrap();
        assert!(
            fixtures::MainReader::new(&o.work).is_err(),
            "workspace must be leased"
        );
        let mut calls = 0;
        let binary = o
            .work
            .join(format!("target/debug/deps/aterm_gui-{take:016x}"));
        fs::write(&binary, format!("compiled tip {take}")).unwrap();
        let reading = reader.read(&targo(), tip.trim(), &pins(7), &mut |cmd| {
            if let Some(out) = field_hold_pass(cmd) {
                return Ok(out);
            }
            calls += 1;
            let tree = cmd.get_current_dir().unwrap();
            assert_eq!(
                fs::read_to_string(tree.join("reader-input")).unwrap(),
                format!("tip {take}\n")
            );
            let stdout = match calls {
                1 => {
                    assert_eq!(
                        fs::read_to_string(tree.join(fixtures::SHIPPED_STATE_CORPUS)).unwrap(),
                        corpus()
                    );
                    assert!(!tree.join("interrupted-desk").exists());
                    if let Some(path) = &previous {
                        assert_eq!(tree, path);
                    }
                    previous = Some(tree.to_path_buf());
                    artifact(tree, &binary)
                }
                2 => {
                    assert_eq!(cmd.get_program(), binary.as_os_str());
                    let output = env_of(cmd, fixtures::ANSWERS_OUT).unwrap();
                    assert!(
                        !Path::new(&output).exists(),
                        "previous answers must be removed"
                    );
                    if take == 0 {
                        fs::write(output, staged_answers(&["history"])).unwrap();
                    }
                    "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
                        .into()
                }
                3 => {
                    fs::write(tree.join("interrupted-desk"), "stale").unwrap();
                    "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
                        .into()
                }
                _ => unreachable!(),
            };
            Ok(Output {
                status: exit(0),
                stdout: stdout.into_bytes(),
                stderr: Vec::new(),
            })
        });
        if take == 0 {
            assert!(reading.checks.starts_with("PASS"), "{}", reading.checks);
        } else {
            // Negative control: a successful recorder that writes nothing cannot
            // borrow the preceding release's answers or reach the consumer tests.
            assert!(reading.answers.is_err());
            assert!(
                reading.checks.contains("main reader answers:"),
                "{}",
                reading.checks
            );
            assert_eq!(calls, 2);
        }
    }
}

#[test]
fn an_unproven_main_recorder_is_held_before_another_compile() {
    use std::os::unix::process::ExitStatusExt as _;
    let o = Origin::new();
    let runs = Cell::new(0);
    let staged = fixtures::stage(
        &targo(),
        &o.work,
        &o.stage_root(),
        &pins(7),
        &mut generator(&["history"], &runs),
    )
    .unwrap();
    let mut calls = 0;
    let landed = fixtures::land(
        &Git::new(&o.work),
        &native(&o),
        &staged,
        &pins(7),
        &mut |preview| {
            let mut reader = fixtures::MainReader::new(&o.work).unwrap();
            let first = reader.read(&targo(), preview, &pins(7), &mut |cmd| {
                calls += 1;
                Ok(if calls == 1 {
                    Output {
                        status: exit(0),
                        stdout: artifact(
                            cmd.get_current_dir().unwrap(),
                            &o.work.join("target/debug/deps/aterm_gui-0123456789abcdef"),
                        )
                        .into_bytes(),
                        stderr: Vec::new(),
                    }
                } else {
                    assert_eq!(calls, 2);
                    Output {
                        status: std::process::ExitStatus::from_raw(9),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    }
                })
            });
            assert!(first.answers.is_err());
            drop(reader);
            let mut reader = fixtures::MainReader::new(&o.work).unwrap();
            let held = reader.read(&targo(), preview, &pins(7), &mut |_| {
                panic!("must not replace an unproven executable")
            });
            assert!(
                held.checks.contains("never showed it ran"),
                "{}",
                held.checks
            );
            held
        },
    )
    .unwrap();
    assert_eq!(calls, 2);
    assert!(matches!(
        landed,
        Landed::Pushed {
            answers: Answers::Unrecorded(_),
            ..
        }
    ));
    assert_eq!(
        o.on_main(fixtures::SHIPPED_STATE_CORPUS),
        corpus().as_bytes()
    );
}

impl GitRunner for Raced<'_> {
    fn git(&self, args: &[&str]) -> crate::ledger::Result<RunOut> {
        assert!(
            !args
                .iter()
                .any(|word| matches!(*word, "fetch" | "push" | "ls-remote")),
            "fixture landing must use typed Origin: {args:?}"
        );
        self.inner.git(args)
    }

    fn git_input(&self, args: &[&str], input: &[u8]) -> crate::ledger::Result<RunOut> {
        self.inner.git_input(args, input)
    }
}

impl OriginTransport for Raced<'_> {
    fn refresh_main(&self) -> Result<String, String> {
        self.refreshes.set(self.refreshes.get() + 1);
        self.remote.refresh_main()
    }
    fn write(
        &self,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<RefWrite, String> {
        self.pushes
            .borrow_mut()
            .push(format!("{reference} {before:?} {after}"));
        if self.remaining.get() > 0
            && let Some(rival) = self.rival
        {
            let left = self.remaining.get();
            self.remaining.set(left - 1);
            let path = rival.join(self.path);
            let old = fs::read_to_string(&path).unwrap();
            fs::write(path, format!("// peer move {left}\n{old}")).unwrap();
            git(rival, &["commit", "-q", "-am", "a peer's change"]);
            if self.movement == Movement::Revert {
                fs::write(rival.join(self.path), old).unwrap();
                git(rival, &["commit", "-q", "-am", "revert peer's edit"]);
            }
            if self.movement == Movement::Rewrite {
                let tree = git(rival, &["rev-parse", "HEAD^{tree}"]);
                let root = git(
                    rival,
                    &["commit-tree", tree.trim(), "-m", "unrelated peer root"],
                );
                let bare = rival.parent().unwrap().join("origin.git");
                git(&bare, &["fetch", "../rival", root.trim()]);
                git(&bare, &["update-ref", "refs/heads/main", root.trim()]);
            } else {
                git(rival, &["push", "-q", "origin", "main"]);
            }
        }
        let outcome = OriginTransport::write(&self.remote, reference, before, after)?;
        if self.lose_reply {
            assert_eq!(
                outcome,
                RefWrite::Written,
                "the native fixture really writes before losing its reply"
            );
            return Err("outcome unknown; side_effect=unknown; [rejected] non-fast-forward".into());
        }
        Ok(outcome)
    }
    fn get(&self, _: &str) -> Result<Option<String>, String> {
        Err("fixture landing only refreshes main".into())
    }
    fn import(&self, _: &str) -> Result<(), String> {
        Err("main refresh imports its exact source".into())
    }
    fn peel(&self, _: &str) -> Result<Option<String>, String> {
        Err("fixture landing reads no tags".into())
    }
    fn tag_object(&self, _: &str) -> Result<Vec<u8>, String> {
        Err("fixture landing reads no tags".into())
    }
    fn delete(&self, _: &str, _: &str) -> Result<RefWrite, String> {
        Err("fixture landing deletes no refs".into())
    }
    fn control_transaction(
        &self,
        _: crate::ledger::ControlRefTransaction,
    ) -> Result<RefWrite, String> {
        Err("fixture landing changes no controls".into())
    }
}

#[test]
fn fixture_races_compose_unrelated_moves_and_recheck_overlapping_files() {
    for (path, moves, expected_reads, movement) in [
        ("README.md", 8, 1, Movement::Edit),
        (HANDOFF_FIXTURE_GUARD, 1, 2, Movement::Edit),
        (HANDOFF_FIXTURE_GUARD, 1, 2, Movement::Revert),
        ("README.md", 1, 2, Movement::Rewrite),
    ] {
        let o = Origin::new();
        let staged = fixtures::stage(
            &targo(),
            &o.work,
            &o.stage_root(),
            &pins(7),
            &mut generator(&["history"], &Cell::new(0)),
        )
        .unwrap();
        let raced = Raced {
            inner: Git::new(&o.work),
            remote: native(&o),
            rival: Some(&o.rival),
            remaining: Cell::new(moves),
            pushes: RefCell::new(Vec::new()),
            refreshes: Cell::new(0),
            lose_reply: false,
            path,
            movement,
        };
        let mut reads = 0;
        let start = std::time::Instant::now();
        let mut checked = String::new();
        let result = fixtures::land(&raced, &raced, &staged, &pins(7), &mut |preview| {
            reads += 1;
            if reads == 1 {
                checked = preview.to_owned();
            }
            assert!(
                git(
                    &o.work,
                    &["show", &format!("{preview}:{HANDOFF_FIXTURE_GUARD}")]
                )
                .contains("v0.6.0")
            );
            if reads > 1 && movement != Movement::Revert {
                assert!(
                    git(&o.work, &["show", &format!("{preview}:{path}")]).contains("peer move")
                );
            }
            let mut reading = reader(&staged)(preview);
            reading.checks = "FAIL: retained reader verdict".into();
            reading
        });
        println!(
            "fixture race path={path} moves={moves} reads={reads} writes={} wall={:.3}s result={result:?}",
            raced.pushes.borrow().len(),
            start.elapsed().as_secs_f64()
        );
        let landed = result.unwrap();
        assert_eq!(reads, expected_reads);
        assert_eq!(raced.pushes.borrow().len(), moves as usize + 1);
        assert_eq!(raced.remaining.get(), 0);
        assert!(
            landed
                .words(&pins(7))
                .contains("FAIL: retained reader verdict")
        );
        assert!(matches!(landed, Landed::Pushed { attempt, .. } if attempt == expected_reads));
        assert_eq!(
            String::from_utf8(o.on_main(path))
                .unwrap()
                .contains("peer move"),
            movement != Movement::Revert
        );
        assert_eq!(
            o.on_main(&format!(
                "{HANDOFF_FIXTURE_ROOT}/v0.6.0/history/parent.toml"
            )),
            fs::read(staged.dir.join("history/parent.toml")).unwrap()
        );
        if expected_reads == 1 {
            let message = git(&o.work, &["show", "-s", "--format=%B", "origin/main"]);
            assert!(message.contains(&format!("Fixture-Reader-Checked: {checked}")));
            assert!(message.contains("Fixture-Composed-Onto:"));
        }
    }
}

#[test]
fn a_fixture_merge_conflict_stops_without_overwriting_the_winner() {
    let o = Origin::new();
    let staged = fixtures::stage(
        &targo(),
        &o.work,
        &o.stage_root(),
        &pins(7),
        &mut generator(&["history"], &Cell::new(0)),
    )
    .unwrap();
    let remote = native(&o);
    let mut reads = 0;
    let result = fixtures::land(
        &Git::new(&o.work),
        &remote,
        &staged,
        &pins(7),
        &mut |preview| {
            reads += 1;
            let path = o.rival.join(HANDOFF_FIXTURE_GUARD);
            fs::write(
                path,
                GUARD.replace(
                    "    (\"v0.5.0\", \"history\"),",
                    "    (\"v0.5.0\", \"other\"),",
                ),
            )
            .unwrap();
            git(&o.rival, &["commit", "-qam", "conflicting fixture rows"]);
            git(&o.rival, &["push", "-q", "origin", "main"]);
            reader(&staged)(preview)
        },
    );
    assert!(result.unwrap_err().to_string().contains("merge-tree"));
    assert_eq!(reads, 1);
    assert!(
        !String::from_utf8(o.on_main(HANDOFF_FIXTURE_GUARD))
            .unwrap()
            .contains("v0.6.0")
    );
}

#[test]
fn an_unknown_fixture_write_never_retries_or_rereads_as_a_conflict() {
    let o = Origin::new();
    let runs = Cell::new(0);
    let staged = fixtures::stage(
        &targo(),
        &o.work,
        &o.stage_root(),
        &pins(7),
        &mut generator(&["history"], &runs),
    )
    .unwrap();
    let answers = fs::read(&staged.answers).unwrap();
    let unknown = Raced {
        inner: Git::new(&o.work),
        remote: native(&o),
        rival: None,
        remaining: Cell::new(1),
        pushes: RefCell::new(Vec::new()),
        refreshes: Cell::new(0),
        lose_reply: true,
        path: "README.md",
        movement: Movement::Edit,
    };
    // The fixture performs the real native write and withholds only the caller's
    // reply. Adapter journal persistence is qualified separately; this control
    // establishes the landing algorithm's treatment of an uncertain result.
    let error =
        fixtures::land(&unknown, &unknown, &staged, &pins(7), &mut reader(&staged)).unwrap_err();
    assert!(error.to_string().contains("outcome unknown"), "{error}");
    assert_eq!(unknown.pushes.borrow().len(), 1);
    assert_eq!(
        unknown.refreshes.get(),
        1,
        "no retry or post-write read can reinterpret unknown"
    );
    assert_eq!(
        o.on_main(&format!(
            "{HANDOFF_FIXTURE_ROOT}/v0.6.0/history/parent.toml"
        )),
        fs::read(staged.dir.join("history/parent.toml")).unwrap(),
        "the withheld native write really landed"
    );
    assert_eq!(
        fs::read(&staged.answers).unwrap(),
        answers,
        "staging remains unchanged"
    );
}

#[test]
fn the_rows_are_appended_to_the_table_and_a_pinned_release_is_refused() {
    let out = fixtures::with_pinned_rows(GUARD, "0.6.0", &["history".to_string()]).unwrap();
    assert!(
        out.contains("    (\"v0.5.0\", \"history\"),\n    (\"v0.6.0\", \"history\"),\n];"),
        "{out}"
    );
    assert!(out.ends_with(
        "const REQUIRED_DESKS: &[(&str, &str)] = &[\n    (\"v0.5.0\", \"history\"),\n];\n"
    ));
    fixtures::with_pinned_rows(&out, "0.6.0", &["history".to_string()]).unwrap_err();
}

#[test]
fn a_fixture_directory_on_main_the_gate_refuses_is_never_overwritten() {
    let o = Origin::new();
    let dir = o.rival.join(HANDOFF_FIXTURE_ROOT).join("v0.6.0/history");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("parent.toml"), "by hand\n").unwrap();
    git(&o.rival, &["add", "-A"]);
    git(&o.rival, &["commit", "-q", "-m", "half"]);
    git(&o.rival, &["push", "-q", "origin", "main"]);
    let runs = Cell::new(0);
    let mut run = generator(&["history"], &runs);
    let staged = fixtures::stage(&targo(), &o.work, &o.stage_root(), &pins(7), &mut run).unwrap();
    fixtures::land(
        &Git::new(&o.work),
        &native(&o),
        &staged,
        &pins(7),
        &mut reader(&staged),
    )
    .unwrap_err();
    assert_eq!(
        o.on_main(&format!(
            "{HANDOFF_FIXTURE_ROOT}/v0.6.0/history/parent.toml"
        )),
        b"by hand\n"
    );
}

#[test]
fn the_release_answers_go_after_the_case_they_follow_and_a_corpus_that_cannot_take_them_is_named() {
    let staged = staged_answers(&["history", "twelve-panes"]);
    let (merged, cases) = fixtures::with_release_answers(&corpus(), &staged, "v0.6.0").unwrap();
    assert_eq!((merged.as_str(), cases), (staged.as_str(), 2));

    let moved = corpus().replace("fnv1a64:5 len=5", "fnv1a64:55 len=55");
    let (merged, _) = fixtures::with_release_answers(&moved, &staged, "v0.6.0").unwrap();
    assert_eq!(
        merged,
        format!(
            "{HEADER}v0.5.0/history/manifest\tok\tfnv1a64:55 len=55\n\
             v0.6.0/history/manifest\tok\tfnv1a64:6 len=6\n\
             v0.6.0/twelve-panes/manifest\tok\tfnv1a64:6 len=6\n\
             negative-control/torn-manifest\terr\tTOML parse error\n"
        ),
        "main's answer kept, the release's after it"
    );

    let refused = |corpus: &str, staged: &str| {
        fixtures::with_release_answers(corpus, staged, "v0.6.0")
            .unwrap_err()
            .to_string()
    };
    assert!(refused(&staged, &staged).contains("records v0.6.0 already"));
    let lost = corpus().replace("v0.5.0/history/manifest\tok\tfnv1a64:5 len=5\n", "");
    assert!(refused(&lost, &staged).contains("holds 0 lines with the case id they go after"));
    assert!(refused(&corpus(), &corpus()).contains("hold no case of its desks"));
    let apart = staged.replacen("v0.6.0/twelve-panes/manifest\tok\tfnv1a64:6 len=6\n", "", 1)
        + "v0.6.0/twelve-panes/manifest\tok\tfnv1a64:6 len=6\n";
    assert!(refused(&corpus(), &apart).contains("apart, not as one block"));
    let first = format!("v0.6.0/history/manifest\tok\tfnv1a64:6 len=6\n{}", corpus());
    assert!(refused(&corpus(), &first).contains("before the file's header"));
}

#[test]
fn a_landing_whose_corpus_cannot_take_the_answers_lands_the_desks_and_says_so() {
    let o = Origin::new();
    let answers = o.rival.join(fixtures::SHIPPED_STATE_CORPUS);
    let lost = corpus().replace("v0.5.0/history/manifest\tok\tfnv1a64:5 len=5\n", "");
    fs::write(&answers, &lost).unwrap();
    git(&o.rival, &["commit", "-q", "-am", "a peer's re-record"]);
    git(&o.rival, &["push", "-q", "origin", "main"]);
    let runs = Cell::new(0);
    let mut run = generator(&["history"], &runs);
    let staged = fixtures::stage(&targo(), &o.work, &o.stage_root(), &pins(7), &mut run).unwrap();
    let landed = fixtures::land(
        &Git::new(&o.work),
        &native(&o),
        &staged,
        &pins(7),
        &mut reader(&staged),
    )
    .unwrap();
    let Landed::Pushed { answers, .. } = &landed else {
        panic!("{landed:?}");
    };
    assert!(
        matches!(answers, Answers::Unrecorded(why) if why.contains("the case id they go after")),
        "{answers:?}"
    );
    assert!(
        landed
            .words(&pins(7))
            .contains("WITHOUT its shipped-state answers"),
        "{}",
        landed.words(&pins(7))
    );
    assert_eq!(
        String::from_utf8(o.on_main(fixtures::SHIPPED_STATE_CORPUS)).unwrap(),
        lost
    );
    assert_eq!(
        o.on_main(&format!("{HANDOFF_FIXTURE_ROOT}/v0.6.0/history/s0.grid")),
        [0u8, 159, 146, 150, 10]
    );
}
