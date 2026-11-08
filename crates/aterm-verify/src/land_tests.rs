// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify land`'s rules over scratch repositories with a bare origin: the inheritance rule, the
//! compose rule and its re-judge, the re-judge bound, and the stated receipt, each beside the
//! control that shows the rule bites.

use super::*;
use aterm_types::land_trailer::KEYS;
use std::cell::RefCell;
use std::path::PathBuf;

#[test]
fn resource_admission_cannot_replace_the_landing_check_scope() {
    let args = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(admission(&[]), Some(aterm_capacity::Admit::Wait));
    assert_eq!(
        admission(&args(&["--admit", "now"])),
        Some(aterm_capacity::Admit::Now)
    );
    assert_eq!(
        admission(&args(&["--admit=wait"])),
        Some(aterm_capacity::Admit::Wait)
    );
    for flags in [
        &["--admit"][..],
        &["--admit", "other"],
        &["--checks", "fmt"],
        &["-p", "one"],
        &["--base", "HEAD"],
        &["--admit", "now", "--all"],
    ] {
        assert_eq!(admission(&args(flags)), None);
    }
    let cmd = judgment_command(
        Path::new("/targo"),
        Path::new("/checkout"),
        "main-sha",
        Path::new("/events"),
        aterm_capacity::Admit::Now,
    );
    assert_eq!(
        cmd.get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        [
            "--unverified",
            "verify",
            "--base",
            "main-sha",
            "--events",
            "/events",
            "--admit",
            "now"
        ]
    );
}

fn git_cmd(dir: &Path) -> Command {
    let mut cmd = aterm_uds::fixture_git::git();
    cmd.current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Andrew Yates")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Andrew Yates")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = git_cmd(dir);
    cmd.args(args);
    let out = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A bare origin whose main holds `a/x.txt`, `b/y.txt` and an eight-line `shared.txt`; a work
/// clone on branch `feature`; and a rival clone that moves main.
struct World {
    _dir: aterm_tempfile::TempDir,
    origin: PathBuf,
    work: PathBuf,
    rival: PathBuf,
}

const SHARED: &str = "1\n2\n3\n4\n5\n6\n7\n8\n";

fn world() -> World {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let root = dir.path().to_path_buf();
    let origin = root.join("origin.git");
    let seed = root.join("seed");
    git(
        &root,
        &["init", "-q", "--bare", "-b", "main", path(&origin)],
    );
    git(&root, &["init", "-q", "-b", "main", path(&seed)]);
    for (file, text) in [
        ("a/x.txt", "a\n"),
        ("b/y.txt", "b\n"),
        ("shared.txt", SHARED),
    ] {
        write(&seed, file, text);
    }
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-q", "-m", "seed"]);
    git(&seed, &["remote", "add", "origin", path(&origin)]);
    git(&seed, &["push", "-q", "origin", "main"]);
    let work = root.join("work");
    let rival = root.join("rival");
    git(&root, &["clone", "-q", path(&origin), path(&work)]);
    git(&root, &["clone", "-q", path(&origin), path(&rival)]);
    git(&work, &["checkout", "-q", "-b", "feature"]);
    World {
        _dir: dir,
        origin,
        work,
        rival,
    }
}

fn path(p: &Path) -> &str {
    p.to_str().expect("a UTF-8 scratch path")
}

fn write(dir: &Path, file: &str, text: &str) {
    let at = dir.join(file);
    std::fs::create_dir_all(at.parent().expect("a parent")).expect("dirs");
    std::fs::write(at, text).expect("write");
}

