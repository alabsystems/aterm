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
use crate::ledger::{GitRunner, RunOut};

const GUARD: &str = "// the guard\nconst PINNED_DESKS: &[(&str, &str)] = &[\n    (\"v0.5.0\", \"history\"),\n];\n\nconst REQUIRED_DESKS: &[(&str, &str)] = &[\n    (\"v0.5.0\", \"history\"),\n];\n";

const RELEASE: &str = "cccccccccccccccccccccccccccccccccccccccc";

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
        let answers = rival.join(fixtures::SHIPPED_STATE_CORPUS);
        fs::create_dir_all(answers.parent().unwrap()).unwrap();
        fs::write(&answers, corpus()).unwrap();
        git(&rival, &["add", "-A"]);
        git(&rival, &["commit", "-q", "-m", "seed"]);
        git(&rival, &["push", "-q", "-u", "origin", "main"]);
        git(root, &["clone", "-q", "origin.git", "work"]);
        let work = root.join("work");
        identity(&work);
        Origin { dir, work, rival }
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

fn exit(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt as _;
    std::process::ExitStatus::from_raw(code << 8)
}

fn env_of(cmd: &Command, key: &str) -> Option<String> {
    cmd.get_envs()
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
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
        assert_eq!(
            env_of(cmd, "ATERM_BUILD_GIT_COMMIT_FULL").as_deref(),
            Some(RELEASE)
        );
        for desk in desks {
            fs::create_dir_all(out.join(desk)).unwrap();
            fs::write(
                out.join(desk).join("parent.toml"),
                format!(
                    "producer_version = \"0.6.0\"\nproducer_commit = \"{short}\"\n\
                     outgoing_build = {build}\n"
                ),
            )
            .unwrap();
            fs::write(out.join(desk).join("s0.grid"), [0u8, 159, 146, 150, 10]).unwrap();
        }
        fs::write(
            out.join("rendezvous.toml"),
            "producer_version = \"0.6.0\"\n",
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
fn the_generator_runs_once_per_reserved_identity() {
    let o = Origin::new();
    let runs = Cell::new(0);
    let mut run = generator(&["history"], &runs);
    let root = o.stage_root();
    let first = fixtures::stage(Path::new("targo"), &o.work, &root, &pins(7), &mut run).unwrap();
    assert!(first.fresh && first.rendezvous, "{first:?}");
    assert_eq!(first.desks, ["history"]);
    assert_eq!(first.dir, pins(7).staged_in(&root).join("v0.6.0"));
    let again = fixtures::stage(Path::new("targo"), &o.work, &root, &pins(7), &mut run).unwrap();
    assert!(!again.fresh);
    assert_eq!(runs.get(), 1);
    let next = fixtures::stage(Path::new("targo"), &o.work, &root, &pins(8), &mut run).unwrap();
    assert!(next.fresh);
    assert_eq!(runs.get(), 2);
    assert!(!pins(7).staged_in(&root).exists());
    assert!(
        !o.work
            .join(crate::gates::HANDOFF_FIXTURE_GUARD_TARGET)
            .exists(),
        "its target directory goes with the run"
    );
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
    fixtures::stage(Path::new("targo"), &o.work, &root, &pins(7), &mut failing).unwrap_err();
    assert!(fixtures::staged(&root, &pins(7)).is_none());

    let runs = Cell::new(0);
    let mut partial = generator(&["held-pane"], &runs);
    fixtures::stage(Path::new("targo"), &o.work, &root, &pins(7), &mut partial).unwrap_err();
    assert!(fixtures::staged(&root, &pins(7)).is_none());

    let mut unanswered = |cmd: &mut Command| {
        let out = generator(&["history"], &runs)(cmd);
        let answers = PathBuf::from(env_of(cmd, fixtures::ANSWERS_OUT).unwrap());
        fs::write(answers, corpus()).unwrap();
        out
    };
    let refused = fixtures::stage(
        Path::new("targo"),
        &o.work,
        &root,
        &pins(7),
        &mut unanswered,
    )
    .unwrap_err();
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
    let staged = fixtures::stage(Path::new("targo"), &o.work, &root, &pins(7), &mut run).unwrap();
    let work = Git::new(&o.work);
    let before = git(&o.work, &["rev-parse", "origin/main"])
        .trim()
        .to_string();

    let landed = fixtures::land(&work, &staged, &pins(7), 5).unwrap();
    let Landed::Pushed {
        commit,
        attempt,
        answers,
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
    git(&o.work, &["fetch", "-q", "origin", "main"]);
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

    let again = fixtures::land(&work, &staged, &pins(7), 5).unwrap();
    assert!(matches!(again, Landed::Already { .. }), "{again:?}");
    git(&o.work, &["fetch", "-q", "origin", "main"]);
    assert_eq!(
        git(&o.work, &["rev-parse", "origin/main"]).trim(),
        commit.as_str()
    );
}

struct Raced<'a> {
    inner: Git,
    rival: &'a Path,
    fired: Cell<bool>,
    pushes: RefCell<Vec<String>>,
}

impl GitRunner for Raced<'_> {
    fn git(&self, args: &[&str]) -> crate::ledger::Result<RunOut> {
        if args.first() == Some(&"push") {
            self.pushes.borrow_mut().push(args.join(" "));
            if !self.fired.replace(true) {
                fs::write(self.rival.join("README.md"), "aterm, moved\n").unwrap();
                git(self.rival, &["commit", "-q", "-am", "a peer's change"]);
                git(self.rival, &["push", "-q", "origin", "main"]);
            }
        }
        self.inner.git(args)
    }

    fn git_input(&self, args: &[&str], input: &[u8]) -> crate::ledger::Result<RunOut> {
        self.inner.git_input(args, input)
    }
}

#[test]
fn a_landing_that_loses_the_race_lands_on_the_winners_tip() {
    let o = Origin::new();
    let runs = Cell::new(0);
    let mut run = generator(&["history"], &runs);
    let staged = fixtures::stage(
        Path::new("targo"),
        &o.work,
        &o.stage_root(),
        &pins(7),
        &mut run,
    )
    .unwrap();
    let raced = Raced {
        inner: Git::new(&o.work),
        rival: &o.rival,
        fired: Cell::new(false),
        pushes: RefCell::new(Vec::new()),
    };
    let landed = fixtures::land(&raced, &staged, &pins(7), 5).unwrap();
    assert!(
        matches!(landed, Landed::Pushed { attempt: 2, .. }),
        "{landed:?}"
    );
    assert_eq!(raced.pushes.borrow().len(), 2);
    assert_eq!(
        o.on_main("README.md"),
        b"aterm, moved\n",
        "the peer's change kept"
    );
    assert!(
        String::from_utf8(o.on_main(HANDOFF_FIXTURE_GUARD))
            .unwrap()
            .contains("(\"v0.6.0\", \"history\"),")
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
    let staged = fixtures::stage(
        Path::new("targo"),
        &o.work,
        &o.stage_root(),
        &pins(7),
        &mut run,
    )
    .unwrap();
    fixtures::land(&Git::new(&o.work), &staged, &pins(7), 5).unwrap_err();
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
    let staged = fixtures::stage(
        Path::new("targo"),
        &o.work,
        &o.stage_root(),
        &pins(7),
        &mut run,
    )
    .unwrap();
    let landed = fixtures::land(&Git::new(&o.work), &staged, &pins(7), 5).unwrap();
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