/// Commit `text` at `file` in `dir`: the commit.
fn commit(dir: &Path, file: &str, text: &str) -> String {
    write(dir, file, text);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", &format!("edit {file}")]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Move origin's main from the rival clone: `text` at `file`, pushed. The new tip.
fn rival_moves(world: &World, file: &str, text: &str) -> String {
    git(&world.rival, &["fetch", "-q", "origin"]);
    git(
        &world.rival,
        &["checkout", "-q", "-B", "main", "origin/main"],
    );
    let tip = commit(&world.rival, file, text);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    tip
}

/// Move origin's main from the rival clone by one more line of `shared.txt` (1-based `line`),
/// over every move before it. The new tip.
fn rival_moves_line(world: &World, line: usize, text: &str) -> String {
    git(&world.rival, &["fetch", "-q", "origin"]);
    git(
        &world.rival,
        &["checkout", "-q", "-B", "main", "origin/main"],
    );
    let now = std::fs::read_to_string(world.rival.join("shared.txt")).expect("shared");
    let mut lines: Vec<&str> = now.lines().collect();
    lines[line - 1] = text;
    let tip = commit(
        &world.rival,
        "shared.txt",
        &format!("{}\n", lines.join("\n")),
    );
    git(&world.rival, &["push", "-q", "origin", "main"]);
    tip
}

fn origin_main(world: &World) -> String {
    git(&world.work, &["ls-remote", "origin", "refs/heads/main"])
        .split_whitespace()
        .next()
        .expect("main")
        .to_owned()
}

/// The receipt git itself reads off `commit`'s trailers, as the harness will.
fn stated(world: &World, commit: &str) -> Option<Stated> {
    let keys: Vec<String> = KEYS.iter().map(|k| format!("key={k}")).collect();
    let format = format!("--format=%(trailers:{},unfold)", keys.join(","));
    Stated::of_trailers(&git(&world.work, &["log", "-1", &format, commit]))
}

fn row(check: &str, word: &str) -> CheckRow {
    CheckRow {
        check: check.into(),
        word: word.into(),
        findings: Vec::new(),
        findings_total: 0,
    }
}

fn red(check: &str, ids: &[&str]) -> CheckRow {
    CheckRow {
        check: check.into(),
        word: "FAIL".into(),
        findings: ids.iter().map(|id| (*id).to_owned()).collect(),
        findings_total: ids.len(),
    }
}

/// A run of `checks` that exited `code` in `secs`, its record whole: its `run-finished` event
/// names that exit and each check's word.
fn ran(code: i32, checks: Vec<CheckRow>, secs: u64) -> Judged {
    let finished = Finished {
        code,
        checks: checks
            .iter()
            .map(|row| (row.check.clone(), row.word.clone()))
            .collect(),
    };
    Judged {
        code,
        checks,
        secs,
        finished: Some(finished),
    }
}

fn green() -> Judged {
    ran(
        exit::PASS,
        vec![row("fmt", "ok"), row("lint", "skip"), row("test", "ok")],
        7,
    )
}

const THEIRS: &str = "-p b --lib -- old_red";

/// One landing of `world`'s work: the exit, what it said, and each judged base. `judge` sees the
/// base and how many judgments came before.
fn land_with(
    world: &World,
    branch: Branch,
    lane: Lane,
    mut judge: impl FnMut(&str, usize) -> Judged,
) -> (i32, Vec<String>, Vec<String>) {
    let said = RefCell::new(Vec::new());
    let bases = RefCell::new(Vec::new());
    let code = land(
        &world.work,
        &mut Seams {
            git: &git_cmd,
            judge: &mut |root: &Path, base: &str| {
                assert_eq!(root, world.work);
                let n = bases.borrow().len();
                bases.borrow_mut().push(base.to_owned());
                judge(base, n)
            },
            branch: &mut |_: &Path, _: &str| branch.clone(),
            lane: &mut |_: &Path, _: &str| lane.clone(),
            say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
        },
    );
    (code, said.into_inner(), bases.into_inner())
}

fn crates(names: &[&str]) -> Branch {
    Branch::Crates(names.iter().map(|n| (*n).to_owned()).collect())
}

/// THE INHERITANCE RULE: a red main's lane named red at the merged main commit, in a crate
/// outside the branch's own, is inherited. NEGATIVE CONTROLS: the same red in one of the
/// branch's crates counts, as does one the lane does not name, one on an unjudged or green main,
/// one under a branch that reaches every crate, a check whose findings the event cut short, and
/// a red no crate can be read from.
#[test]
fn a_red_main_already_had_outside_the_branch_is_inherited_and_every_other_counts() {
    let lane = Lane::Red([THEIRS.to_owned(), "tippy --workspace".to_owned()].into());
    let run = |rows: Vec<CheckRow>| ran(exit::FAILED, rows, 1);
    let theirs = run(vec![row("fmt", "ok"), red("test", &[THEIRS])]);
    assert_eq!(
        decide(&theirs, &lane, &crates(&["a"])),
        Decision {
            inherited: vec![THEIRS.into()],
            ..Decision::default()
        }
    );
    let counted = |judged: &Judged, lane: &Lane, branch: &Branch| {
        decide(judged, lane, branch).counted == ["test"]
    };
    assert!(
        counted(&theirs, &lane, &crates(&["a", "b"])),
        "in the branch's crates"
    );
    assert!(counted(
        &run(vec![red("test", &[THEIRS, "-p b --lib -- new_red"])]),
        &lane,
        &crates(&["a"])
    ));
    for unnamed in [Lane::Pass, Lane::Unjudged("no verdict".into())] {
        assert!(counted(&theirs, &unnamed, &crates(&["a"])), "{unnamed:?}");
    }
    assert!(counted(
        &theirs,
        &lane,
        &Branch::Everything("Cargo.lock".into())
    ));
    let mut cut = red("test", &[THEIRS]);
    cut.findings_total = 11;
    assert!(
        counted(&run(vec![cut]), &lane, &crates(&["a"])),
        "unseen findings"
    );
    assert!(counted(
        &run(vec![red("test", &["tippy --workspace"])]),
        &lane,
        &crates(&["a"])
    ));
}

/// A check that did not run, a run that could not run or ended oddly, and a failed run that
/// named no failed check are never a pass.
#[test]
fn a_run_that_did_not_finish_green_is_never_a_pass() {
    let named = |code: i32, checks: Vec<CheckRow>| {
        decide(&ran(code, checks, 1), &Lane::Pass, &crates(&["a"]))
    };
    assert_eq!(named(exit::PASS, green().checks), Decision::default());
    assert_eq!(
        named(exit::COULD_NOT_RUN, vec![row("export-scan", "NOT RUN")]).not_run,
        ["export-scan"]
    );
    assert!(!named(exit::COULD_NOT_RUN, Vec::new()).not_run.is_empty());
    assert!(!named(exit::PASS, Vec::new()).not_run.is_empty());
    assert!(!named(101, green().checks).not_run.is_empty());
    assert!(!named(exit::FAILED, green().checks).counted.is_empty());
}

/// A red item names a test's one crate, through a path spec too. NEGATIVE CONTROLS: a row label,
/// a lint's label naming packages, a NOT RUN label and an id naming two packages name none.
#[test]
fn a_red_item_names_the_crate_of_its_test() {
    assert_eq!(crate_of("-p agh --lib -- jq_all"), Some("agh"));
    assert_eq!(
        crate_of("-p path+file:///w/crates/x#xtra@0.1.0 --test it -- case"),
        Some("xtra")
    );
    for none in [
        "targo test --workspace --tests",
        "-- a test named -p q",
        "tippy -p a -p b --all-targets -- -D warnings",
        "NOT RUN targo test -p a --tests — -p c --lib: why",
        "-p a -p b --tests -- case",
        "-p a --lib -- ",
    ] {
        assert_eq!(crate_of(none), None, "{none}");
    }
}

/// A RECORD NOT WHOLE IS NEVER A PASS AND INHERITS NOTHING: a run whose `run-finished` event names
/// a check no line holds (that line lost), or is missing, or names another exit, inherits nothing
/// and does not pass, however its rows read. CONTROL: the same rows, whole, inherit main's red.
#[test]
fn a_record_not_whole_inherits_nothing_and_is_never_a_pass() {
    let lane = Lane::Red([THEIRS.to_owned()].into());
    let whole = ran(
        exit::FAILED,
        vec![row("fmt", "ok"), red("test", &[THEIRS])],
        1,
    );
    assert_eq!(
        decide(&whole, &lane, &crates(&["a"])),
        Decision {
            inherited: vec![THEIRS.into()],
            ..Decision::default()
        }
    );
    let mut lost = whole.clone();
    if let Some(finished) = lost.finished.as_mut() {
        finished.checks.push(("lint".into(), "FAIL".into()));
    }
    let mut missing = whole.clone();
    missing.finished = None;
    let mut other = whole.clone();
    if let Some(finished) = other.finished.as_mut() {
        finished.code = exit::PASS;
    }
    let mut green_missing = green();
    green_missing.finished = None;
    for judged in [lost, missing, other, green_missing] {
        let d = decide(&judged, &lane, &crates(&["a"]));
        assert!(
            d.inherited.is_empty() && !d.not_run.is_empty(),
            "{judged:?}: {d:?}"
        );
    }
}

/// The row reader takes what verify's own event writer writes: a failed test check's findings
/// and their total.
#[test]
fn a_check_row_reads_verifys_own_check_finished_event() {
    let result = crate::report::CheckResult {
        check: crate::Check::Test,
        outcome: crate::Outcome::Fail(crate::Severity::GateFailed),
        seconds: 3.5,
        detail: "1 test failed".into(),
        findings: vec![crate::ladder::Finding {
            id: THEIRS.into(),
            rerun: None,
            message: Vec::new(),
        }],
        rerun: "targo --unverified verify test".into(),
        stopped: false,
    };
    let line = crate::report::check_event(&result, &[]);
    assert_eq!(
        CheckRow::of_event(line.trim_end()),
        Some(red("test", &[THEIRS]))
    );
    assert_eq!(
        CheckRow::of_event("verify-event: {\"event\":\"stage-finished\"}"),
        None
    );
    let line = crate::report::run_event(&[result], exit::FAILED);
    assert_eq!(
        Finished::of_event(line.trim_end()),
        Some(Finished {
            code: exit::FAILED,
            checks: vec![("test".into(), "FAIL".into())],
        })
    );
    assert_eq!(CheckRow::of_event(line.trim_end()), None);
}

/// A green branch lands as one merge of main into it, stating its judgment: the tree judged, the
/// main commit its cone was computed from and each check's row; the checkout's branch is the
/// landing, clean, and its remote-tracking main names it.
#[test]
fn a_green_branch_lands_as_one_merge_stating_its_judgment() {
    let world = world();
    let feature = commit(&world.work, "a/x.txt", "a2\n");
    let main = rival_moves(&world, "b/y.txt", "b2\n");
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases, std::slice::from_ref(&main));
    let landing = origin_main(&world);
    let parents = git(&world.work, &["rev-list", "--parents", "-n", "1", &landing]);
    assert_eq!(parents, format!("{landing} {feature} {main}"));
    let receipt = stated(&world, &landing).expect("the landing states its judgment");
    assert_eq!(receipt.checked.len(), 1);
    assert_eq!(
        receipt.checked[0].tree,
        git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")])
    );
    assert_eq!(receipt.checked[0].base, main);
    assert_eq!(
        receipt.checked[0]
            .rows
            .iter()
            .map(|r| format!("{}:{}", r.check, r.word))
            .collect::<Vec<_>>(),
        ["fmt:ok", "lint:skip", "test:ok"]
    );
    assert_eq!((receipt.composed_onto, receipt.inherited.len()), (None, 0));
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
    assert_eq!(git(&world.work, &["rev-parse", "origin/main"]), landing);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
    assert_eq!(
        git(&world.work, &["log", "-1", "--format=%s", &landing]),
        "Merge origin/main into feature"
    );
}

/// A BRANCH THAT ALREADY MERGED MAIN LANDS AS THAT MERGE, RESTATED: its parents and its own
/// message kept, the trailers added, and no second merge made over it. CONTROL:
/// [`a_green_branch_lands_as_one_merge_stating_its_judgment`], a branch that has not merged main,
/// lands as one new merge of it.
#[test]
fn a_branch_that_merged_main_by_hand_lands_as_that_merge_restated() {
    let world = world();
    let feature = commit(&world.work, "a/x.txt", "a2\n");
    let main = rival_moves(&world, "b/y.txt", "b2\n");
    git(&world.work, &["fetch", "-q", "origin"]);
    git(
        &world.work,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge main by hand",
            "origin/main",
        ],
    );
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases, std::slice::from_ref(&main));
    let landing = origin_main(&world);
    assert_eq!(
        git(&world.work, &["rev-list", "--parents", "-n", "1", &landing]),
        format!("{landing} {feature} {main}"),
        "the hand merge's own parents: no second merge"
    );
    assert_eq!(
        git(&world.work, &["log", "-1", "--format=%s", &landing]),
        "Merge main by hand"
    );
    assert!(stated(&world, &landing).is_some(), "{said:#?}");
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
}

/// A CHECKOUT THAT MOVES OR CHANGES DURING THE RUN PUSHES NOTHING: the run judged files the
/// landing would not hold. An edit no commit holds, and a commit, made while the run ran, each
/// stop it. CONTROL: the same run, the checkout left alone, lands.
#[test]
fn a_checkout_that_moves_or_changes_during_the_run_pushes_nothing() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let before = origin_main(&world);
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| {
        write(&world.work, "loose.txt", "edited during the run\n");
        green()
    });
    assert_eq!((code, bases.len()), (exit::COULD_NOT_RUN, 1), "{said:#?}");
    assert!(
        said.iter().any(|l| l.contains("changes no commit holds")),
        "{said:#?}"
    );
    assert_eq!(origin_main(&world), before);
    std::fs::remove_file(world.work.join("loose.txt")).expect("tidy");
    let (code, said, _) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| {
        commit(&world.work, "a/x.txt", "a3\n");
        green()
    });
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    assert!(said.iter().any(|l| l.contains("HEAD moved")), "{said:#?}");
    assert_eq!(origin_main(&world), before);
    let (code, said, _) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_ne!(origin_main(&world), before);
}

/// A PUSH MAIN REFUSED WHILE IT STAYED PUT IS AN ERROR, NEVER A RETRY OR A MOVED MAIN: origin's
/// main is locked, so the push fails and main is where it was; nothing is composed or judged
/// again. CONTROL: unlocked, the same landing lands.
#[test]
fn a_push_main_refused_in_place_is_an_error_never_a_retry() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let before = origin_main(&world);
    let lock = world.origin.join("refs/heads/main.lock");
    std::fs::write(&lock, "").expect("lock origin's main");
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!((code, bases.len()), (exit::COULD_NOT_RUN, 1), "{said:#?}");
    assert!(
        said.iter().any(|l| l.contains("COULD NOT RUN — git push")),
        "{said:#?}"
    );
    assert!(
        !said
            .iter()
            .any(|l| l.contains("moved to") || l.contains("composing")),
        "{said:#?}"
    );
    assert_eq!(origin_main(&world), before);
    std::fs::remove_file(&lock).expect("unlock");
    let (code, said, _) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
}

/// A PUSH GIT REPORTED FAILED THAT MAIN TOOK IS LANDED: a connection lost after the server moved
/// main loses only the report, so a main that holds the landing (as its tip, or under commits
/// pushed after it) took it. NEGATIVE CONTROLS: a main still where it was refused it, an error;
/// a main moved without it moved.
#[test]
fn a_push_reported_failed_that_main_took_is_landed() {
    let world = world();
    let onto = origin_main(&world);
    let landing = commit(&world.work, "a/x.txt", "a2\n");
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    assert_eq!(
        plumbing.settle(&landing, &onto, "refused"),
        Err("refused".to_owned())
    );
    git(&world.work, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(plumbing.settle(&landing, &onto, "report lost"), Ok(None));
    let past = rival_moves(&world, "b/y.txt", "b2\n");
    assert_eq!(plumbing.settle(&landing, &onto, "report lost"), Ok(None));
    let elsewhere = commit(&world.work, "a/x.txt", "a3\n");
    assert_eq!(plumbing.settle(&elsewhere, &onto, "moved"), Ok(Some(past)));
}

/// An uninitialised gitlink is reported before any clone. CONTROL: a checkout with no
/// submodules, as all the landing worlds above are, needs no import and updates locally.
#[test]
fn landing_does_not_clone_an_uninitialised_submodule() {
    let world = world();
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    assert_eq!(plumbing.update_submodules(), Ok(()));
    let main = origin_main(&world);
    write(
        &world.work,
        ".gitmodules",
        "[submodule \"child\"]\npath = child\nurl = ../origin.git\n",
    );
    git(&world.work, &["add", ".gitmodules"]);
    git(
        &world.work,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &main,
            "child",
        ],
    );
    git(
        &world.work,
        &["commit", "-qm", "add an uninitialised local child"],
    );
    assert!(plumbing.update_submodules().is_err());
    assert!(!world.work.join("child/.git").exists(), "no clone");
    assert!(git(&world.work, &["submodule", "status"]).starts_with('-'));
}

/// A counted red and a dirty checkout push nothing; a branch already on main lands nothing.
#[test]
fn a_red_or_a_dirty_checkout_pushes_nothing() {
    let world = world();
    let before = origin_main(&world);
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(
        (code, bases.len()),
        (exit::PASS, 0),
        "nothing to land: {said:#?}"
    );
    commit(&world.work, "a/x.txt", "a2\n");
    write(&world.work, "loose.txt", "not committed\n");
    let (code, _, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!((code, bases.len()), (exit::COULD_NOT_RUN, 0));
    std::fs::remove_file(world.work.join("loose.txt")).expect("tidy");
    let (code, said, _) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| {
        ran(exit::FAILED, vec![red("test", &["-p a --lib -- mine"])], 1)
    });
    assert_eq!(code, exit::FAILED, "{said:#?}");
    assert_eq!(origin_main(&world), before, "nothing was pushed");
}

/// A resumed landing stays put when main has advanced past it. Dirty work still refuses;
/// committing new work in that same checkout must judge and land against the advanced main.
#[test]
fn an_already_landed_checkout_resumes_without_absorbing_main_or_losing_new_work() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let (code, said, _) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let landed = origin_main(&world);
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landed);
    let advanced = rival_moves(&world, "b/y.txt", "b2\n");
    assert_ne!(advanced, landed);

    let said = RefCell::new(Vec::new());
    let resume = || {
        land(
            &world.work,
            &mut Seams {
                git: &git_cmd,
                judge: &mut |_, _| panic!("an already-landed checkout needs no judgment"),
                branch: &mut |_, _| panic!("an already-landed checkout needs no branch scope"),
                lane: &mut |_, _| panic!("an already-landed checkout needs no lane verdict"),
                say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
            },
        )
    };
    assert_eq!(resume(), exit::PASS, "{:#?}", said.borrow());
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landed);
    assert_eq!(origin_main(&world), advanced);
    assert!(git(&world.work, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        std::fs::read_to_string(world.work.join("b/y.txt")).expect("old main's file"),
        "b\n",
        "the no-op must not merge newer main into the checkout"
    );

    write(&world.work, "a/x.txt", "a3\n");
    write(&world.work, "loose.txt", "unfinished\n");
    let dirty = git(&world.work, &["status", "--porcelain"]);
    assert_eq!(resume(), exit::COULD_NOT_RUN, "{:#?}", said.borrow());
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landed);
    assert_eq!(origin_main(&world), advanced);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), dirty);
    for (file, text) in [("a/x.txt", "a3\n"), ("loose.txt", "unfinished\n")] {
        assert_eq!(
            std::fs::read_to_string(world.work.join(file)).expect("preserved work"),
            text
        );
    }

    std::fs::remove_file(world.work.join("loose.txt")).expect("finish loose work");
    let new_work = commit(&world.work, "a/x.txt", "a3\n");
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        bases.as_slice(),
        std::slice::from_ref(&advanced),
        "new work must be judged"
    );
    let tip = origin_main(&world);
    assert_ne!(tip, advanced);
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), tip);
    for ancestor in [&advanced, &new_work] {
        git(
            &world.work,
            &["merge-base", "--is-ancestor", ancestor, &tip],
        );
    }
    for (file, text) in [("a/x.txt", "a3\n"), ("b/y.txt", "b2\n")] {
        assert_eq!(
            std::fs::read_to_string(world.work.join(file)).expect("landed file"),
            text
        );
    }
    assert!(git(&world.work, &["status", "--porcelain"]).is_empty());
}

/// An inherited red lands, named in the landing's `Aterm-Inherited`. NEGATIVE CONTROL: the same
/// red, main's lane naming it just as much, in one of the branch's crates counts and pushes
/// nothing.
#[test]
fn an_inherited_red_lands_named_and_the_same_red_in_the_branch_counts() {
    let failing = |_: &str, _: usize| {
        ran(
            exit::FAILED,
            vec![row("fmt", "ok"), red("test", &[THEIRS])],
            1,
        )
    };
    let lane = Lane::Red([THEIRS.to_owned()].into());
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let (code, said, _) = land_with(&world, crates(&["a"]), lane.clone(), failing);
    assert_eq!(code, exit::PASS, "{said:#?}");
    let receipt = stated(&world, &origin_main(&world)).expect("stated");
    assert_eq!(receipt.inherited, [THEIRS]);
    assert_eq!(receipt.checked[0].rows[1].word, "FAIL");

    let control = self::world();
    commit(&control.work, "b/y.txt", "b2\n");
    let before = origin_main(&control);
    let (code, _, _) = land_with(&control, crates(&["b"]), lane, failing);
    assert_eq!(code, exit::FAILED);
    assert_eq!(origin_main(&control), before);
}

/// The lane is read once the run ends: main's verdict on the merged commit, given while the run
/// judged, names the red, so it is inherited. NEGATIVE CONTROL: a lane that still has not judged
/// that commit when the run ends inherits nothing, and nothing is pushed.
#[test]
fn a_lane_verdict_given_during_the_run_is_inherited() {
    for (judged_meanwhile, want) in [(true, exit::PASS), (false, exit::FAILED)] {
        let world = world();
        commit(&world.work, "a/x.txt", "a2\n");
        let before = origin_main(&world);
        let run_ended = std::cell::Cell::new(false);
        let said = RefCell::new(Vec::new());
        let code = land(
            &world.work,
            &mut Seams {
                git: &git_cmd,
                judge: &mut |_: &Path, _: &str| {
                    run_ended.set(true);
                    ran(
                        exit::FAILED,
                        vec![row("fmt", "ok"), red("test", &[THEIRS])],
                        1,
                    )
                },
                branch: &mut |_: &Path, _: &str| crates(&["a"]),
                lane: &mut |_: &Path, _: &str| {
                    if judged_meanwhile && run_ended.get() {
                        Lane::Red([THEIRS.to_owned()].into())
                    } else {
                        Lane::Unjudged("not yet".into())
                    }
                },
                say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
            },
        );
        assert_eq!(code, want, "{:#?}", said.borrow());
        if judged_meanwhile {
            let receipt = stated(&world, &origin_main(&world)).expect("stated");
            assert_eq!(receipt.inherited, [THEIRS]);
        } else {
            assert_eq!(origin_main(&world), before);
        }
    }
}

#[test]
fn semantic_git_output_keeps_its_admission_prefix() {
    let world = world();
    let mut text = "-uninitialised prerequisite\n".to_owned();
    text.push_str(&"x".repeat(aterm_uds::deadline::OUTPUT_KEEP + 1));
    commit(&world.work, "large.txt", &text);
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let actual = plumbing.local(&["show", "HEAD:large.txt"]).unwrap();
    assert_eq!(actual.len(), text.len(), "semantic output cannot be a tail");
    assert!(actual.starts_with("-uninitialised prerequisite\n"));
}

fn import_git(world: &World, stream: &str) {
    // Import logical Git paths directly: their bytes need not be representable by this
    // host's working-tree filesystem. The real diff-tree protocol must preserve them.
    let input = world._dir.path().join("paths.noindex");
    std::fs::write(&input, stream).unwrap();
    let mut import = git_cmd(&world.origin);
    import.args(["fast-import", "--quiet"]);
    let out = aterm_uds::deadline::output_within_from(
        &mut import,
        aterm_uds::spawnfd::Stdio::from(std::fs::File::open(&input).unwrap()),
        aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("import completes");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn composition_compares_git_path_bytes_before_rendering_diagnostics() {
    let world = world();
    let base = origin_main(&world);
    let mut stream = String::new();
    for (reference, file) in [
        ("left", "a/\\200.txt"),
        ("right", "a/\\201.txt"),
        ("same", "a/\\200.txt"),
    ] {
        stream.push_str(&format!(
            "commit refs/heads/{reference}\n\
             committer Andrew Yates <test@example.invalid> 1 +0000\n\
             data {}\n{reference}\nfrom {base}\n\
             M 100644 inline \"{file}\"\ndata 2\nx\n\n",
            reference.len()
        ));
    }
    import_git(&world, &stream);
    let plumbing = Git {
        root: &world.origin,
        make: &git_cmd,
    };
    let left = plumbing.files(&base, "refs/heads/left").unwrap();
    let right = plumbing.files(&base, "refs/heads/right").unwrap();
    let same = plumbing.files(&base, "refs/heads/same").unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(right.len(), 1);
    assert!(
        left.is_disjoint(&right),
        "different Git path bytes are disjoint"
    );
    assert_eq!(
        left.intersection(&same).count(),
        1,
        "the same bytes still overlap"
    );
    assert!(left.contains(b"a/\x80.txt".as_slice()));
    assert_eq!(display_git_path(b"a/\x80.txt"), "\"a/\\x80.txt\"");
    let shown = display_git_path(b"a/\nverify land: LANDED");
    assert_eq!(
        shown.lines().count(),
        1,
        "a path cannot inject a diagnostic row"
    );
    assert!(shown.contains("\\n"));
}

#[test]
fn a_large_git_path_listing_keeps_shared_paths() {
    let world = world();
    let base = origin_main(&world);
    let prefix = format!("z/{}/{}/", "x".repeat(96), "y".repeat(96));
    let mut stream = String::new();
    for (reference, text) in [
        ("early", SHARED.replacen('1', "one", 1)),
        ("many", SHARED.replacen('8', "eight", 1)),
    ] {
        stream.push_str(&format!(
            "commit refs/heads/{reference}\n\
             committer Andrew Yates <test@example.invalid> 1 +0000\n\
             data {}\n{reference}\nfrom {base}\n\
             M 100644 inline shared.txt\ndata {}\n{text}",
            reference.len(),
            text.len()
        ));
        if reference == "many" {
            for index in 0..6000 {
                stream.push_str(&format!(
                    "M 100644 inline {prefix}{index:06}.txt\ndata 2\nx\n"
                ));
            }
        }
        stream.push('\n');
    }
    import_git(&world, &stream);
    let plumbing = Git {
        root: &world.origin,
        make: &git_cmd,
    };
    assert!(
        plumbing
            .merge_tree("refs/heads/early", "refs/heads/many")
            .unwrap()
            .is_some()
    );
    let early = plumbing.files(&base, "refs/heads/early").unwrap();
    let many = plumbing.files(&base, "refs/heads/many").unwrap();
    assert!(
        !early.is_disjoint(&many),
        "a large listing cannot hide the shared path"
    );
    assert_eq!(many.len(), 6001, "the listing is complete");
}

/// THE COMPOSE RULE: main moved during the run, its new commits touching none of the branch's
/// files and the merge clean, so the pass is composed onto it without a run: one judgment, the
/// landing's parents the judged commit and the new main, `Aterm-Composed-Onto` naming it.
/// NEGATIVE CONTROL: main moved touching a file the branch changed (the merge still clean), so
/// it is merged and only its delta judged again, since the judged commit, and nothing is composed.
#[test]
fn a_moved_main_composes_only_where_its_files_are_disjoint_else_it_is_rejudged() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let moved = RefCell::new(None);
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, n| {
        if n == 0 {
            *moved.borrow_mut() = Some(rival_moves(&world, "b/y.txt", "b2\n"));
        }
        green()
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 1, "composed, not judged again: {said:#?}");
    let moved = moved.into_inner().expect("main moved");
    let landing = origin_main(&world);
    let judged = git(&world.work, &["rev-parse", &format!("{landing}^1")]);
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{landing}^2")]),
        moved
    );
    let receipt = stated(&world, &landing).expect("stated");
    assert_eq!(receipt.composed_onto.as_deref(), Some(moved.as_str()));
    assert_eq!(
        receipt.checked[0].tree,
        git(&world.work, &["rev-parse", &format!("{judged}^{{tree}}")])
    );
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:b/y.txt")]),
        "b2"
    );
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:a/x.txt")]),
        "a2"
    );
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");

    let control = self::world();
    commit(&control.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let first_main = origin_main(&control);
    let (code, said, bases) = land_with(&control, crates(&["a"]), Lane::Pass, |_, n| {
        if n == 0 {
            rival_moves(&control, "shared.txt", &SHARED.replacen('8', "eight", 1));
        }
        green()
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 2, "judged again: {said:#?}");
    assert_eq!(bases[0], first_main);
    let landing = origin_main(&control);
    let receipt = stated(&control, &landing).expect("stated");
    assert_eq!(receipt.checked.len(), 2);
    assert_eq!(receipt.composed_onto, None);
    let first_judged = git(&control.work, &["rev-parse", &format!("{landing}^1")]);
    assert_eq!(
        bases[1], first_judged,
        "the re-judge's cone is the delta since the judged commit"
    );
    assert_eq!(receipt.checked[1].base, first_judged);
    assert_eq!(
        git(&control.work, &["show", &format!("{landing}:shared.txt")]),
        "one\n2\n3\n4\n5\n6\n7\neight"
    );
}

#[cfg(unix)]
#[test]
fn a_whitespace_only_conflict_path_is_not_erased() {
    for (file, shown) in [("  ", "\"  \""), ("a/\nnotice.txt", "\"a/\\nnotice.txt\"")] {
        let world = world();
        commit(&world.work, file, "ours\n");
        let moved = rival_moves(&world, file, "theirs\n");
        let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
        assert_eq!(code, exit::FAILED, "{said:#?}");
        assert!(bases.is_empty(), "a conflict starts no checks");
        assert!(said.iter().any(|line| line.contains(shown)), "{said:#?}");
        assert!(
            said.iter().all(|line| !line.contains('\n')),
            "one-line diagnostics: {said:#?}"
        );
        assert_eq!(origin_main(&world), moved, "a conflict pushes nothing");
    }
}

/// A moved main that does not merge cleanly stops on the conflict, left for the person; nothing
/// is pushed.
#[test]
fn a_conflicting_moved_main_stops_on_the_conflict() {
    let world = world();
    commit(&world.work, "shared.txt", &SHARED.replacen('4', "mine", 1));
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| {
        rival_moves(&world, "shared.txt", &SHARED.replacen('4', "theirs", 1));
        green()
    });
    assert_eq!((code, bases.len()), (exit::FAILED, 1), "{said:#?}");
    assert!(
        said.iter()
            .any(|l| l.contains("conflicts") && l.contains("\"shared.txt\"")),
        "{said:#?}"
    );
    assert!(git(&world.work, &["status", "--porcelain"]).contains("UU shared.txt"));
    assert_ne!(
        git(&world.work, &["log", "-1", "--format=%s", "origin/main"]),
        "Merge origin/main into feature"
    );
}

/// After three re-judges a main still moving over the branch's files stops the landing: the
/// branch merged with the last main and judged, nothing pushed, exit 75. NEGATIVE CONTROL: the
/// same main moving only twice lands on the third judgment.
#[test]
fn it_stops_after_three_rejudges_merged_and_judged() {
    let moving = |world: &World, times: usize| {
        let mut tips = Vec::new();
        let (code, said, bases) = land_with(world, crates(&["a"]), Lane::Pass, |_, n| {
            if n < times {
                tips.push(rival_moves_line(world, 4 + n, &format!("moved{n}")));
            }
            green()
        });
        (code, said, bases, tips)
    };
    let world = world();
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let (code, said, bases, tips) = moving(&world, usize::MAX);
    assert_eq!(code, STOPPED, "{said:#?}");
    assert_eq!(bases.len(), 1 + REJUDGES, "{said:#?}");
    let last = tips.last().expect("main moved");
    assert_eq!(origin_main(&world), *last, "nothing was pushed");
    let head = git(&world.work, &["rev-parse", "HEAD"]);
    let previous = &tips[tips.len() - 2];
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{head}^2")]),
        *previous,
        "merged with the main it judged last"
    );
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");

    let control = self::world();
    commit(&control.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let (code, said, bases, _) = moving(&control, 2);
    assert_eq!((code, bases.len()), (exit::PASS, 3), "{said:#?}");
    assert_eq!(
        stated(&control, &origin_main(&control))
            .expect("stated")
            .rejudges(),
        2
    );
}
