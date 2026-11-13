// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify land`'s rules over scratch repositories with a bare origin: the inheritance rule, the
//! compose rule and its re-judge, the re-judge bound, and the stated receipt, each beside the
//! control that shows the rule bites.

use super::*;
use aterm_types::land_trailer::{Interrupted, KEYS};
use std::cell::RefCell;
use std::path::PathBuf;

#[path = "land_rejudge_tests.rs"]
mod rejudge;

#[path = "land_metadata_tests.rs"]
mod metadata;

#[path = "land_markdown_tests.rs"]
mod markdown;

#[test]
fn a_closed_foreign_intent_refreshes_main_before_any_new_attempt() {
    use std::cell::Cell;
    for observed in ["unchanged", "moved", "landed"] {
        let attempts = Cell::new(0);
        let waited = Cell::new(false);
        let refreshed = Cell::new(false);
        let mut races = 2;
        let result = push_after_intents(
            &mut races,
            &|| {
                let attempt = attempts.get();
                attempts.set(attempt + 1);
                if attempt == 0 {
                    Ok(crate::remote::Pushed::Blocked {
                        intent: agh::IntentId::parse("66-fc387e8b41a0").unwrap(),
                        why: "pre-send".into(),
                    })
                } else {
                    assert!(waited.get() && refreshed.get());
                    assert_eq!(observed, "unchanged");
                    Ok(crate::remote::Pushed::Done)
                }
            },
            &|intent| {
                assert_eq!(intent.as_str(), "66-fc387e8b41a0");
                waited.set(true);
                Ok(())
            },
            &|| {
                assert!(waited.get());
                refreshed.set(true);
                Ok(match observed {
                    "unchanged" => PushObservation::Unchanged,
                    "moved" => PushObservation::Moved("fresh-main".into()),
                    _ => PushObservation::Landed,
                })
            },
            &|_| panic!("a pre-send obstruction is not generic rejection"),
        );
        assert_eq!(
            result,
            Ok((observed == "moved").then(|| "fresh-main".into()))
        );
        assert_eq!(attempts.get(), if observed == "unchanged" { 2 } else { 1 });
        assert_eq!(races, 3);
    }
}

#[test]
fn foreign_intent_recovery_keeps_one_budget_and_never_retries_unknown_or_refused_writes() {
    use std::cell::Cell;
    let attempts = Cell::new(0);
    let waits = Cell::new(0);
    let mut races = COMPOSES - 2;
    let result = push_after_intents(
        &mut races,
        &|| {
            attempts.set(attempts.get() + 1);
            Ok(crate::remote::Pushed::Blocked {
                intent: agh::IntentId::parse(&format!("{}-000000000000", attempts.get())).unwrap(),
                why: "pre-send".into(),
            })
        },
        &|_| {
            waits.set(waits.get() + 1);
            Ok(())
        },
        &|| Ok(PushObservation::Unchanged),
        &|_| unreachable!(),
    );
    assert!(result.is_err());
    assert_eq!((races, attempts.get(), waits.get()), (COMPOSES, 3, 2));

    for mode in ["unknown", "rejected", "read-failed", "deadline"] {
        let attempts = Cell::new(0);
        let mut races = 0;
        let result = push_after_intents(
            &mut races,
            &|| {
                attempts.set(attempts.get() + 1);
                match mode {
                    "unknown" => Err("unknown own outcome".into()),
                    "rejected" => Ok(crate::remote::Pushed::Rejected("refused in place".into())),
                    _ => Ok(crate::remote::Pushed::Blocked {
                        intent: agh::IntentId::parse("66-fc387e8b41a0").unwrap(),
                        why: "pre-send".into(),
                    }),
                }
            },
            &|_| Err(mode.into()),
            &|| panic!("no refresh after failed wait"),
            &|why| Err(why.into()),
        );
        assert!(result.is_err());
        assert_eq!(attempts.get(), 1);
    }
}

#[test]
fn a_new_composition_attempt_keeps_the_original_push_deadline() {
    use std::cell::Cell;
    let now = Cell::new(0);
    let until = 10;
    let sent = Cell::new(0);
    let attempt = || {
        if now.get() >= until {
            return Err("original deadline".into());
        }
        sent.set(sent.get() + 1);
        Ok(crate::remote::Pushed::Blocked {
            intent: agh::IntentId::parse("66-fc387e8b41a0").unwrap(),
            why: "pre-send".into(),
        })
    };
    let mut races = 0;
    let first = push_after_intents(
        &mut races,
        &attempt,
        &|_| Ok(()),
        &|| {
            now.set(until);
            Ok(PushObservation::Moved("fresh-main".into()))
        },
        &|_| unreachable!(),
    );
    assert_eq!(first, Ok(Some("fresh-main".into())));
    // The driver passes the same absolute bound after its clean composition. An expired
    // attempt cannot even inspect another blocker, much less send another atomic batch.
    let second = push_after_intents(
        &mut races,
        &attempt,
        &|_| panic!("no new wait"),
        &|| panic!("no new observation"),
        &|_| unreachable!(),
    );
    assert_eq!(second, Err("original deadline".into()));
    assert_eq!((sent.get(), races), (1, 1));
}

#[test]
fn resource_admission_and_timeout_cannot_replace_the_landing_check_scope() {
    let args = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        options(&[]),
        Some(LandOptions {
            admit: aterm_capacity::Admit::Wait,
            timeout: None,
        })
    );
    assert_eq!(
        options(&args(&["--admit", "now"])),
        Some(LandOptions {
            admit: aterm_capacity::Admit::Now,
            timeout: None,
        })
    );
    assert_eq!(
        options(&args(&["--timeout=off", "--admit=wait"])),
        Some(LandOptions {
            admit: aterm_capacity::Admit::Wait,
            timeout: Some("off".into()),
        })
    );
    assert_eq!(
        options(&args(&["--admit", "now", "--timeout", "21600"])),
        Some(LandOptions {
            admit: aterm_capacity::Admit::Now,
            timeout: Some("21600".into()),
        })
    );
    for flags in [
        &["--admit"][..],
        &["--admit", "other"],
        &["--timeout"],
        &["--timeout", "later"],
        &["--checks", "fmt"],
        &["-p", "one"],
        &["--base", "HEAD"],
        &["--admit", "now", "--all"],
        &["--admit", "now", "--admit=wait"],
        &["--timeout", "off", "--timeout=30"],
    ] {
        assert_eq!(options(&args(flags)), None);
    }
    let cmd = judgment_command(
        Path::new("/verifier"),
        Path::new("/checkout"),
        "main-sha",
        &[],
        Path::new("/events"),
        aterm_capacity::Admit::Now,
        &crate::EnvSnapshot::default(),
        Some("off"),
    )
    .unwrap();
    let words: Vec<_> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    #[cfg(unix)]
    assert_eq!(
        &words[..4],
        &[
            aterm_uds::tether::VERB.to_owned(),
            std::process::id().to_string(),
            "/verifier".to_owned(),
            "/verifier".to_owned()
        ]
    );
    let words = &words[if cfg!(unix) { 4 } else { 0 }..];
    let expected: Vec<String> = [
        "--base",
        "main-sha",
        "--events",
        "/events",
        "--admit",
        "now",
        crate::cli::INPUT_DRIFT,
        "--timeout",
        "off",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    let expected = {
        let mut expected = expected;
        expected.insert(0, PARENT_LIFETIME.to_owned());
        expected
    };
    assert_eq!(words, expected);
    // A recheck names its checks first and brackets no test inputs, since it runs no test.
    let cmd = judgment_command(
        Path::new("/verifier"),
        Path::new("/checkout"),
        "tip-sha",
        &RECHECKS,
        Path::new("/events"),
        aterm_capacity::Admit::Wait,
        &crate::EnvSnapshot::default(),
        None,
    )
    .unwrap();
    let words: Vec<_> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let words = &words[if cfg!(unix) { 4 } else { 0 }..];
    let lifetime = usize::from(cfg!(any(target_vendor = "apple", target_os = "linux")));
    assert_eq!(
        &words[lifetime..],
        [
            "fmt",
            "census",
            "export-scan",
            "--base",
            "tip-sha",
            "--events",
            "/events",
            "--admit",
            "wait",
        ]
    );
    let parsed = crate::cli::parse(&words[lifetime..]).expect("the recheck's command parses");
    assert_eq!(
        parsed.checks,
        [
            crate::checks::Check::Fmt,
            crate::checks::Check::Census,
            crate::checks::Check::ExportScan
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

#[test]
fn landing_reads_the_watch_journal_for_its_exact_base_tree() {
    for state in ["red", "pass"] {
        let world = world();
        let base = git(&world.work, &["rev-parse", "origin/main"]);
        let tree = git(&world.work, &["rev-parse", "origin/main^{tree}"]);
        git(&world.work, &["config", "user.name", "Andrew Yates"]);
        git(
            &world.work,
            &["config", "user.email", "test@example.invalid"],
        );
        assert!(matches!(live_lane(&world.work, &base), Lane::Unjudged(_)));
        let record = crate::judged::Record {
            commit: base.clone(),
            tree,
            host: "scratch-lane".into(),
            verifier: base.clone(),
            controller: String::new(),
            inputs: "fixture-seal".into(),
            token: "fixture".into(),
            state: state.into(),
            cause: String::new(),
            started: 1,
            updated: 2,
            lease_until: 0,
            rows: vec![crate::judged::Row {
                id: "tree".into(),
                outcome: state.into(),
                log: String::new(),
                red_items: if state == "red" {
                    vec![THEIRS.into()]
                } else {
                    vec![]
                },
                suspects: vec![],
                fixed_by: None,
                decisions: vec![],
            }],
        };
        assert!(
            crate::judged::append(
                &world.work,
                "origin",
                None,
                &record,
                crate::judged::deadline()
            )
            .unwrap()
        );
        commit(&world.work, "a/x.txt", "branch work\n");
        // This new tree has no verdict: a mutation that uses HEAD rather than base fails.
        let head = git(&world.work, &["rev-parse", "HEAD"]);
        assert!(matches!(live_lane(&world.work, &head), Lane::Unjudged(_)));
        let lane = live_lane(&world.work, &base);
        assert!(match (&lane, state) {
            (Lane::Red(items), "red") => items.contains(THEIRS),
            (Lane::Pass, "pass") => true,
            _ => false,
        });
        let (code, said, _) = land_with(&world, crates(&["a"]), lane, |_, _| green());
        assert_eq!(code, 0, "{said:?}");
        let expected = if state == "pass" {
            "green"
        } else {
            "red (1 items)"
        };
        let line = format!("verify land: main's lane judged {base} {expected}");
        assert!(said.contains(&line), "{said:?}");
        println!("{line}");
    }
}

/// One landing of `world`'s work: the exit, what it said, and each judged base. `judge` sees the
/// base and how many judgments came before.
fn land_with(
    world: &World,
    branch: Branch,
    lane: Lane,
    mut judge: impl FnMut(&str, usize) -> Judged,
) -> (i32, Vec<String>, Vec<String>) {
    land_watching(world, branch, lane, |base, n, _| Ran::Ended(judge(base, n)))
}

/// [`land_with`], whose `judge` also gets the running judgment's poll of main: a call stands
/// for a stage boundary or [`POLL`] passing, and its answer says whether the run ended
/// ([`Ran::Ended`]) or a `true` interrupted it ([`Ran::Interrupted`]).
fn land_watching(
    world: &World,
    branch: Branch,
    lane: Lane,
    judge: impl FnMut(&str, usize, Poll<'_>) -> Ran,
) -> (i32, Vec<String>, Vec<String>) {
    land_through(world, &git_cmd, branch, lane, judge)
}

/// [`land_watching`] through the git seam `git`. A recheck fails the test: these worlds share
/// no Markdown.
fn land_through(
    world: &World,
    git: &dyn Fn(&Path) -> Command,
    branch: Branch,
    lane: Lane,
    judge: impl FnMut(&str, usize, Poll<'_>) -> Ran,
) -> (i32, Vec<String>, Vec<String>) {
    let (code, said, bases, _) = land_rechecking(world, git, branch, lane, judge, |base| {
        panic!("no recheck was expected (base {base})")
    });
    (code, said, bases)
}

/// A landing through [`land_rechecking`]: the exit, what it said, each judged base, and each
/// recheck's base and the HEAD it ran on.
type Landed = (i32, Vec<String>, Vec<String>, Vec<(String, String)>);

/// [`land_through`], whose `recheck` stands for each recheck's run: it sees the base. Also each
/// recheck's base and the HEAD it ran on.
fn land_rechecking(
    world: &World,
    git: &dyn Fn(&Path) -> Command,
    branch: Branch,
    lane: Lane,
    mut judge: impl FnMut(&str, usize, Poll<'_>) -> Ran,
    mut recheck: impl FnMut(&str) -> Judged,
) -> Landed {
    let said = RefCell::new(Vec::new());
    let bases = RefCell::new(Vec::new());
    let rechecks = RefCell::new(Vec::new());
    let code = land(
        &world.work,
        &mut Seams {
            git,
            judge: &mut |root: &Path, base: &str, poll: Poll<'_>| {
                assert_eq!(root, world.work);
                let n = bases.borrow().len();
                bases.borrow_mut().push(base.to_owned());
                judge(base, n, poll)
            },
            branch: &mut |_: &Path, _: &str| branch.clone(),
            lane: &mut |_: &Path, _: &str| lane.clone(),
            recheck: &mut |root: &Path, base: &str| {
                assert_eq!(root, world.work);
                let head = self::git(root, &["rev-parse", "HEAD"]);
                rechecks.borrow_mut().push((base.to_owned(), head));
                recheck(base)
            },
            say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
        },
    );
    (
        code,
        said.into_inner(),
        bases.into_inner(),
        rechecks.into_inner(),
    )
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

#[test]
fn land_requires_a_completed_terminal_line_but_preserves_later_partial_diagnostics() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("verify.events");
    let events = crate::event_log::EventLog::open(&path).unwrap();
    let result = crate::report::CheckResult {
        check: crate::Check::Fmt,
        outcome: crate::Outcome::Ok,
        seconds: 1.0,
        detail: String::new(),
        findings: Vec::new(),
        rerun: "targo --unverified verify fmt".into(),
        stopped: false,
    };
    events.write(&crate::report::check_event(&result, &[]));
    events.write(&crate::report::run_event(&[result], exit::PASS));
    let complete = std::fs::read_to_string(&path).unwrap();
    let baseline = Judged::from_events(&complete, exit::PASS, 7);
    assert!(baseline.whole());
    assert_eq!(baseline.checks.len(), 1);
    assert_eq!(baseline.secs, 7);
    assert_eq!(
        decide(&baseline, &Lane::Pass, &crates(&["a"])),
        Decision::default()
    );

    let unframed = complete.strip_suffix('\n').unwrap();
    let missing = Judged::from_events(unframed, exit::PASS, 7);
    assert_eq!(missing.checks, baseline.checks);
    assert!(missing.finished.is_none());
    assert!(!missing.whole());
    let decision = decide(&missing, &Lane::Pass, &crates(&["a"]));
    assert!(!decision.not_run.is_empty());
    assert!(decision.inherited.is_empty());

    let crlf = Judged::from_events(&complete.replace('\n', "\r\n"), exit::PASS, 7);
    assert_eq!(crlf, baseline);
    let partial_tail = format!("{complete}verify-event: {{\"schema\":1,\"event\":\"disk\"");
    let with_tail = Judged::from_events(&partial_tail, exit::PASS, 7);
    assert_eq!(with_tail, baseline);
    assert_eq!(
        decide(&with_tail, &Lane::Pass, &crates(&["a"])),
        Decision::default()
    );
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
    for branch in [Some("feature"), Some("feature\u{a0}"), None] {
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
        let judged = git(&world.work, &["rev-parse", "HEAD"]);
        match branch {
            Some("feature") => {}
            Some(name) => {
                git(&world.work, &["checkout", "-q", "-b", name]);
            }
            None => {
                git(&world.work, &["checkout", "-q", "--detach"]);
            }
        }
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
        if branch != Some("feature") {
            assert_eq!(
                git(&world.work, &["rev-parse", "refs/heads/feature"]),
                judged,
                "adoption must not move the similarly named or detached checkout's other branch"
            );
        }
        assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
        if let Some(name) = branch {
            assert_eq!(
                git(&world.work, &["rev-parse", &format!("refs/heads/{name}")]),
                landing
            );
        } else {
            let plumbing = Git {
                root: &world.work,
                make: &git_cmd,
            };
            assert_eq!(
                plumbing
                    .status(&["symbolic-ref", "--quiet", "HEAD"], LOCAL)
                    .unwrap()
                    .status
                    .code(),
                Some(1)
            );
        }
    }
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
    assert_eq!(
        plumbing.update_submodules(),
        Err("initialise child through agh before verify land".into())
    );
    assert!(!world.work.join("child/.git").exists(), "no clone");
    assert!(git(&world.work, &["submodule", "status"]).starts_with('-'));
}

/// A world whose main vendors a local `child` repository at `vendor/sub`, merged into the work
/// checkout with the submodule initialised there, as a checkout is before verify land: the child,
/// and the commit it pins.
fn vendored_child(world: &World) -> (PathBuf, String) {
    let scratch = world.work.parent().expect("scratch");
    let child = scratch.join("child");
    git(scratch, &["init", "-q", "-b", "main", path(&child)]);
    let first = commit(&child, "lib.txt", "1\n");
    git(
        &world.rival,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            path(&child),
            "vendor/sub",
        ],
    );
    git(&world.rival, &["commit", "-qm", "vendor the child"]);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    merge_main_alone(world);
    git(
        &world.work,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "--no-fetch",
        ],
    );
    assert_eq!(
        git(&world.work.join("vendor/sub"), &["rev-parse", "HEAD"]),
        first
    );
    (child, first)
}

/// Merge origin's main into the work checkout as the land's agh fetch of main does: never
/// recursing into a submodule, which git's own fetch would do on demand.
fn merge_main_alone(world: &World) {
    git(
        &world.work,
        &["fetch", "-q", "--recurse-submodules=no", "origin"],
    );
    git(
        &world.work,
        &["merge", "-q", "--no-edit", "--no-verify", "origin/main"],
    );
}

/// Main pins `vendor/sub` at `object` (from the rival clone), merged into the work checkout.
fn main_pins(world: &World, object: &str) {
    git(&world.rival, &["fetch", "-q", "origin"]);
    git(&world.rival, &["merge", "-q", "--ff-only", "origin/main"]);
    git(
        &world.rival,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{object},vendor/sub"),
        ],
    );
    git(&world.rival, &["commit", "-qm", "pin the child"]);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    merge_main_alone(world);
}

/// A COMMIT A MERGED MAIN NEWLY PINS IN AN INITIALISED SUBMODULE IS IMPORTED, alone, through
/// that submodule's own origin (here not its tip), into agh's gitlink ref, and checked out; a
/// later pin that does not descend from it replaces the ref. NEGATIVE CONTROLS: a pin that is
/// no commit, and one its origin does not hold, are each named and leave the submodule where it
/// was. CONTROL: a pin the submodule already holds is checked out with no remote at all.
#[test]
fn a_submodule_pin_main_moved_is_imported_through_its_own_origin() {
    let world = world();
    let (child, first) = vendored_child(&world);
    let sub = world.work.join("vendor/sub");
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let submodule = Git {
        root: &sub,
        make: &git_cmd,
    };
    let second = commit(&child, "lib.txt", "2\n");
    let third = commit(&child, "lib.txt", "3\n");
    main_pins(&world, &second);
    assert_eq!(
        submodule.has_commit(&second),
        Ok(false),
        "main's pin is new here"
    );
    assert_eq!(plumbing.update_submodules(), Ok(()));
    assert_eq!(git(&sub, &["rev-parse", "HEAD"]), second);
    assert_eq!(git(&sub, &["rev-parse", GITLINK_REF]), second);
    assert_eq!(submodule.has_commit(&third), Ok(false), "the pin alone");

    git(&child, &["checkout", "-q", "-b", "side", &first]);
    let side = commit(&child, "side.txt", "s\n");
    main_pins(&world, &side);
    assert_eq!(plumbing.update_submodules(), Ok(()));
    assert_eq!(git(&sub, &["rev-parse", "HEAD"]), side);
    assert_eq!(git(&sub, &["rev-parse", GITLINK_REF]), side, "replaced");

    let blob = git(&child, &["rev-parse", "HEAD:side.txt"]);
    let absent = "1".repeat(40);
    for pin in [&blob, &absent] {
        main_pins(&world, pin);
        let refused = plumbing
            .update_submodules()
            .expect_err("no commit to check out");
        assert!(
            refused.starts_with(&format!(
                "vendor/sub's pinned commit {pin} was not imported: "
            )),
            "{refused}"
        );
        assert_eq!(git(&sub, &["rev-parse", "HEAD"]), side);
    }

    git(&sub, &["remote", "set-url", "origin", "../no-such-origin"]);
    main_pins(&world, &first);
    assert_eq!(plumbing.update_submodules(), Ok(()));
    assert_eq!(git(&sub, &["rev-parse", "HEAD"]), first);
}

/// A SUBMODULE WHOSE ORIGIN IS ON GITHUB is asked through agh alone: one exact fetch of the pin
/// into the submodule's own repository (never the superproject's), then the checkout. The agh
/// step here is a stand-in that fetches from the local child, as agh fetches from GitHub.
#[test]
fn a_github_submodules_pin_is_fetched_by_agh_into_the_submodule() {
    let world = world();
    let (child, _) = vendored_child(&world);
    let sub = world.work.join("vendor/sub");
    git(
        &sub,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/Fixture/Sub.git",
        ],
    );
    let second = commit(&child, "lib.txt", "2\n");
    main_pins(&world, &second);
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let fetches = std::cell::Cell::new(0);
    let remote = |root: &Path, tool, args: &[std::ffi::OsString], until| match tool {
        crate::remote::Tool::Git => crate::remote::run(root, &git_cmd, tool, args, until),
        crate::remote::Tool::Agh => {
            fetches.set(fetches.get() + 1);
            assert_eq!(root, sub, "agh runs for the submodule");
            assert_eq!(&args[..3], ["repo", "fetch", "fixture/sub"]);
            assert_eq!(args[3], sub.as_os_str(), "into the submodule's repository");
            assert_eq!(args[4], "--input");
            let batch: aterm_json::Value =
                aterm_json::from_slice(&std::fs::read(&args[5]).expect("live input"))
                    .expect("JSON");
            assert_eq!(
                batch,
                aterm_json::json!([{"source":second,"into":GITLINK_REF,"expected_oid":second}])
            );
            let wanted = format!("+{second}:{GITLINK_REF}");
            let mut fetch = git_cmd(Path::new(&args[3]));
            fetch.args([
                "-c",
                "protocol.file.allow=always",
                "fetch",
                "-q",
                "--no-tags",
                path(&child),
                &wanted,
            ]);
            aterm_uds::deadline::output_until(&mut fetch, until).map_err(|e| e.to_string())
        }
    };
    assert_eq!(plumbing.update_submodules_through(&remote), Ok(()));
    assert_eq!(fetches.get(), 1);
    assert_eq!(git(&sub, &["rev-parse", "HEAD"]), second);
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
                judge: &mut |_, _, _| panic!("an already-landed checkout needs no judgment"),
                branch: &mut |_, _| panic!("an already-landed checkout needs no branch scope"),
                lane: &mut |_, _| panic!("an already-landed checkout needs no lane verdict"),
                recheck: &mut |_, _| panic!("an already-landed checkout needs no recheck"),
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
                judge: &mut |_: &Path, _: &str, _: Poll<'_>| {
                    run_ended.set(true);
                    Ran::Ended(ran(
                        exit::FAILED,
                        vec![row("fmt", "ok"), red("test", &[THEIRS])],
                        1,
                    ))
                },
                branch: &mut |_: &Path, _: &str| crates(&["a"]),
                lane: &mut |_: &Path, _: &str| {
                    if judged_meanwhile && run_ended.get() {
                        Lane::Red([THEIRS.to_owned()].into())
                    } else {
                        Lane::Unjudged("not yet".into())
                    }
                },
                recheck: &mut |_: &Path, _: &str| panic!("no recheck was expected"),
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
    text.push('\u{a0}');
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
            .tree()
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

/// THE COMPOSE RULE: a clean merge with disjoint changes or exactly convergent entries
/// composes the pass without another run: one judgment, the landing's parents the judged
/// commit and the new main, `Aterm-Composed-Onto` naming it.
/// NEGATIVE CONTROL: genuinely different shared entries are merged and judged again,
/// even where the merge itself is clean. A lock change that is not the isolated product-version
/// move says so in its refusal; the old line named `Cargo.lock` and stopped, as if no exception
/// had been tried. Where the exception's evidence could not be read, or has a shape it does not
/// read, the refusal says it could not be judged and why; the old line claimed the move was
/// absent even for a version move whose merge tree or child workspace went unread.
#[test]
fn a_moved_main_composes_disjoint_or_convergent_changes_else_it_is_rejudged() {
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
    let moved_main = git(&control.work, &["rev-parse", &format!("{landing}^2")]);
    assert_eq!(
        bases[1], moved_main,
        "the re-judge's cone is the branch's delta from the moved main"
    );
    assert_eq!(receipt.checked[1].base, moved_main);
    assert_eq!(
        git(&control.work, &["show", &format!("{landing}:shared.txt")]),
        "one\n2\n3\n4\n5\n6\n7\neight"
    );

    // Distinct commits may already agree on the complete overlapping entry.
    for change in ["edit", "delete", "mode"] {
        let world = self::world();
        let converge = |dir: &Path, message: &str| {
            git(dir, &["config", "core.filemode", "false"]);
            match change {
                "delete" => std::fs::remove_file(dir.join("shared.txt")).unwrap(),
                "edit" => write(dir, "shared.txt", "converged\n"),
                _ => {}
            }
            git(dir, &["add", "-A"]);
            if change == "mode" {
                git(dir, &["update-index", "--chmod=+x", "shared.txt"]);
            }
            git(dir, &["commit", "-qm", message]);
            git(dir, &["rev-parse", "HEAD"])
        };
        let checked = converge(&world.work, "the branch's convergent change");
        let moved = RefCell::new(None);
        let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, n| {
            if n == 0 {
                git(&world.rival, &["fetch", "-q", "origin"]);
                git(
                    &world.rival,
                    &["checkout", "-q", "-B", "main", "origin/main"],
                );
                let tip = converge(&world.rival, "main's convergent change");
                assert_ne!(checked, tip, "the commits really diverged");
                git(&world.rival, &["push", "-q", "origin", "main"]);
                *moved.borrow_mut() = Some(tip);
            }
            green()
        });
        assert_eq!(code, exit::PASS, "{change}: {said:#?}");
        let moved = moved.into_inner().unwrap();
        let landing = origin_main(&world);
        let entry = git(&world.work, &["ls-tree", &checked, "--", "shared.txt"]);
        assert_eq!(
            git(&world.work, &["ls-tree", &moved, "--", "shared.txt"]),
            entry
        );
        assert_eq!(
            git(&world.work, &["ls-tree", &landing, "--", "shared.txt"]),
            entry
        );
        if change == "delete" {
            assert!(entry.is_empty(), "all three entries are absent");
        } else if change == "mode" {
            assert!(entry.starts_with("100755 blob "));
        }
        assert_eq!(bases.len(), 1, "{change}: composed once: {said:#?}");
        let receipt = stated(&world, &landing).unwrap();
        assert_eq!(receipt.checked.len(), 1);
        assert_eq!(receipt.composed_onto.as_deref(), Some(moved.as_str()));
    }

    // Object equality alone does not cover a different mode or kind of entry.
    for mode in ["100755", "120000"] {
        let world = self::world();
        let base = origin_main(&world);
        let checked = commit(&world.work, "shared.txt", "same object\n");
        write(&world.rival, "shared.txt", "same object\n");
        git(&world.rival, &["add", "-A"]);
        let object = git(&world.rival, &["rev-parse", ":shared.txt"]);
        git(
            &world.rival,
            &["update-index", "--cacheinfo", mode, &object, "shared.txt"],
        );
        git(&world.rival, &["commit", "-qm", "a different entry mode"]);
        let main = git(&world.rival, &["rev-parse", "HEAD"]);
        git(&world.rival, &["push", "-q", "origin", "main"]);
        git(&world.work, &["fetch", "-q", "origin"]);
        assert_eq!(
            git(
                &world.work,
                &["rev-parse", &format!("{checked}:shared.txt")]
            ),
            object
        );
        assert!(
            git(&world.work, &["ls-tree", &checked, "--", "shared.txt"])
                .starts_with("100644 blob ")
        );
        assert!(
            git(&world.work, &["ls-tree", &main, "--", "shared.txt"])
                .starts_with(&format!("{mode} blob "))
        );
        let plumbing = Git {
            root: &world.work,
            make: &git_cmd,
        };
        let different = plumbing
            .paths(&[
                "diff-tree",
                "-r",
                "-t",
                "-z",
                "--name-only",
                "--no-renames",
                "--ignore-submodules=none",
                &checked,
                &main,
            ])
            .unwrap();
        assert!(different.contains(b"shared.txt".as_slice()));
        match plumbing.merge_tree(&main, &checked).unwrap() {
            Compose::Tree(tree) => assert_eq!(
                plumbing.independent_changes(&base, &main, &checked, &tree),
                Ok(Independence::Blocked {
                    paths: vec![b"shared.txt".to_vec()],
                    unread: None,
                    lock: None
                }),
                "{mode}: a clean merge still differs from what was checked"
            ),
            refused => {
                assert_eq!(mode, "120000", "the regular mode change merges cleanly");
                assert_eq!(
                    refused,
                    Compose::Conflict,
                    "a type change is Git's conflict"
                );
            }
        }
    }

    // Recursive leaf diffs alone omit a newly present parent tree. Its complete
    // entry still differs from the checked deletion, even if the merge is clean.
    let shape = self::world();
    let base = origin_main(&shape);
    std::fs::remove_file(shape.work.join("shared.txt")).unwrap();
    git(&shape.work, &["add", "-A"]);
    git(&shape.work, &["commit", "-qm", "delete the old file"]);
    let checked = git(&shape.work, &["rev-parse", "HEAD"]);
    std::fs::remove_file(shape.rival.join("shared.txt")).unwrap();
    let main = commit(&shape.rival, "shared.txt/child", "a new tree\n");
    git(&shape.rival, &["push", "-q", "origin", "main"]);
    git(&shape.work, &["fetch", "-q", "origin"]);
    let plumbing = Git {
        root: &shape.work,
        make: &git_cmd,
    };
    assert!(
        plumbing
            .files(&base, &main)
            .unwrap()
            .contains(b"shared.txt".as_slice())
    );
    assert!(
        !plumbing
            .files(&checked, &main)
            .unwrap()
            .contains(b"shared.txt".as_slice())
    );
    let tree_entries = plumbing
        .paths(&[
            "diff-tree",
            "-r",
            "-t",
            "-z",
            "--name-only",
            "--no-renames",
            "--ignore-submodules=none",
            &checked,
            &main,
        ])
        .unwrap();
    assert!(tree_entries.contains(b"shared.txt".as_slice()));
    let tree = plumbing
        .merge_tree(&main, &checked)
        .unwrap()
        .tree()
        .map(str::to_owned)
        .expect("the deletion and replacement directory merge cleanly");
    assert_eq!(
        plumbing.independent_changes(&base, &main, &checked, &tree),
        Ok(Independence::Blocked {
            paths: vec![b"shared.txt".to_vec()],
            unread: None,
            lock: None
        }),
        "an absent entry is not a directory"
    );

    // A product-version move does not change the branch's independent lock entry.
    // The real merge must retain both edits; other metadata and uncertain identity do
    // not receive this exception, even when their textual merge is clean.
    const WORKSPACE: &str =
        "[workspace]\nmembers = [\"crates/*\"]\n[workspace.package]\nversion = \"0.1.0\"\n";
    const PRODUCT: &str = "[package]\nname = \"aterm\"\nversion.workspace = true\n\
        [[bin]]\nname = \"aterm\"\npath = \"src/main.rs\"\n\
        [dependencies]\nagent = { path = \"../agent\" }\n";
    const AGENT: &str =
        "[package]\nname = \"agent\"\nversion = \"0.0.0\"\n[dependencies]\nbase = \"1.0.0\"\n";
    const LOCK: &str = "version = 4\n\n\
        [[package]]\nname = \"aterm\"\nversion = \"0.1.0\"\ndependencies = [\"agent\"]\n\n\
        [[package]]\nname = \"agent\"\nversion = \"0.0.0\"\ndependencies = [\"base\"]\n\n\
        [[package]]\nname = \"agh-fake\"\nversion = \"0.0.0\"\n\n\
        [[package]]\nname = \"base\"\nversion = \"1.0.0\"\n\
        source = \"registry+https://example.invalid/index\"\nchecksum = \"old-checksum\"\n";
    for change in [
        "version",
        "dependency",
        "checksum",
        "format",
        "trivia",
        "mismatch",
        "inheritor",
        "mode",
        "unknown",
        "symlink-member",
        "root-package",
        "workspace-override",
        "build-disabled",
        "build-override",
        "build-default",
        "path-member",
        "nested-workspace",
        "nested-override",
        "escaping-path",
        "gitlink-workspace",
        "gitlink-override",
    ] {
        let world = self::world();
        let workspace = if change == "root-package" {
            format!("{WORKSPACE}\n[package]\nname = \"root\"\nversion.workspace = true\n")
        } else if matches!(change, "gitlink-workspace" | "gitlink-override") {
            WORKSPACE.replace(
                "[workspace.package]",
                "exclude = [\"vendor/sub\"]\n[workspace.package]",
            )
        } else {
            WORKSPACE.to_owned()
        };
        let product = match change {
            "build-disabled" => PRODUCT.replace("[package]\n", "[package]\nbuild = false\n"),
            "build-override" => PRODUCT.replace("[package]\n", "[package]\nbuild = \"other.rs\"\n"),
            "build-default" => PRODUCT.replace("[package]\n", "[package]\nbuild = \"build.rs\"\n"),
            _ => PRODUCT.to_owned(),
        };
        for (file, text) in [
            ("Cargo.toml", workspace.as_str()),
            ("Cargo.lock", LOCK),
            ("crates/aterm/Cargo.toml", product.as_str()),
            ("crates/aterm/src/main.rs", "fn main() {}\n"),
            ("crates/aterm/build.rs", "// the fixed identity writer\n"),
            ("crates/aterm-stamp/src/lib.rs", "// runtime identity\n"),
            (
                "crates/aterm-census/src/guards/identity_read.rs",
                "// the unchanged identity boundary\n",
            ),
            (
                "crates/agent/Cargo.toml",
                if change == "inheritor" {
                    "[package]\nname = \"agent\"\nversion.workspace = true\n\
                     [dependencies]\nbase = \"1.0.0\"\n"
                } else if change == "workspace-override" {
                    "[package]\nname = \"agent\"\nversion = \"0.0.0\"\nworkspace = \"../..\"\n\
                     [dependencies]\nbase = \"1.0.0\"\n"
                } else {
                    AGENT
                },
            ),
            (
                "crates/agh-fake/Cargo.toml",
                "[package]\nname = \"agh-fake\"\nversion = \"0.0.0\"\n",
            ),
        ] {
            write(&world.rival, file, text);
        }
        if change == "symlink-member" {
            // Exercise Git's immutable symlink shape without requiring OS symlink
            // privileges. The target member would otherwise evade the manifest scan.
            for dir in [&world.rival, &world.work] {
                git(dir, &["config", "core.symlinks", "false"]);
            }
            write(&world.rival, "crates/linked", "../outside");
            write(
                &world.rival,
                "outside/Cargo.toml",
                "[package]\nname = \"hidden\"\nversion.workspace = true\n",
            );
        }
        if matches!(change, "gitlink-workspace" | "gitlink-override") {
            let child = world._dir.path().join("child");
            git(&world.rival, &["init", "-q", "-b", "main", path(&child)]);
            write(
                &child,
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/helper\"]\n[workspace.package]\nversion = \"7.0.0\"\n",
            );
            write(
                &child,
                "crates/helper/Cargo.toml",
                if change == "gitlink-override" {
                    "[package]\nname = \"helper\"\nversion.workspace = true\nworkspace = \"../../../..\"\n"
                } else {
                    "[package]\nname = \"helper\"\nversion.workspace = true\n"
                },
            );
            write(&child, "crates/helper/src/lib.rs", "pub fn helper() {}\n");
            // Like astream's negative fixture corpus, this manifest is inspected
            // but its deliberately absent target is not in the dependency closure.
            write(
                &child,
                "fixtures/unused/Cargo.toml",
                "[package]\nname = \"unused\"\nversion = \"0.0.0\"\n[dependencies]\nmissing = { path = \"missing\" }\n",
            );
            git(&child, &["add", "-A"]);
            git(&child, &["commit", "-qm", "an independent child workspace"]);
            git(
                &world.rival,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "-q",
                    path(&child),
                    "vendor/sub",
                ],
            );
        }
        git(&world.rival, &["add", "-A"]);
        if change == "symlink-member" {
            let target = git(&world.rival, &["rev-parse", ":crates/linked"]);
            git(
                &world.rival,
                &[
                    "update-index",
                    "--cacheinfo",
                    "120000",
                    &target,
                    "crates/linked",
                ],
            );
        }
        git(
            &world.rival,
            &["commit", "-q", "-m", "the versioned product"],
        );
        git(&world.rival, &["push", "-q", "origin", "main"]);
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "--ff-only", "origin/main"]);
        if matches!(change, "gitlink-workspace" | "gitlink-override") {
            git(
                &world.work,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "update",
                    "--init",
                    "--no-fetch",
                ],
            );
        }
        if change == "symlink-member" {
            assert!(
                git(&world.work, &["ls-tree", "HEAD", "crates/linked"]).starts_with("120000 blob ")
            );
        }
        let mut own_lock = LOCK.replace(
            "dependencies = [\"base\"]",
            "dependencies = [\"base\", \"agh-fake\"]",
        );
        let mut agent = git(&world.work, &["show", "HEAD:crates/agent/Cargo.toml"]);
        if matches!(
            change,
            "path-member"
                | "nested-workspace"
                | "nested-override"
                | "escaping-path"
                | "gitlink-workspace"
                | "gitlink-override"
        ) {
            let target = match change {
                "escaping-path" => "../../../outside",
                "path-member" => "../../support/helper",
                "gitlink-workspace" | "gitlink-override" => "../../vendor/sub/crates/helper",
                _ => "../../support/workspace/helper",
            };
            agent.push_str(&format!("\n[dependencies.helper]\npath = \"{target}\"\n"));
            own_lock = own_lock.replace(
                "dependencies = [\"base\", \"agh-fake\"]",
                "dependencies = [\"base\", \"agh-fake\", \"helper\"]",
            );
            own_lock.push_str(&format!(
                "\n[[package]]\nname = \"helper\"\nversion = \"{}\"\n",
                if matches!(change, "path-member" | "gitlink-override") {
                    "0.1.0"
                } else if change == "gitlink-workspace" {
                    "7.0.0"
                } else {
                    "0.0.0"
                }
            ));
            if change == "path-member" {
                write(
                    &world.work,
                    "support/helper/Cargo.toml",
                    "[package]\nname = \"helper\"\nversion.workspace = true\n",
                );
            } else if matches!(change, "nested-workspace" | "nested-override") {
                write(
                    &world.work,
                    "support/workspace/Cargo.toml",
                    "[workspace]\nmembers = [\"helper\"]\n[workspace.package]\nversion = \"0.0.0\"\n",
                );
                write(
                    &world.work,
                    "support/workspace/helper/Cargo.toml",
                    if change == "nested-override" {
                        "[package]\nname = \"helper\"\nversion.workspace = true\nworkspace = \"../../..\"\n"
                    } else {
                        "[package]\nname = \"helper\"\nversion.workspace = true\n"
                    },
                );
            }
        }
        write(
            &world.work,
            "crates/agent/Cargo.toml",
            &format!("{agent}\n[dev-dependencies]\nagh-fake = {{ path = \"../agh-fake\" }}\n"),
        );
        commit(&world.work, "Cargo.lock", &own_lock);
        let moved = RefCell::new(None);
        let (code, said, bases) = land_with(&world, crates(&["agent"]), Lane::Pass, |_, n| {
            if n == 0 {
                git(&world.rival, &["fetch", "-q", "origin"]);
                git(
                    &world.rival,
                    &["checkout", "-q", "-B", "main", "origin/main"],
                );
                let workspace = workspace.replace(
                    "0.1.0",
                    if change == "mismatch" {
                        "0.3.0"
                    } else {
                        "0.2.0"
                    },
                );
                let mut lock = LOCK.replacen("0.1.0", "0.2.0", 1);
                match change {
                    "dependency" => {
                        lock = lock.replace(
                            "dependencies = [\"agent\"]",
                            "dependencies = [\"agent\", \"base\"]",
                        );
                    }
                    "checksum" => lock = lock.replace("old-checksum", "new-checksum"),
                    "format" => lock = lock.replacen("version = 4", "version = 3", 1),
                    "trivia" => lock.insert_str(0, "# another change\n"),
                    "unknown" => lock.push_str("future-policy = true\n"),
                    _ => {}
                }
                write(&world.rival, "Cargo.toml", &workspace);
                write(&world.rival, "Cargo.lock", &lock);
                git(&world.rival, &["add", "-A"]);
                if change == "mode" {
                    git(&world.rival, &["update-index", "--chmod=+x", "Cargo.lock"]);
                }
                git(
                    &world.rival,
                    &["commit", "-q", "-m", "move product identity"],
                );
                let tip = git(&world.rival, &["rev-parse", "HEAD"]);
                git(&world.rival, &["push", "-q", "origin", "main"]);
                *moved.borrow_mut() = Some(tip);
            }
            green()
        });
        assert_eq!(code, exit::PASS, "{change}: {said:#?}");
        let composed = matches!(
            change,
            "version" | "build-default" | "nested-workspace" | "gitlink-workspace"
        );
        assert_eq!(
            bases.len(),
            if composed { 1 } else { 2 },
            "{change}: {said:#?}"
        );
        if !composed {
            let refused = said
                .iter()
                .find(|line| line.contains("does not compose"))
                .unwrap_or_else(|| panic!("{change}: a refusal line: {said:#?}"));
            // The exception names the difference the read trees decided, or why it could not
            // judge them; a shape it does not read is never stated as an absent move.
            let unread = |why: &str| {
                format!(
                    ", and whether its change is the isolated product-version move that alone \
                     may compose could not be judged: {why}"
                )
            };
            let shapes = |endpoint: &str| {
                unread(&format!(
                    "{endpoint}'s workspace manifests or Cargo.lock are outside the shapes this \
                     exception reads"
                ))
            };
            let lock = match change {
                "dependency" | "checksum" | "trivia" | "unknown" => {
                    ", and its change is not the isolated product-version move that alone may \
                     compose: new main changes Cargo.lock beyond the product version"
                        .to_owned()
                }
                "format" | "mismatch" | "mode" => shapes("new main"),
                "inheritor" | "root-package" | "workspace-override" | "build-disabled"
                | "build-override" => shapes("the base"),
                "path-member" | "nested-override" | "escaping-path" => shapes("the checked tree"),
                "symlink-member" => unread(
                    "the base: \"crates/linked\" sits directly in crates/ rather than in a \
                     member directory",
                ),
                "gitlink-override" => unread(
                    "submodule \"vendor/sub\": its workspace is outside the shapes this \
                     exception reads",
                ),
                _ => panic!("{change}: an unclassified refusal: {refused}"),
            };
            assert!(
                refused.ends_with(&format!(
                    "touch the branch's \"Cargo.lock\", where the checked tree's entry differs \
                     from new main's or the merge's{lock}"
                )),
                "{change}: a refused lock says what its version exception decided: {refused}"
            );
        }
        let moved = moved.into_inner().unwrap();
        let landing = origin_main(&world);
        let receipt = stated(&world, &landing).unwrap();
        assert_eq!(
            receipt.composed_onto.as_deref(),
            composed.then_some(moved.as_str()),
            "{change}"
        );
        if composed {
            let parents = git(&world.work, &["show", "-s", "--format=%P", &landing]);
            let checked = parents.split_whitespace().next().unwrap();
            let tree = git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")]);
            assert!(
                receipt_composition(
                    &world.work,
                    &std::env::var_os("PATH").unwrap_or_default(),
                    checked,
                    &moved,
                    &tree,
                )
                .unwrap(),
                "{change}"
            );
            let base = git(&world.work, &["merge-base", checked, &moved]);
            let claims_absent = "is not the isolated product-version move";
            if change == "version" {
                // The positive control composes from the same plumbing.
                let plumbing = Git {
                    root: &world.work,
                    make: &git_cmd,
                };
                let composes = plumbing
                    .independent_changes(&base, &moved, checked, &tree)
                    .unwrap();
                assert!(
                    matches!(&composes, Independence::Composes(why)
                        if why.contains("the isolated product version moved from 0.1.0 to 0.2.0")),
                    "{composes:?}"
                );
                // A merge tree no object store holds: neither the endpoint entries nor the
                // exception's own trees can be read, so nothing decided the move is absent.
                let missing = "0".repeat(tree.len());
                let blocked = plumbing
                    .independent_changes(&base, &moved, checked, &missing)
                    .unwrap();
                let Independence::Blocked {
                    paths,
                    unread: Some(_),
                    ..
                } = &blocked
                else {
                    panic!("an unreadable merge keeps the lock overlap: {blocked:?}");
                };
                assert_eq!(paths, &[b"Cargo.lock".to_vec()]);
                let words = blocked.to_string();
                assert!(
                    !words.contains(claims_absent),
                    "unread evidence decided nothing about the move: {words}"
                );
                assert!(
                    words.contains("(the endpoint entries could not be read")
                        && words.contains(
                            "and whether its change is the isolated product-version move \
                             that alone may compose could not be judged: the merge: git ls-tree"
                        ),
                    "{words}"
                );
            }
            if change == "gitlink-workspace" {
                // A checkout whose submodule was never initialized reads every endpoint entry,
                // but not the child workspace the exception must close over.
                let fresh = world._dir.path().join("fresh");
                git(
                    world._dir.path(),
                    &["clone", "-q", path(&world.origin), path(&fresh)],
                );
                let plumbing = Git {
                    root: &fresh,
                    make: &git_cmd,
                };
                let blocked = plumbing
                    .independent_changes(&base, &moved, checked, &tree)
                    .unwrap();
                let Independence::Blocked {
                    paths,
                    unread: None,
                    ..
                } = &blocked
                else {
                    panic!("the endpoint entries were read: {blocked:?}");
                };
                assert_eq!(paths, &[b"Cargo.lock".to_vec()]);
                let words = blocked.to_string();
                assert!(
                    !words.contains(claims_absent),
                    "an unread child workspace decided nothing about the move: {words}"
                );
                assert!(
                    words.contains("could not be judged: submodule \"vendor/sub\": git rev-parse"),
                    "{words}"
                );
            }
        }
        let lock = git(&world.work, &["show", &format!("{landing}:Cargo.lock")]);
        assert!(lock.contains(
            if matches!(
                change,
                "path-member"
                    | "nested-workspace"
                    | "nested-override"
                    | "escaping-path"
                    | "gitlink-workspace"
                    | "gitlink-override"
            ) {
                "dependencies = [\"base\", \"agh-fake\", \"helper\"]"
            } else {
                "dependencies = [\"base\", \"agh-fake\"]"
            }
        ));
        assert!(lock.contains("version = \"0.2.0\""));
        assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
    }
}

/// A moved main that converges on one shared path and differs on another is judged again, and
/// the line that says why names only the path that still differs: the convergent one was
/// discarded and did not block composition. Where the endpoint entries cannot be read, nothing
/// is discarded and the refusal says so beside the whole overlap.
/// NEGATIVE CONTROL: the old line printed the raw overlap, naming the convergent `a/x.txt` too
/// ("touch the branch's "a/x.txt", "shared.txt"").
#[test]
fn a_refused_composition_names_only_the_shared_paths_that_still_differ() {
    let world = world();
    write(&world.work, "a/x.txt", "converged\n");
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, n| {
        if n == 0 {
            write(&world.rival, "a/x.txt", "converged\n");
            rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
        }
        green()
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 2, "judged again: {said:#?}");
    let refused = said
        .iter()
        .find(|line| line.contains("does not compose"))
        .unwrap_or_else(|| panic!("a refusal line: {said:#?}"));
    assert!(refused.contains("\"shared.txt\""), "{refused}");
    assert!(
        !refused.contains("a/x.txt"),
        "a convergent path did not block composition: {refused}"
    );

    let unread = self::world();
    let base = origin_main(&unread);
    write(&unread.work, "a/x.txt", "converged\n");
    let checked = commit(&unread.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    write(&unread.rival, "a/x.txt", "converged\n");
    let main = rival_moves(&unread, "shared.txt", &SHARED.replacen('8', "eight", 1));
    git(&unread.work, &["fetch", "-q", "origin"]);
    let plumbing = Git {
        root: &unread.work,
        make: &git_cmd,
    };
    let tree = plumbing
        .merge_tree(&main, &checked)
        .unwrap()
        .tree()
        .map(str::to_owned)
        .expect("a clean merge");
    assert_eq!(
        plumbing.independent_changes(&base, &main, &checked, &tree),
        Ok(Independence::Blocked {
            paths: vec![b"shared.txt".to_vec()],
            unread: None,
            lock: None
        })
    );
    let missing = "0".repeat(tree.len());
    let blocked = plumbing
        .independent_changes(&base, &main, &checked, &missing)
        .unwrap();
    let Independence::Blocked {
        paths,
        unread: Some(_),
        lock: None,
    } = &blocked
    else {
        panic!("an unreadable merge entry keeps the overlap: {blocked:?}");
    };
    assert_eq!(paths, &[b"a/x.txt".to_vec(), b"shared.txt".to_vec()]);
    let words = blocked.to_string();
    assert!(
        words.contains("\"a/x.txt\", \"shared.txt\" (the endpoint entries could not be read"),
        "{words}"
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

/// SLICE 0's exit test (`docs/DESIGN-landing-economy.md` §2.0, §5): main gains a commit that
/// touches the branch's file while the judgment runs, and the first poll after it interrupts
/// the run, naming the tip and the shared file. The interrupted run is no verdict: the receipt
/// states it as interrupted, never as checked, beside the judgment of the moved main, which is
/// merged and judged at once and counts as one re-judge. A poll before main moved interrupts
/// nothing. NEGATIVE CONTROL: a disjoint move mid-run interrupts nothing, a second poll of the
/// same tip reads main but tests nothing (it makes only the git calls of a poll with nothing
/// new), and the pass composes onto the moved main after the push, as before.
#[test]
fn a_main_that_cannot_compose_interrupts_the_judgment_at_its_next_poll() {
    let world = world();
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let first_main = origin_main(&world);
    let judged_tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
    let (mut polls, mut moved) = (Vec::new(), String::new());
    let (code, said, bases) = land_watching(&world, crates(&["a"]), Lane::Pass, |_, n, poll| {
        if n == 0 {
            polls.push(poll(&mut || false));
            moved = rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
            polls.push(poll(&mut || false));
            if polls == [false, true] {
                return Ran::Interrupted { secs: 42 };
            }
        }
        Ran::Ended(green())
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        polls,
        [false, true],
        "the first poll after the move interrupts"
    );
    assert_eq!(bases, [first_main.clone(), moved.clone()], "{said:#?}");
    let line = format!(
        "verify land: INTERRUPTED the judgment of {judged_tree}: origin/main moved to {moved}, \
         which does not compose: its new commits touch the branch's \"shared.txt\", where the \
         checked tree's entry differs from new main's or the merge's"
    );
    assert_eq!(
        said.iter().filter(|said| **said == line).count(),
        1,
        "{said:#?}"
    );
    let landing = origin_main(&world);
    let receipt = stated(&world, &landing).expect("stated");
    assert_eq!(
        receipt.checked.len(),
        1,
        "the interrupted run states no verdict"
    );
    assert_eq!(receipt.checked[0].base, moved);
    assert_eq!(
        receipt.interrupted,
        [Interrupted {
            tree: judged_tree,
            base: first_main,
            tip: moved.clone(),
            secs: 42,
        }]
    );
    assert_eq!((receipt.rejudges(), receipt.secs()), (1, 42 + 7));
    assert_eq!(receipt.composed_onto, None);
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{landing}^2")]),
        moved
    );
    assert!(
        said.iter()
            .any(|said| said.starts_with("verify land: LANDED")
                && said.contains("(1 re-judge, 1 after an interrupted judgment)")),
        "{said:#?}"
    );
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");

    let control = self::world();
    commit(&control.work, "a/x.txt", "a2\n");
    let calls = std::cell::Cell::new(0_usize);
    let counting = |root: &Path| {
        calls.set(calls.get() + 1);
        git_cmd(root)
    };
    let (mut polls, mut costs, mut moved) = (Vec::new(), Vec::new(), String::new());
    let (code, said, bases) = land_through(
        &control,
        &counting,
        crates(&["a"]),
        Lane::Pass,
        |_, n, poll| {
            assert_eq!(n, 0, "a disjoint move is never judged again");
            for moves in [false, true, false] {
                if moves {
                    moved = rival_moves(&control, "b/y.txt", "b2\n");
                }
                let before = calls.get();
                polls.push(poll(&mut || false));
                costs.push(calls.get() - before);
            }
            Ran::Ended(green())
        },
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(polls, [false, false, false], "{said:#?}");
    assert!(
        costs[1] > costs[0] && costs[2] == costs[0],
        "the moved tip is tested once, its second read only read: {costs:?}"
    );
    assert_eq!(bases.len(), 1, "{said:#?}");
    assert!(
        !said.iter().any(|said| said.contains("INTERRUPTED")),
        "{said:#?}"
    );
    let receipt = stated(&control, &origin_main(&control)).expect("stated");
    assert_eq!(receipt.composed_onto.as_deref(), Some(moved.as_str()));
    assert!(receipt.interrupted.is_empty());
}

/// Each interrupt counts toward [`REJUDGES`]: a main that keeps moving into the branch's file
/// interrupts every judgment, and once the re-judges are spent the tool stops with the branch
/// merged with the main it judged last, that judgment interrupted, and nothing pushed.
#[test]
fn interrupted_judgments_count_toward_the_rejudge_bound() {
    let world = world();
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let mut tips = Vec::new();
    let (code, said, bases) = land_watching(&world, crates(&["a"]), Lane::Pass, |_, n, poll| {
        tips.push(rival_moves_line(&world, 4 + n, &format!("moved{n}")));
        assert!(
            poll(&mut || false),
            "judgment {n}: the first poll after the move interrupts"
        );
        Ran::Interrupted { secs: 1 }
    });
    assert_eq!(code, STOPPED, "{said:#?}");
    assert_eq!(bases.len(), 1 + REJUDGES, "{said:#?}");
    let last = tips.last().expect("main moved");
    assert_eq!(origin_main(&world), *last, "nothing was pushed");
    let previous = &tips[tips.len() - 2];
    let head = git(&world.work, &["rev-parse", "HEAD"]);
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{head}^2")]),
        *previous,
        "merged with the main it judged last"
    );
    let stop = format!(
        "verify land: STOPPED after {REJUDGES} re-judges — the branch is merged with {previous}, \
         its judgment there interrupted, and nothing was pushed. Run it again"
    );
    assert!(said.contains(&stop), "{said:#?}");
    assert_eq!(
        said.iter()
            .filter(|said| said.starts_with("verify land: INTERRUPTED"))
            .count(),
        1 + REJUDGES,
        "{said:#?}"
    );
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
}

/// A poll that cannot read main changes nothing: the judgment goes on, and its reason is said
/// once while it repeats, again once a read in between worked, and not at all when the poll
/// became moot (the run's own answer came, which ends its read). Once main reads again, a tip
/// that does not compose interrupts as ever. NEGATIVE CONTROL: the polls between the failures
/// read main and answer `false`, as nothing moved, and a failure after one says its reason.
#[test]
fn a_poll_that_cannot_read_main_says_so_once_a_run_and_the_judgment_goes_on() {
    let world = world();
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let hidden = world.origin.with_extension("hidden");
    let hide = |hidden_now: bool| {
        let (from, to) = if hidden_now {
            (&world.origin, &hidden)
        } else {
            (&hidden, &world.origin)
        };
        std::fs::rename(from, to).expect("origin moves");
    };
    let mut polls = Vec::new();
    let (code, said, bases) = land_watching(&world, crates(&["a"]), Lane::Pass, |_, n, poll| {
        if n == 0 {
            hide(true);
            polls.push(poll(&mut || false));
            polls.push(poll(&mut || false));
            hide(false);
            polls.push(poll(&mut || false));
            hide(true);
            polls.push(poll(&mut || false));
            hide(false);
            polls.push(poll(&mut || false));
            hide(true);
            polls.push(poll(&mut || true));
            hide(false);
            rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
            polls.push(poll(&mut || false));
            if polls[6] {
                return Ran::Interrupted { secs: 1 };
            }
        }
        Ran::Ended(green())
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        polls,
        [false, false, false, false, false, false, true],
        "{said:#?}"
    );
    let unread: Vec<_> = said
        .iter()
        .filter(|said| said.contains("origin/main was not read or tested during the judgment"))
        .collect();
    assert_eq!(unread.len(), 2, "{said:#?}");
    assert!(
        unread.iter().all(|said| said.ends_with("; it goes on")),
        "{said:#?}"
    );
    assert_eq!(bases.len(), 2, "{said:#?}");
}

/// A run on a scripted clock: its stage boundaries at the offsets given, a red reported and
/// its exit at the offsets its cells hold (which a poll may set), and each wait
/// spending all it was given unless the exit comes first.
struct Scripted {
    clock: std::rc::Rc<std::cell::Cell<Duration>>,
    boundaries: Vec<Duration>,
    unpassed: std::rc::Rc<std::cell::Cell<Option<Duration>>>,
    ends: std::rc::Rc<std::cell::Cell<Option<Duration>>>,
    code: i32,
    interrupted: usize,
}

impl Scripted {
    fn new(clock: &std::rc::Rc<std::cell::Cell<Duration>>, boundaries: &[u64]) -> Self {
        clock.set(Duration::ZERO);
        Self {
            clock: clock.clone(),
            boundaries: boundaries
                .iter()
                .copied()
                .map(Duration::from_secs)
                .collect(),
            unpassed: std::rc::Rc::default(),
            ends: std::rc::Rc::default(),
            code: exit::PASS,
            interrupted: 0,
        }
    }
}

impl Running for Scripted {
    fn wait(&mut self, within: Duration) -> std::io::Result<Option<i32>> {
        assert_eq!(self.interrupted, 0, "an interrupted run is not waited on");
        let now = self.clock.get();
        if let Some(ends) = self.ends.get()
            && ends <= now + within
        {
            self.clock.set(ends.max(now));
            return Ok(Some(self.code));
        }
        self.clock.set(now + within);
        Ok(None)
    }

    fn reported(&mut self) -> Report {
        let now = self.clock.get();
        Report {
            boundaries: self.boundaries.iter().filter(|at| **at <= now).count(),
            unpassed: self.unpassed.get().is_some_and(|at| at <= now),
        }
    }

    fn interrupt(&mut self) {
        self.interrupted += 1;
    }
}

/// A running judgment asks its poll at each stage boundary its run reports, within one
/// boundary tick of it, and at least every [`POLL`], on an injected clock; every poll has a new
/// boundary or a [`POLL`] behind it. The first `true` interrupts the run at once, and the run is
/// not waited on again. NEGATIVE CONTROL: polls that answer `false` interrupt nothing, and the
/// run's own exit ends the supervision with its code.
#[test]
fn a_running_judgment_polls_at_each_stage_boundary_and_at_least_every_poll_interval() {
    let secs = Duration::from_secs;
    let start = Instant::now();
    let clock = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
    let ends = secs(1000);
    let mut run = Scripted::new(&clock, &[10, 11, 400]);
    run.ends.set(Some(ends));
    let mut polled = Vec::new();
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
        polled.push(clock.get());
        false
    })
    .unwrap();
    assert_eq!(ending, Ending::Exited(exit::PASS));
    assert_eq!(run.interrupted, 0);
    for at in &run.boundaries {
        assert!(
            polled
                .iter()
                .any(|poll| *poll >= *at && *poll < *at + BOUNDARY_TICK),
            "boundary at {at:?} unpolled: {polled:?}"
        );
    }
    let mut before = Duration::ZERO;
    for poll in polled.iter().copied().chain([ends]) {
        assert!(poll - before <= POLL, "{before:?} to {poll:?}: {polled:?}");
        let fresh = run.boundaries.iter().any(|at| before < *at && *at <= poll);
        assert!(
            poll == ends || fresh || poll - before >= POLL,
            "{poll:?} had nothing behind it: {polled:?}"
        );
        before = poll;
    }

    let mut run = Scripted::new(&clock, &[10, 30]);
    let mut asked = 0;
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
        asked += 1;
        asked == 2
    })
    .unwrap();
    assert_eq!(ending, Ending::Interrupted);
    assert_eq!((asked, run.interrupted), (2, 1));
    assert!(
        clock.get() >= secs(30) && clock.get() < secs(30) + BOUNDARY_TICK,
        "interrupted at its second boundary's poll: {:?}",
        clock.get()
    );
}

/// A run that reported a red is decided by its own end, as without the poll: main is not read
/// again, and a `true` that comes as the red is reported, or as the run exits, interrupts
/// nothing; the poll is told the question became moot, so its read of main ends. NEGATIVE
/// CONTROL: the same `true` with neither interrupts the run (the test above), the poll reads the
/// question live before either came, and a boundary before the red is polled.
#[test]
fn a_run_with_a_check_that_did_not_pass_is_decided_by_its_own_end() {
    let start = Instant::now();
    let clock = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
    let mut run = Scripted::new(&clock, &[10, 20, 30]);
    run.unpassed.set(Some(Duration::from_secs(15)));
    run.ends.set(Some(Duration::from_secs(1000)));
    run.code = exit::FAILED;
    let mut polled = Vec::new();
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
        polled.push(clock.get());
        clock.get() >= Duration::from_secs(20)
    })
    .unwrap();
    assert_eq!(ending, Ending::Exited(exit::FAILED));
    assert_eq!(run.interrupted, 0);
    assert_eq!(
        polled.len(),
        1,
        "only the boundary before the red: {polled:?}"
    );
    assert_eq!(clock.get(), Duration::from_secs(1000));

    for meanwhile in ["a red", "its exit"] {
        let mut run = Scripted::new(&clock, &[10]);
        run.ends.set(Some(Duration::from_secs(1000)));
        let (unpassed, ends) = (run.unpassed.clone(), run.ends.clone());
        let ending = supervise(&mut run, &mut || start + clock.get(), &mut |moot| {
            assert!(!moot(), "{meanwhile}: the run's answer has not come yet");
            match meanwhile {
                "its exit" => ends.set(Some(clock.get())),
                _ => unpassed.set(Some(clock.get())),
            }
            assert!(moot(), "{meanwhile}: the poll's read of main ends");
            true
        })
        .unwrap();
        assert_eq!(ending, Ending::Exited(exit::PASS), "{meanwhile}");
        assert_eq!(run.interrupted, 0, "{meanwhile}");
    }
}

/// A run's stage boundaries are the complete `stage-finished` records its events file holds,
/// each counted once as the file grows, one split across two reads included; it is unpassed
/// once a complete record names a red, any word but `ok` or `skip`: a `check-finished` row, a
/// `test-binary-finished` record or a decision of a `stage-finished` record, which comes before
/// its check's row does. NEGATIVE CONTROLS: a file not there yet, another event, plain text
/// naming the event, a malformed record and a line that has not ended count nothing, and passed
/// rows, binaries and stages, a split red row's first half and a malformed red row or binary
/// leave it passed.
#[test]
fn a_runs_events_report_its_complete_stage_boundaries_and_any_red() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let stage = crate::report::stage_event(
        0,
        Some(crate::checks::Check::Fmt),
        &crate::ladder::Report::new("fmt"),
        Duration::ZERO,
        Duration::ZERO,
        None,
    );
    let mut value: aterm_json::Value =
        aterm_json::from_str(stage.trim_end().strip_prefix("verify-event: ").unwrap()).unwrap();
    value.as_object_mut().unwrap().insert(
        "invocation".into(),
        aterm_json::json!({"id": "12".repeat(16), "source_observation": null, "identity_error": null}),
    );
    let attributed = format!("verify-event: {value}\n");
    let (head, tail) = attributed.split_at(attributed.len() / 2);
    let row = |outcome: &str| {
        format!(
            "verify-event: {{\"schema\":1,\"event\":\"check-finished\",\"check\":\"test\",\
             \"outcome\":\"{outcome}\",\"findings\":[],\"findings_total\":0}}\n"
        )
    };
    let (red, ok, skip) = (row("FAIL"), row("ok"), row("skip"));
    let (red_head, red_tail) = red.split_at(red.len() / 2);
    let report = |boundaries, unpassed| Report {
        boundaries,
        unpassed,
    };
    let events = dir.path().join("verify.events");
    let append = |events: &Path, text: &str| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(events)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    };
    let mut tail_of = EventsTail::default();
    assert_eq!(tail_of.read(&events), report(0, false), "no file yet");
    for (text, read) in [
        (ok.as_str(), report(0, false)),
        (head, report(0, false)),
        (tail, report(1, false)),
        (
            "the stage-finished event, said in plain text\n",
            report(1, false),
        ),
        (
            "verify-event: {\"event\":\"stage-finished\"\n",
            report(1, false),
        ),
        (stage.as_str(), report(2, false)),
        ("", report(2, false)),
        (stage.trim_end(), report(2, false)),
        ("\n", report(3, false)),
        (skip.as_str(), report(3, false)),
        (
            "verify-event: {\"event\":\"check-finished\",\"outcome\":\"FAIL\"}\n",
            report(3, false),
        ),
        (red_head, report(3, false)),
        (red_tail, report(3, true)),
        (stage.as_str(), report(4, true)),
        (ok.as_str(), report(4, true)),
    ] {
        append(&events, text);
        assert_eq!(tail_of.read(&events), read, "after {text:?}");
    }
    let not_run = dir.path().join("not-run.events");
    let mut tail_of = EventsTail::default();
    append(&not_run, &row("NOT RUN"));
    assert_eq!(tail_of.read(&not_run), report(0, true));

    let (binary, deciding) = (binary_line, deciding_stage);
    for (case, (red, lines)) in [
        (
            false,
            vec![binary("ok"), binary("skip"), deciding(&["ok", "skip"])],
        ),
        (
            false,
            vec![
                "verify-event: {\"event\":\"test-binary-finished\",\"outcome\":\"FAIL\"}\n"
                    .to_owned(),
                binary("FAIL").trim_end().to_owned(),
            ],
        ),
        (true, vec![binary("ok"), binary("FAIL")]),
        (true, vec![binary("NOT RUN")]),
        (true, vec![deciding(&["ok", "FAIL"])]),
        (true, vec![deciding(&["NOT RUN"])]),
    ]
    .into_iter()
    .enumerate()
    {
        let events = dir.path().join(format!("{case}.events"));
        let mut tail_of = EventsTail::default();
        for line in &lines {
            append(&events, line);
        }
        let stages = lines
            .iter()
            .filter(|line| line.contains("stage-finished") && line.ends_with('\n'))
            .count();
        assert_eq!(tail_of.read(&events), report(stages, red), "{lines:#?}");
    }
}

/// A `test-binary-finished` events line whose outcome is `outcome`.
fn binary_line(outcome: &str) -> String {
    format!(
        "verify-event: {{\"schema\":1,\"event\":\"test-binary-finished\",\
         \"test\":\"-p fixture --lib\",\"outcome\":\"{outcome}\",\"exit_code\":101}}\n"
    )
}

/// A `stage-finished` events line ([`crate::report::stage_event`]) whose decisions have
/// `outcomes`.
fn deciding_stage(outcomes: &[&str]) -> String {
    let stage = crate::report::stage_event(
        0,
        Some(crate::checks::Check::Test),
        &crate::ladder::Report::new("test"),
        Duration::ZERO,
        Duration::ZERO,
        None,
    );
    let mut value: aterm_json::Value =
        aterm_json::from_str(stage.trim_end().strip_prefix("verify-event: ").unwrap()).unwrap();
    value.as_object_mut().unwrap().insert(
        "decisions".into(),
        aterm_json::Value::Array(
            outcomes
                .iter()
                .map(|outcome| aterm_json::json!({"outcome": outcome, "detail": "a row"}))
                .collect(),
        ),
    );
    format!("verify-event: {value}\n")
}

/// [`Scripted`], its events written to a file at their offsets on its clock and read by the
/// production reader ([`EventsTail`]).
struct Written {
    run: Scripted,
    events: std::path::PathBuf,
    lines: std::collections::VecDeque<(Duration, String)>,
    tail: EventsTail,
}

impl Running for Written {
    fn wait(&mut self, within: Duration) -> std::io::Result<Option<i32>> {
        self.run.wait(within)
    }

    fn reported(&mut self) -> Report {
        use std::io::Write as _;
        let now = self.run.clock.get();
        while let Some((_, line)) = self.lines.front().filter(|(at, _)| *at <= now).cloned() {
            self.lines.pop_front();
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.events)
                .unwrap()
                .write_all(line.as_bytes())
                .unwrap();
        }
        self.tail.read(&self.events)
    }

    fn interrupt(&mut self) {
        self.run.interrupt();
    }
}

/// A red test binary or stage, which a check reports before its own row, ends the polling as a
/// red row does: through the production events reader, a run that reported either is decided by
/// its own end, though every poll after it would answer `true`. NEGATIVE CONTROL: the same run
/// with a passing binary in its place is interrupted at the next boundary.
#[test]
fn a_red_test_binary_or_stage_its_check_has_not_yet_rowed_ends_the_polling() {
    let secs = Duration::from_secs;
    let start = Instant::now();
    let clock = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
    let dir = aterm_tempfile::tempdir().expect("scratch");
    for (case, (red, ending)) in [
        (binary_line("FAIL"), Ending::Exited(exit::FAILED)),
        (
            deciding_stage(&["ok", "FAIL"]),
            Ending::Exited(exit::FAILED),
        ),
        (binary_line("ok"), Ending::Interrupted),
    ]
    .into_iter()
    .enumerate()
    {
        let mut run = Scripted::new(&clock, &[]);
        run.ends.set(Some(secs(1000)));
        run.code = exit::FAILED;
        let boundary = deciding_stage(&["ok"]);
        let mut run = Written {
            run,
            events: dir.path().join(format!("{case}.events")),
            lines: [
                (10, boundary.clone()),
                (15, red),
                (20, boundary.clone()),
                (30, boundary),
            ]
            .into_iter()
            .map(|(at, line)| (secs(at), line))
            .collect(),
            tail: EventsTail::default(),
        };
        let mut polled = Vec::new();
        let got = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
            polled.push(clock.get());
            clock.get() >= secs(20)
        })
        .unwrap();
        assert_eq!(got, ending, "case {case}: {polled:?}");
        let interrupted = run.run.interrupted;
        if ending == Ending::Interrupted {
            assert_eq!((polled.len(), interrupted), (2, 1), "{polled:?}");
        } else {
            assert_eq!((polled.len(), interrupted), (1, 0), "{polled:?}");
            assert_eq!(clock.get(), secs(1000));
        }
    }
}

/// What a signal to a judgment's group does not reach, by a read of the process table: every
/// process outside the group that its members started, and theirs in turn at any depth (a
/// verifier Targo's run left in a group of its own, its stage group and that stage's child), each
/// by its pid and birth. Read again once the group is gone, it is those of them still running (an
/// exec in place, which moves an Apple pid version, included) and what they started since.
/// NEGATIVE CONTROLS: the group's own members, the land process that started it, a bystander
/// and its child are never named, nor a later process given an ended one's pid (another birth),
/// and with nothing known an empty group names nothing.
#[test]
fn what_a_judgments_group_started_outside_it_is_read_at_any_depth_by_pid_and_birth() {
    use aterm_uds::process::{Birth, Identity, Lineage};
    let row = |pid: i32, birth: u64, version: i32, parent: u32, pgid: u32| Lineage {
        identity: Identity {
            pid,
            birth: Birth(birth),
            idversion: Some(version),
        },
        uniqueid: birth,
        parent,
        pgid,
        launcher_version: None,
        tty: None,
    };
    let names = |found: &[Started]| -> BTreeSet<(i32, u64)> {
        found.iter().map(|one| (one.pid, one.birth.0)).collect()
    };
    let group = 100;
    let first = [
        // The land process, leading its own group, and the judgment's leader (Targo's bootstrap)
        // with a compiler in its group, which started a process in a group of its own.
        row(50, 1, 1, 1, 50),
        row(100, 2, 1, 50, 100),
        row(101, 3, 1, 100, 100),
        row(102, 4, 1, 101, 102),
        // The verifier Targo's run left in a group of its own, its stage group, the stage's child.
        row(200, 5, 1, 100, 200),
        row(300, 6, 1, 200, 300),
        row(301, 7, 1, 300, 300),
        // A bystander and its child.
        row(400, 8, 1, 1, 400),
        row(401, 9, 1, 400, 400),
    ];
    let found = beyond(&first, group, &[]);
    assert_eq!(
        names(&found),
        BTreeSet::from([(102, 4), (200, 5), (300, 6), (301, 7)])
    );
    // The group is gone: launchd adopted the verifier, which exec'd in place; the compiler's
    // process ended and a later one took its pid; the stage started another child.
    let again = [
        row(50, 1, 1, 1, 50),
        row(102, 20, 1, 1, 102),
        row(200, 5, 2, 1, 200),
        row(300, 6, 1, 200, 300),
        row(301, 7, 1, 300, 300),
        row(302, 21, 1, 300, 302),
        row(400, 8, 1, 1, 400),
        row(401, 9, 1, 400, 400),
    ];
    assert_eq!(
        names(&beyond(&again, group, &found)),
        BTreeSet::from([(200, 5), (300, 6), (301, 7), (302, 21)])
    );
    assert!(beyond(&again, group, &[]).is_empty());
}

/// A complete small Keep-a-Changelog document whose Unreleased Fixed entries are entries.
fn fixed_changelog(entries: &[&str]) -> String {
    format!(
        "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n{}## [0.1.0] - 2026-11-01\n\n### Fixed\n\n- **Already shipped.** Earlier.\n",
        entries.concat()
    )
}

/// Put base on main and the feature, then leave tip on the feature and main on the rival.
fn divergent_changelog(base: &str, tip: &str, main: &str) -> World {
    let world = world();
    rival_moves(&world, "CHANGELOG.md", base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&world.work, "CHANGELOG.md", tip);
    rival_moves(&world, "CHANGELOG.md", main);
    world
}

/// What Git's union driver writes for the same immutable base, tip and main.
/// This is the negative control for the entry merge, in a separate scratch repository.
fn raw_union(base: &str, tip: &str, main: &str) -> String {
    let world = world();
    rival_moves(
        &world,
        ".gitattributes",
        "# Negative control only.\nCHANGELOG.md merge=union\n",
    );
    rival_moves(&world, "CHANGELOG.md", base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&world.work, "CHANGELOG.md", tip);
    rival_moves(&world, "CHANGELOG.md", main);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(
        &world.work,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "raw union negative control",
            "origin/main",
        ],
    );
    std::fs::read_to_string(world.work.join("CHANGELOG.md")).expect("union changelog")
}

fn landed_changelog(world: &World) -> (String, Vec<String>, Vec<String>) {
    let (code, said, bases) = land_with(world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let text = git(
        &world.work,
        &["show", &format!("{}:CHANGELOG.md", origin_main(world))],
    );
    (text, said, bases)
}

/// A branch that never touched CHANGELOG.md lands after main rewrote two entries of one
/// subsection since the fork, and takes main's file byte for byte. The entry merge used to
/// classify main's own rewrite as ambiguous and refuse the landing, naming entries only main
/// could change, until the branch merged main by hand.
#[test]
fn a_branch_that_left_the_changelog_lands_over_mains_multi_entry_rewrite() {
    let base = fixed_changelog(&["- **Old A.** First.\n\n", "- **Old B.** Second.\n\n"]);
    let main = fixed_changelog(&["- **New A.** First.\n\n", "- **New B.** Second.\n\n"]);

    let world = world();
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&world.work, "a/x.txt", "the branch's own change\n");
    rival_moves(&world, "CHANGELOG.md", &main);

    let (landed, said, _) = landed_changelog(&world);
    assert_eq!(landed, main.trim_end(), "{said:#?}");
}

const REAL_QUALIFICATION: &str = concat!(
    "- Release qualification validates exports privately beside the tree and build, then stages\n",
    "  only after their verdicts pass, so a failed tree cannot strand a new staged line.\n\n",
);
const REAL_HARNESS: &str = "\
- **Harness versions since v0.192.0: `alab claude harness 0.31`.** Each harness's minor moves once per release in which it changed.\n\n";
const REAL_NODE_OLD: &str = concat!(
    "- **An idle `aterm node serve` wakes less often.** Its fleet listener and LAN toolset sharing wait\n",
    "  for a connection or a setting change instead of looking five and four times a second.\n\n",
);
const REAL_NODE_NEW: &str = concat!(
    "- **An idle `aterm node serve` no longer polls to share its LAN toolset.** Toolset sharing waits\n",
    "  for a connection or a setting change instead of looking four times a second, as its fleet\n",
    "  listener already did in 0.193.0.\n\n",
);
const REAL_IDLE_MAC: &str = concat!(
    "- **An idle Mac no longer wakes aterm ten times a second for Commons.** Commons repair now waits\n",
    "  for a peer's beacon, a connection, the end of a round or its next scheduled step: measured over\n",
    "  20 seconds on one Mac, an idle one went from about 9 wakeups a second to about 1.4. A background\n",
    "  `aterm node serve` now hands Commons to a window you open within about 2 seconds.\n\n",
);
const REAL_YOLO: &str = concat!(
    "- **Claude Code's permission chip reads `○ yolo` or `● yolo`, and a click only turns yolo on.**\n",
    "  The chip beside the Autopilot chip no longer says `lame` when Claude Code is not in bypass\n",
    "  permissions: it shows a quiet `○ yolo`, and `● yolo` when it is, in the same plain ink as the\n",
    "  other values on the line.\n\n",
);
const REAL_RELEASE: &str = concat!(
    "## [0.193.0] - 2026-11-10\n\n",
    "### Fixed\n\n",
    "- **Release previews use the selected release's tools.** A dry run freezes and executes\n",
    "  the selected cutter before preflight.\n",
);

fn real_934c_base() -> String {
    format!(
        "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n{REAL_QUALIFICATION}\
         ### Changed\n\n{REAL_HARNESS}{REAL_NODE_OLD}\
         ### Fixed\n\n{REAL_IDLE_MAC}{REAL_RELEASE}"
    )
}

fn real_4b899_tip(base: &str) -> String {
    base.replacen(REAL_HARNESS, &format!("{REAL_YOLO}{REAL_HARNESS}"), 1)
}

fn real_827aa_main() -> String {
    format!(
        "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n{REAL_QUALIFICATION}{REAL_IDLE_MAC}\
         ### Changed\n\n{REAL_HARNESS}{REAL_NODE_NEW}{REAL_RELEASE}"
    )
}

/// THE REAL 0.194 SHAPE MERGES BY ENTRY. These are the relevant literal entries and structure
/// from base 934c557d (blob 4a32e4f8), tip 4b899af (acd6625c) and main 827aa6f
/// (ec208a55). Main's rewrite replaces the old node-serve entry, main's one Fixed subsection
/// wins, and the tip's yolo addition keeps its slot before the harness entry.
/// NEGATIVE CONTROL: Git's union result has both node-serve wordings and Changed twice, the
/// defect published by 7d1464c's blob 0bf0e574 in v0.194.0.
#[test]
fn real_7d1464c_shape_merges_entries_instead_of_unioning_lines() {
    let base = real_934c_base();
    let tip = real_4b899_tip(&base);
    let main = real_827aa_main();
    let expected = main.replacen(REAL_HARNESS, &format!("{REAL_YOLO}{REAL_HARNESS}"), 1);

    let union = raw_union(&base, &tip, &main);
    assert!(union.contains(REAL_NODE_OLD.trim()), "{union}");
    assert!(union.contains(REAL_NODE_NEW.trim()), "{union}");
    assert_eq!(union.matches("### Changed").count(), 2, "{union}");

    let world = divergent_changelog(&base, &tip, &main);
    let (landed, said, _) = landed_changelog(&world);
    assert_eq!(landed, expected.trim_end(), "{said:#?}");
    assert!(!landed.contains(REAL_NODE_OLD.trim()));
    assert_eq!(landed.matches("### Changed").count(), 1);
    assert_eq!(landed.matches("### Fixed").count(), 2);
}

/// The compact shape of the bad 0.195.0 input: main cut the stale harness record into 0.194.0
/// while the branch added its own fix above the old Unreleased copy and retained two Fixed
/// headings. The union merge left both defects for the next cutter to publish.
/// NEGATIVE CONTROL: union retains the stale line under Unreleased and both Fixed headings.
#[test]
fn real_0195_shape_removes_the_cut_entry_and_collapses_fixed() {
    const STALE: &str = "- `alab claude harness 0.31` records the upgrade-drive changes since the previous public release.\n\n";
    const AGH: &str = "- **`agh` killed mid-fetch no longer leaves Git running.** Fixed.\n\n";
    const HARNESS: &str =
        "- **Harness versions since v0.193.0: `alab claude harness 0.31`.** One record.\n\n";
    const TAIL: &str = "- Fleet status refreshes keep their original deadline.\n\n";
    const BRANCH_FIX: &str =
        "- **`verify land` joins a kind its merge opened twice.** One heading remains.\n\n";
    const OLD_RELEASE: &str =
        "## [0.193.0] - 2026-11-10\n\n### Fixed\n\n- **Earlier.** Already shipped.\n";

    let base = format!(
        "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n{STALE}{AGH}### Changed\n\n{HARNESS}### Fixed\n\n{TAIL}{OLD_RELEASE}"
    );
    let tip = base.replacen(STALE, &format!("{BRANCH_FIX}{STALE}"), 1);
    let release = format!("## [0.194.0] - 2026-11-10\n\n### Fixed\n\n{STALE}{OLD_RELEASE}");
    let main = format!(
        "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n{AGH}### Changed\n\n{HARNESS}### Fixed\n\n{TAIL}{release}"
    );

    let union = raw_union(&base, &tip, &main);
    let union_unreleased = union.split_once("## [0.194.0]").expect("union release").0;
    assert!(union_unreleased.contains(STALE.trim()), "{union}");
    assert_eq!(union_unreleased.matches("### Fixed").count(), 2, "{union}");

    let world = divergent_changelog(&base, &tip, &main);
    let (landed, said, _) = landed_changelog(&world);
    let (landed_unreleased, landed_release) =
        landed.split_once("## [0.194.0]").expect("landed release");
    assert!(!landed_unreleased.contains(STALE.trim()), "{landed}");
    assert_eq!(
        landed_unreleased.matches("### Fixed").count(),
        1,
        "{landed}"
    );
    assert!(landed_unreleased.contains(BRANCH_FIX.trim()), "{landed}");
    assert_eq!(
        landed_release,
        main.split_once("## [0.194.0]")
            .expect("main release")
            .1
            .trim_end(),
        "main's released section changed: {said:#?}"
    );
}

/// A one-sided rewrite remains one entry when main adds immediately beside it.
/// NEGATIVE CONTROL: the union driver keeps the old and rewritten copies.
#[test]
fn a_branch_rewrite_replaces_old_words_beside_mains_nearby_addition() {
    const OLD: &str = "- **Idle worker.** Polls for work.\n  This is the old explanation.\n\n";
    const NEW: &str = "- **Idle worker.** Waits for work.\n  This is the precise explanation.\n\n";
    const NEARBY: &str = "- **Main's nearby fix.** Written while the branch ran.\n\n";
    const ANCHOR: &str = "- **Stable anchor.** Unchanged after both edits.\n\n";
    let base = fixed_changelog(&[OLD, ANCHOR]);
    let tip = fixed_changelog(&[NEW, ANCHOR]);
    let main = fixed_changelog(&[NEARBY, OLD, ANCHOR]);
    let expected = fixed_changelog(&[NEARBY, NEW, ANCHOR]);

    let union = raw_union(&base, &tip, &main);
    assert!(union.contains(OLD.trim()), "{union}");
    assert!(union.contains(NEW.trim()), "{union}");

    let world = divergent_changelog(&base, &tip, &main);
    let (landed, said, _) = landed_changelog(&world);
    assert_eq!(landed, expected.trim_end(), "{said:#?}");
    assert!(!landed.contains(OLD.trim()));
    assert_eq!(landed.matches("**Idle worker.**").count(), 1);
}

/// A release cut which arrives during the first judgment is merged before the re-judge. Main's
/// released section stays byte-for-byte as main cut it and the branch-only addition goes into
/// the fresh Unreleased section.
/// NEGATIVE CONTROL: a raw union strands the branch entry inside 0.2.0.
#[test]
fn a_rejudge_release_cut_keeps_mains_release_exact_and_branch_addition_unreleased() {
    const MAIN_ENTRY: &str = "- **Main's fix.** Before the cut.\n\n";
    const BRANCH_ENTRY: &str =
        "- **The branch's fix.** Written before the cut,\n  landed after it.\n\n";
    let base = fixed_changelog(&[MAIN_ENTRY]);
    let tip = fixed_changelog(&[BRANCH_ENTRY, MAIN_ENTRY]);
    let main = base.replacen(
        "## [Unreleased]\n\n",
        "## [Unreleased]\n\n## [0.2.0] - 2026-11-10\n\n",
        1,
    );
    let expected = main.replacen(
        "## [Unreleased]\n\n",
        &format!("## [Unreleased]\n\n### Fixed\n\n{BRANCH_ENTRY}"),
        1,
    );

    let union = raw_union(&base, &tip, &main);
    let union_release = union
        .split_once("## [0.2.0] - 2026-11-10")
        .expect("union release")
        .1;
    assert!(union_release.contains(BRANCH_ENTRY.trim()), "{union}");

    let world = world();
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&world.work, "CHANGELOG.md", &tip);
    let checked = RefCell::new(Vec::new());
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, n| {
        checked
            .borrow_mut()
            .push(git(&world.work, &["rev-parse", "HEAD"]));
        if n == 0 {
            rival_moves(&world, "CHANGELOG.md", &main);
        }
        green()
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 2, "{said:#?}");
    let landed = git(
        &world.work,
        &["show", &format!("{}:CHANGELOG.md", origin_main(&world))],
    );
    assert_eq!(landed, expected.trim_end(), "{said:#?}");
    assert_eq!(
        landed
            .split_once("## [0.2.0] - 2026-11-10")
            .expect("landed release")
            .1,
        main.split_once("## [0.2.0] - 2026-11-10")
            .expect("main release")
            .1
            .trim_end(),
        "the released section is main's exact section"
    );
    let checked = checked.into_inner();
    let landed_tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
    assert!(
        !receipt_composition(
            &world.work,
            &std::env::var_os("PATH").unwrap_or_default(),
            &checked[0],
            &bases[1],
            &landed_tree,
        )
        .expect("the receipt reader uses the writer's main/checked orientation"),
        "a semantic CHANGELOG merge must be rejudged, never composed"
    );
}

/// A rewrite that races the cut is new Unreleased text, not a retroactive edit to the release.
/// The rule is the same for the initial merge and a cut which arrives during judgment.
#[test]
fn a_rewritten_entry_racing_a_release_cut_stays_unreleased() {
    const OLD: &str = "- **Stable title.** Old words before the cut.\n\n";
    const NEW: &str = "- **Stable title.** Reviewed replacement after the cut.\n\n";
    let base = fixed_changelog(&[OLD]);
    let tip = fixed_changelog(&[NEW]);
    let main = base.replacen(
        "## [Unreleased]\n\n",
        "## [Unreleased]\n\n## [0.2.0] - 2026-11-10\n\n",
        1,
    );
    let expected = main.replacen(
        "## [Unreleased]\n\n",
        &format!("## [Unreleased]\n\n### Fixed\n\n{NEW}"),
        1,
    );

    let union = raw_union(&base, &tip, &main);
    let (union_unreleased, union_release) = union
        .split_once("## [0.2.0] - 2026-11-10")
        .expect("union release");
    assert!(!union_unreleased.contains(NEW.trim()), "{union}");
    assert!(union_release.contains(NEW.trim()), "{union}");
    assert!(!union_release.contains(OLD.trim()), "{union}");

    for during_check in [false, true] {
        let world = world();
        rival_moves(&world, "CHANGELOG.md", &base);
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
        commit(&world.work, "CHANGELOG.md", &tip);
        if !during_check {
            rival_moves(&world, "CHANGELOG.md", &main);
        }
        let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, n| {
            if during_check && n == 0 {
                rival_moves(&world, "CHANGELOG.md", &main);
            }
            green()
        });
        assert_eq!(code, exit::PASS, "{during_check}: {said:#?}");
        assert_eq!(bases.len(), if during_check { 2 } else { 1 });
        let landed = git(
            &world.work,
            &["show", &format!("{}:CHANGELOG.md", origin_main(&world))],
        );
        assert_eq!(landed, expected.trim_end(), "{during_check}: {said:#?}");
    }
}

/// An entry merge that cannot decide composes nothing. The ordinary landing merge must surface
/// the named conflict, while a historical receipt remains uncomposed rather than falsely proving
/// that moving main was independent.
#[test]
fn an_undecidable_entry_merge_composes_nothing() {
    const OLD: &str = "- **Same entry.** Original words.\n\n";
    const TIP: &str = "- **Same entry.** Branch rewrite.\n\n";
    const MAIN: &str = "- **Same entry.** Main rewrite.\n\n";
    let world = world();
    rival_moves(&world, "CHANGELOG.md", &fixed_changelog(&[OLD]));
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    let checked = commit(&world.work, "CHANGELOG.md", &fixed_changelog(&[TIP]));
    let main = rival_moves(&world, "CHANGELOG.md", &fixed_changelog(&[MAIN]));
    git(&world.work, &["fetch", "-q", "origin"]);
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let refused = plumbing
        .changelog_merge(&checked, &main)
        .expect("Git plumbing works")
        .expect_err("different rewrites are undecidable");
    assert!(
        refused
            .entries
            .iter()
            .any(|entry| entry.starts_with("Same entry")),
        "{}: {:?}",
        refused.reason,
        refused.entries
    );
    assert_eq!(
        plumbing.merge_tree(&main, &checked),
        Ok(Compose::Refused(refused.clone()))
    );
}

/// A MERGE THAT CARRIED MAIN'S MALFORMED CHANGELOG REFUSES NO LATER LANDING (the TM-1 review
/// branch, 2026-11-10, whose old merges carried main's changelogs of 2026-11-07 and 2026-11-10,
/// which the reader of their day refused): main held a changelog the reader refuses (here a
/// repeated release section) and the branch, which never writes the changelog, merged it; main
/// repaired it and the branch merged that; then main rewords an entry the branch still holds.
/// The authorship walk passes both merges, each changelog one parent's byte for byte, unparsed,
/// and the branch lands with main's changelog. NEGATIVE CONTROL: a commit that writes its own
/// change atop the malformed text is still read, and still refused.
#[test]
fn a_merge_that_carried_mains_malformed_changelog_refuses_no_later_landing() {
    const MAINS: &str = "- **Main's fix.** Before the cut.\n\n";
    let base = fixed_changelog(&[MAINS]);
    let world = world();
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&world.work, "a/x.txt", "a, the branch's\n");
    let malformed = format!(
        "{base}\n## [0.1.0] - 2026-11-01\n\n### Fixed\n\n- **A repeated release.** Main's own.\n"
    );
    let repeated = rival_moves(&world, "CHANGELOG.md", &malformed);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
    let carried = git(&world.work, &["rev-parse", "HEAD"]);
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
    let branch = git(&world.work, &["rev-parse", "HEAD"]);
    let reworded = fixed_changelog(&["- **Main's fix.** Reworded.\n\n"]);
    let main = rival_moves(&world, "CHANGELOG.md", &reworded);
    git(&world.work, &["fetch", "-q", "origin"]);
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let merged = plumbing
        .changelog_merge(&branch, &main)
        .expect("Git plumbing works");
    assert!(merged.is_ok(), "{merged:?}");

    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases, std::slice::from_ref(&main), "{said:#?}");
    assert_eq!(
        git(
            &world.work,
            &["show", &format!("{}:CHANGELOG.md", origin_main(&world))]
        ),
        reworded.trim_end()
    );

    git(&world.work, &["checkout", "-q", "-b", "control", &carried]);
    let authored = commit(
        &world.work,
        "CHANGELOG.md",
        &malformed.replacen(
            MAINS,
            &format!("- **The branch's own.** Written.\n\n{MAINS}"),
            1,
        ),
    );
    let entry = changelog::removed_entries(&base, &reworded)
        .expect("both parse")
        .into_iter()
        .next()
        .expect("main rewrote its fix");
    let refused = plumbing
        .entry_written_since(&repeated, &authored, &entry)
        .unwrap_err();
    assert!(refused.contains("repeats section"), "{refused}");
}

/// Entries both sides add are no conflict: the worktree merge combines them and that tree is
/// judged, which the land says apart from Git's conflict and a refused entry merge (a branch's
/// harness announcement met main's changelog edits, 2026-11-10, and was told "not clean").
#[test]
fn entries_both_sides_add_compose_through_the_worktree_merge() {
    const BRANCH: &str = "- **The branch's fix.** Written on the branch.\n\n";
    const MAIN: &str = "- **Main's second fix.** Written on main.\n\n";
    let base = fixed_changelog(&[]);
    let tip = fixed_changelog(&[BRANCH]);
    let main_text = fixed_changelog(&[MAIN]);
    let world = divergent_changelog(&base, &tip, &main_text);
    let checked = git(&world.work, &["rev-parse", "HEAD"]);
    let main = origin_main(&world);
    git(&world.work, &["fetch", "-q", "origin"]);
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    assert!(
        matches!(plumbing.changelog_merge(&checked, &main), Ok(Ok(Some(_)))),
        "both sides' entries merge"
    );
    assert_eq!(plumbing.merge_tree(&main, &checked), Ok(Compose::Entries));

    // A branch that never changed the changelog needs no second judgment when main
    // alone adds an entry or cuts a release during its first judgment.
    for (release_cut, compact) in [(false, false), (true, false), (false, true)] {
        let world = self::world();
        let base_text = fixed_changelog(&[MAIN]);
        let base = rival_moves(&world, "CHANGELOG.md", &base_text);
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
        let branch = commit(&world.work, "a/x.txt", "a real branch-only change\n");
        let base_entry = git(&world.work, &["ls-tree", &base, "--", "CHANGELOG.md"]);
        assert_eq!(
            git(&world.work, &["ls-tree", &branch, "--", "CHANGELOG.md"]),
            base_entry
        );
        let incoming = if release_cut {
            base_text.replacen(
                "## [Unreleased]\n\n",
                "## [Unreleased]\n\n## [0.2.0] - 2026-11-10\n\n",
                1,
            )
        } else {
            fixed_changelog(&[BRANCH, MAIN])
        };
        let incoming = if compact {
            incoming.replace("\n\n-", "\n-")
        } else {
            incoming
        };
        let checked = RefCell::new(Vec::new());
        let moved = RefCell::new(String::new());
        let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, n| {
            checked
                .borrow_mut()
                .push(git(&world.work, &["rev-parse", "HEAD"]));
            if n == 0 {
                *moved.borrow_mut() = rival_moves(&world, "CHANGELOG.md", &incoming);
            }
            green()
        });
        assert_eq!(code, exit::PASS, "{said:#?}");
        let moved = moved.into_inner();
        let checked = checked.into_inner();
        let landing = origin_main(&world);
        assert_eq!(
            git(&world.work, &["ls-tree", &checked[0], "--", "CHANGELOG.md"]),
            base_entry
        );
        assert_eq!(
            git(&world.work, &["merge-base", "--all", &checked[0], &moved]),
            base
        );
        assert_eq!(
            git(&world.work, &["show", &format!("{landing}:CHANGELOG.md")]),
            incoming.trim_end()
        );
        assert_eq!(
            git(&world.work, &["ls-tree", &landing, "--", "CHANGELOG.md"]),
            git(&world.work, &["ls-tree", &moved, "--", "CHANGELOG.md"])
        );
        assert_eq!(
            bases.len(),
            1,
            "incoming-only validated entries need no second judgment: {said:#?}"
        );
        let receipt = stated(&world, &landing).unwrap();
        assert_eq!(receipt.checked.len(), 1);
        assert_eq!(receipt.composed_onto.as_deref(), Some(moved.as_str()));
        let path = std::env::var_os("PATH").unwrap_or_default();
        assert!(
            receipt_composition(
                &world.work,
                &path,
                &checked[0],
                &moved,
                &git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")])
            )
            .unwrap()
        );
        assert!(
            !receipt_composition(
                &world.work,
                &path,
                &checked[0],
                &moved,
                &git(
                    &world.work,
                    &["rev-parse", &format!("{}^{{tree}}", checked[0])]
                )
            )
            .unwrap(),
            "a tree retaining the old changelog is not the proven incoming result"
        );
    }

    // Equal changelog bytes do not prove that the branch left its complete entry unchanged.
    let mode = self::world();
    let base = rival_moves(&mode, "CHANGELOG.md", &fixed_changelog(&[]));
    git(&mode.work, &["fetch", "-q", "origin"]);
    git(&mode.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&mode.work, "a/x.txt", "branch change\n");
    git(&mode.work, &["update-index", "--chmod=+x", "CHANGELOG.md"]);
    git(
        &mode.work,
        &["commit", "-qm", "change the changelog entry mode"],
    );
    let checked = git(&mode.work, &["rev-parse", "HEAD"]);
    let main = rival_moves(&mode, "CHANGELOG.md", &fixed_changelog(&[MAIN]));
    git(&mode.work, &["fetch", "-q", "origin"]);
    assert_eq!(
        git(&mode.work, &["rev-parse", &format!("{base}:CHANGELOG.md")]),
        git(
            &mode.work,
            &["rev-parse", &format!("{checked}:CHANGELOG.md")]
        )
    );
    assert!(
        git(&mode.work, &["ls-tree", &checked, "--", "CHANGELOG.md"]).starts_with("100755 blob ")
    );
    assert_eq!(
        Git {
            root: &mode.work,
            make: &git_cmd
        }
        .merge_tree(&main, &checked),
        Ok(Compose::Entries)
    );
}

/// Real release-gate landings repeatedly passed while unrelated main notes
/// changed. Main's adjacent bullets lacked a blank separator, so serializing
/// the entry model inserted one and needlessly restarted the source checks.
#[test]
fn unchanged_changelog_composes_main_notes_without_rejudging_source() {
    let base = fixed_changelog(&[]);
    let main_text = fixed_changelog(&[
        "- **Main's fix.** Added during source checks.\n",
        "- **Another main fix.** Preserve main's written spacing.\n\n",
    ]);
    let world = world();
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&world.work, "a/x.txt", "checked source\n");
    let checked = RefCell::new(None);
    let moved = RefCell::new(None);
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, n| {
        *checked.borrow_mut() = Some(git(&world.work, &["rev-parse", "HEAD"]));
        if n == 0 {
            *moved.borrow_mut() = Some(rival_moves(&world, "CHANGELOG.md", &main_text));
        }
        green()
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 1, "main-only notes must compose: {said:#?}");
    let landing = origin_main(&world);
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:CHANGELOG.md")]),
        main_text.trim_end()
    );
    let moved = moved.into_inner().unwrap();
    assert_eq!(
        stated(&world, &landing).unwrap().composed_onto.as_deref(),
        Some(moved.as_str())
    );
    assert!(
        receipt_composition(
            &world.work,
            &std::env::var_os("PATH").unwrap_or_default(),
            &checked.into_inner().unwrap(),
            &moved,
            &git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")]),
        )
        .unwrap(),
        "the historical reader must accept the same exact composition"
    );
    // The adjacent both-sides-additions test is the negative control: that
    // changed branch changelog still requires an entry-wise merge and judgment.
}

#[test]
fn checked_changelog_composes_an_unchanged_main_endpoint_without_rejudging() {
    let base = fixed_changelog(&[]);
    let checked_text = fixed_changelog(&["- **Checked harness versions.** Already judged.\n\n"]);
    let world = world();
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&world.work, "a/x.txt", "checked source\n");
    commit(&world.work, "CHANGELOG.md", &checked_text);
    let checked = git(&world.work, &["rev-parse", "HEAD"]);
    let moved = RefCell::new(None);
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, n| {
        assert_eq!(n, 0, "the checked endpoint needs no new entry merge");
        *moved.borrow_mut() = Some(rival_moves(&world, "b/x.txt", "main source\n"));
        green()
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 1);
    let landing = origin_main(&world);
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:CHANGELOG.md")]),
        checked_text.trim_end()
    );
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:b/x.txt")]),
        "main source"
    );
    let moved = moved.into_inner().unwrap();
    assert_eq!(
        stated(&world, &landing).unwrap().composed_onto.as_deref(),
        Some(moved.as_str())
    );
    assert!(
        receipt_composition(
            &world.work,
            &std::env::var_os("PATH").unwrap_or_default(),
            &checked,
            &moved,
            &git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")]),
        )
        .unwrap()
    );
}

#[test]
fn an_unchecked_normalization_of_the_checked_changelog_still_needs_judgment() {
    let base = fixed_changelog(&[]);
    let canonical = fixed_changelog(&[
        "- **Checked entry.** Kept.\n\n",
        "- **Other entry.** Kept too.\n\n",
    ]);
    let checked_text = canonical.replace("Kept.\n\n-", "Kept.\n-");
    assert_ne!(checked_text, canonical);
    let world = world();
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    let checked = commit(&world.work, "CHANGELOG.md", &checked_text);
    let main = rival_moves(&world, "b/x.txt", "main source\n");
    git(&world.work, &["fetch", "-q", "origin"]);
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    assert_eq!(plumbing.merge_tree(&main, &checked), Ok(Compose::Entries));
}

#[test]
fn a_clean_noncanonical_changelog_still_requires_the_entry_merge() {
    let world = world();
    rival_moves(&world, ".gitattributes", "CHANGELOG.md merge=union\n");
    let base = real_934c_base();
    let tip = real_4b899_tip(&base);
    let main_text = real_827aa_main();
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    let checked = commit(&world.work, "CHANGELOG.md", &tip);
    let main = rival_moves(&world, "CHANGELOG.md", &main_text);
    git(&world.work, &["fetch", "-q", "origin"]);
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let ordinary = plumbing
        .complete(
            &[
                "merge-tree",
                "--write-tree",
                "--no-messages",
                &main,
                &checked,
            ],
            LOCAL,
        )
        .unwrap();
    assert!(
        ordinary.status.success(),
        "the unsafe union is a clean Git merge"
    );
    assert_eq!(plumbing.merge_tree(&main, &checked), Ok(Compose::Entries));
}

#[test]
fn canonical_changelog_bytes_do_not_waive_shared_path_independence() {
    const A: &str = "- **First entry.** Original.\n\n";
    const B: &str = "- **Last entry.** Original.\n\n";
    const MIDDLE: &str = "- **Middle one.** Unchanged.\n\n- **Middle two.** Unchanged.\n\n- **Middle three.** Unchanged.\n\n";
    let base = fixed_changelog(&[A, MIDDLE, B]);
    let tip = base.replace(A, "- **First entry.** Branch rewrite.\n\n");
    let main_text = base.replace(B, "- **Last entry.** Main rewrite.\n\n");
    let world = divergent_changelog(&base, &tip, &main_text);
    let checked = git(&world.work, &["rev-parse", "HEAD"]);
    let main = origin_main(&world);
    git(&world.work, &["fetch", "-q", "origin"]);
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    assert_eq!(plumbing.merge_tree(&main, &checked), Ok(Compose::Entries));
    let ordinary = plumbing
        .complete(
            &[
                "merge-tree",
                "--write-tree",
                "--no-messages",
                &main,
                &checked,
            ],
            LOCAL,
        )
        .unwrap();
    assert!(ordinary.status.success());
    let tree = String::from_utf8(ordinary.stdout).unwrap();
    let tree = tree.lines().next().unwrap();
    let base_commit = git(&world.work, &["merge-base", &main, &checked]);
    assert!(matches!(
        plumbing
            .independent_changes(&base_commit, &main, &checked, tree)
            .unwrap(),
        Independence::Blocked { .. }
    ));
}

/// If both sides independently wrote the same complete entry and main shipped it, the branch's
/// Unreleased copy is not published again. This convergent case still goes through entry identity.
#[test]
fn an_identical_independent_addition_main_shipped_is_not_duplicated() {
    const ENTRY: &str = "- **Same addition.** Independently written.\n\n";
    let base = fixed_changelog(&[]);
    let tip = fixed_changelog(&[ENTRY]);
    let main = format!(
        "# Changelog\n\n## [Unreleased]\n\n## [0.2.0] - 2026-11-10\n\n### Fixed\n\n{ENTRY}## [0.1.0] - 2026-11-01\n\n### Fixed\n\n- **Already shipped.** Earlier.\n"
    );
    let world = divergent_changelog(&base, &tip, &main);
    let (landed, said, _) = landed_changelog(&world);
    assert_eq!(landed, main.trim_end(), "{said:#?}");
    assert_eq!(landed.matches("**Same addition.**").count(), 1);
}

/// A person can remove an entry, hand-merge main with the branch tree, then explicitly restore
/// it. When the later main endpoint also keeps that entry, there is no opposing removal or
/// rewrite: the final merge keeps the restored bytes exactly once.
#[test]
fn a_persons_hand_restored_entry_is_kept_from_the_endpoint() {
    const RESTORED: &str = "- **Restored entry.** The person kept this deliberately.\n\n";
    const MAIN_ONE: &str = "- **Main one.** Arrived before the hand merge.\n\n";
    const MAIN_TWO: &str = "- **Main two.** Arrived after the hand merge.\n\n";
    let base = fixed_changelog(&[RESTORED]);
    let removed = fixed_changelog(&[]);
    let first_main = fixed_changelog(&[RESTORED, MAIN_ONE]);
    let final_main = fixed_changelog(&[RESTORED, MAIN_ONE, MAIN_TWO]);

    let world = world();
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&world.work, "CHANGELOG.md", &removed);
    rival_moves(&world, "CHANGELOG.md", &first_main);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(
        &world.work,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-s",
            "ours",
            "-m",
            "hand merge before restoring the entry",
            "origin/main",
        ],
    );
    commit(&world.work, "CHANGELOG.md", &first_main);
    rival_moves(&world, "CHANGELOG.md", &final_main);

    let (landed, said, _) = landed_changelog(&world);
    assert_eq!(landed, final_main.trim_end(), "{said:#?}");
    assert_eq!(landed.matches("**Restored entry.**").count(), 1);
}

/// Endpoint equality cannot hide a person's explicit restoration. A removal or rewrite on the
/// other side is opposing intent, so the landing stops before judgment and names the entry.
#[test]
fn an_explicit_restoration_racing_a_rewrite_is_a_named_premerge_conflict() {
    const OLD: &str = "- **Restored entry.** A person put these exact words back.\n\n";
    const NEW: &str = "- **Replacement entry.** The branch chose new words.\n\n";
    const MAIN: &str = "- **Main addition.** Independent.\n\n";
    let base = fixed_changelog(&[OLD]);
    let removed = fixed_changelog(&[]);
    let restored = fixed_changelog(&[MAIN, OLD]);
    let branch = fixed_changelog(&[NEW]);

    let world = world();
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&world.work, "CHANGELOG.md", &branch);
    rival_moves(&world, "CHANGELOG.md", &removed);
    let main = rival_moves(&world, "CHANGELOG.md", &restored);
    let before = git(&world.work, &["rev-parse", "HEAD"]);

    let (code, said, judged) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| {
        panic!("an authored restoration must be resolved before judgment")
    });
    assert_eq!(code, exit::FAILED, "{said:#?}");
    assert!(judged.is_empty(), "{judged:#?}");
    let report = said.join("\n");
    assert!(report.contains("explicitly restored"), "{report}");
    assert!(report.contains("Restored entry"), "{report}");
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), before);
    assert_eq!(origin_main(&world), main);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
}

/// A tip whose bytes equal the base can still be an authored delete-and-restore. The rule is
/// symmetric: restoration on either side conflicts with the other side's final removal.
#[test]
fn a_base_equal_endpoint_cannot_hide_an_authored_restoration_on_either_side() {
    const ENTRY: &str = "- **Restored entry.** Exact base bytes.\n\n";
    let base = fixed_changelog(&[ENTRY]);
    let removed = fixed_changelog(&[]);
    for restoring_main in [false, true] {
        let world = world();
        rival_moves(&world, "CHANGELOG.md", &base);
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
        let fork = git(&world.work, &["rev-parse", "HEAD"]);
        commit(&world.work, "a/x.txt", "branch-only history\n");
        if restoring_main {
            commit(&world.work, "CHANGELOG.md", &removed);
            rival_moves(&world, "CHANGELOG.md", &removed);
            rival_moves(&world, "CHANGELOG.md", &base);
        } else {
            commit(&world.work, "CHANGELOG.md", &removed);
            commit(&world.work, "CHANGELOG.md", &base);
            rival_moves(&world, "CHANGELOG.md", &removed);
        }
        let branch = git(&world.work, &["rev-parse", "HEAD"]);
        let main = origin_main(&world);
        git(&world.work, &["fetch", "-q", "origin"]);
        assert_eq!(git(&world.work, &["merge-base", &branch, &main]), fork);

        let (code, said, judged) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| {
            panic!("an authored restoration must be resolved before judgment")
        });
        assert_eq!(code, exit::FAILED, "{restoring_main}: {said:#?}");
        assert!(judged.is_empty(), "{judged:#?}");
        let report = said.join("\n");
        assert!(report.contains("explicitly restored"), "{report}");
        assert!(report.contains("Restored entry"), "{report}");
        assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), branch);
        assert_eq!(origin_main(&world), main);
        assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
    }
}

/// A restoration later superseded by the same side's final deletion is history, not current
/// intent. Both endpoints remove the old Fixed entry, so main's independently located Changed
/// addition lands. A same-subsection singleton replacement would deliberately remain ambiguous.
#[test]
fn a_superseded_restoration_does_not_override_the_final_shared_removal() {
    const OLD: &str = "- **Old entry.** Finally removed on both sides.\n\n";
    const MAIN: &str = "- **Main addition.** Remains current.\n\n";
    let base = fixed_changelog(&[OLD]);
    let removed = fixed_changelog(&[]);
    let final_main = fixed_changelog(&[]).replacen(
        "## [Unreleased]\n\n### Fixed\n\n",
        &format!("## [Unreleased]\n\n### Changed\n\n{MAIN}"),
        1,
    );

    let world = world();
    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&world.work, "CHANGELOG.md", &removed);
    rival_moves(&world, "CHANGELOG.md", &removed);
    rival_moves(&world, "CHANGELOG.md", &base);
    rival_moves(&world, "CHANGELOG.md", &final_main);

    let (landed, said, _) = landed_changelog(&world);
    assert_eq!(landed, final_main.trim_end(), "{said:#?}");
    assert!(!landed.contains("**Old entry.**"), "{landed}");
}

/// A merge can import stale words from an old parent without authoring them. Such an inherited
/// addition is not branch intent and cannot be resurrected beside main's current entry.
#[test]
fn a_merge_inherited_stale_entry_is_not_treated_as_a_branch_addition() {
    const STALE: &str = "- **Stale entry.** Removed before the feature fork.\n\n";
    const MAIN: &str = "- **Main addition.** Current text.\n\n";
    let old = fixed_changelog(&[STALE]);
    let base = fixed_changelog(&[]);
    let main = fixed_changelog(&[MAIN]);

    let world = world();
    rival_moves(&world, "CHANGELOG.md", &old);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    git(&world.work, &["checkout", "-q", "-b", "stale-parent"]);
    commit(
        &world.work,
        "a/x.txt",
        "stale parent retains the old changelog\n",
    );
    let stale_parent = git(&world.work, &["rev-parse", "HEAD"]);
    git(&world.work, &["checkout", "-q", "feature"]);

    rival_moves(&world, "CHANGELOG.md", &base);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    git(
        &world.work,
        &["merge", "-q", "--no-ff", "--no-commit", &stale_parent],
    );
    write(&world.work, "CHANGELOG.md", &old);
    git(&world.work, &["add", "CHANGELOG.md"]);
    git(
        &world.work,
        &[
            "commit",
            "-q",
            "-m",
            "merge stale parent without authoring its entry",
        ],
    );
    let branch = git(&world.work, &["rev-parse", "HEAD"]);
    let remote = rival_moves(&world, "CHANGELOG.md", &main);

    let (code, said, judged) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| {
        panic!("an inherited stale entry must be refused before judgment")
    });
    assert_eq!(code, exit::FAILED, "{said:#?}");
    assert!(judged.is_empty(), "{judged:#?}");
    let report = said.join("\n");
    assert!(
        report.contains("no exact authored branch addition"),
        "{report}"
    );
    assert!(report.contains("Stale entry"), "{report}");
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), branch);
    assert_eq!(origin_main(&world), remote);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
}

/// A reorder is an order edit, not a removal plus an addition. The tip's C,A,B order of shared
/// entries wins while main's new M remains anchored between A and B.
#[test]
fn a_reorder_only_edit_combines_with_a_main_addition_by_entry() {
    const A: &str = "- **A.** First in the base.\n\n";
    const B: &str = "- **B.** Second in the base.\n\n";
    const C: &str = "- **C.** Moved to the front by the branch.\n\n";
    const M: &str = "- **M.** Main inserted this between A and B.\n\n";
    let base = fixed_changelog(&[A, B, C]);
    let tip = fixed_changelog(&[C, A, B]);
    let main = fixed_changelog(&[A, M, B, C]);
    let expected = fixed_changelog(&[C, A, M, B]);

    let world = divergent_changelog(&base, &tip, &main);
    let (landed, said, _) = landed_changelog(&world);
    assert_eq!(landed, expected.trim_end(), "{said:#?}");
    for title in ["**A.**", "**B.**", "**C.**", "**M.**"] {
        assert_eq!(landed.matches(title).count(), 1, "{title}: {landed}");
    }
}

/// Make two independent merges X=(left,right) and Y=(right,left), leaving both left and right as
/// best merge bases. The strategy is deliberately ours: the test controls each endpoint's
/// changelog explicitly after the graph is made.
fn criss_cross(
    world: &World,
    left_file: (&str, &str),
    right_file: (&str, &str),
) -> (String, String) {
    let left = commit(&world.work, left_file.0, left_file.1);
    git(
        &world.work,
        &["push", "-q", "origin", "HEAD:refs/heads/criss-left"],
    );
    let right = commit(&world.rival, right_file.0, right_file.1);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.rival, &["fetch", "-q", "origin"]);
    git(
        &world.work,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-s",
            "ours",
            "-m",
            "X keeps the left tree",
            "origin/main",
        ],
    );
    git(
        &world.rival,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-s",
            "ours",
            "-m",
            "Y keeps the right tree",
            "origin/criss-left",
        ],
    );
    (left, right)
}

fn assert_two_bases(world: &World, left: &str, right: &str) {
    git(&world.work, &["fetch", "-q", "origin"]);
    let actual: BTreeSet<String> =
        git(&world.work, &["merge-base", "--all", "HEAD", "origin/main"])
            .split_whitespace()
            .map(str::to_owned)
            .collect();
    assert_eq!(
        actual,
        [left.to_owned(), right.to_owned()].into_iter().collect()
    );
}

/// Several best bases are evidence, not an invitation to choose one. Here the left base has
/// A,B while the right base has B,A. Both tips converge on A,B before adding their own entry, so
/// independently merging from either base yields the same bytes and the landing proceeds.
#[test]
fn multiple_merge_bases_land_when_every_entry_merge_converges() {
    const A: &str = "- **Base A.** Shared entry.\n\n";
    const B: &str = "- **Base B.** Shared entry.\n\n";
    const BRANCH: &str = "- **Branch addition.** From the checked tip.\n\n";
    const MAIN: &str = "- **Main addition.** From moving main.\n\n";
    let common = fixed_changelog(&[A, B]);
    let reordered = fixed_changelog(&[B, A]);
    let tip = fixed_changelog(&[BRANCH, A, B]);
    let main = fixed_changelog(&[MAIN, A, B]);

    let world = world();
    rival_moves(&world, "CHANGELOG.md", &common);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    let (left, right) = criss_cross(
        &world,
        ("a/x.txt", "left base\n"),
        ("CHANGELOG.md", &reordered),
    );
    commit(&world.work, "CHANGELOG.md", &tip);
    commit(&world.rival, "CHANGELOG.md", &main);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    assert_two_bases(&world, &left, &right);

    let (landed, said, _) = landed_changelog(&world);
    for title in [
        "**Branch addition.**",
        "**Main addition.**",
        "**Base A.**",
        "**Base B.**",
    ] {
        assert_eq!(
            landed.matches(title).count(),
            1,
            "{title}: {landed}\n{said:#?}"
        );
    }
    assert_eq!(landed.matches("### Fixed").count(), 2, "{landed}");
    assert!(
        landed.find("**Base A.**") < landed.find("**Base B.**"),
        "{landed}"
    );
}

/// If the independently computed results differ, no best base is privileged. The landing stops
/// before judgment, names both bases and their entries, and changes neither endpoint.
#[test]
fn multiple_merge_bases_with_different_results_are_a_named_prejudge_conflict() {
    const COMMON: &str = "- **Common base.** Before the criss-cross.\n\n";
    const LEFT: &str = "- **Left base.** Kept by X.\n\n";
    const RIGHT: &str = "- **Right base.** Kept by Y.\n\n";
    let common = fixed_changelog(&[COMMON]);
    let left_text = fixed_changelog(&[LEFT]);
    let right_text = fixed_changelog(&[RIGHT]);

    let world = world();
    rival_moves(&world, "CHANGELOG.md", &common);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    let (left, right) = criss_cross(
        &world,
        ("CHANGELOG.md", &left_text),
        ("CHANGELOG.md", &right_text),
    );
    git(&world.rival, &["push", "-q", "origin", "main"]);
    assert_two_bases(&world, &left, &right);
    let before = git(&world.work, &["rev-parse", "HEAD"]);
    let remote = origin_main(&world);

    let (code, said, judged) =
        land_with(&world, crates(&["a"]), Lane::Pass, |_, _| panic!("judged"));
    assert_eq!(code, exit::FAILED, "{said:#?}");
    assert!(judged.is_empty(), "{judged:#?}");
    let report = said.join("\n");
    assert!(
        report.contains(&left) && report.contains(&right),
        "{report}"
    );
    assert!(
        report.contains("Left base") && report.contains("Right base"),
        "{report}"
    );
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), before);
    assert_eq!(origin_main(&world), remote);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
}

/// The semantic result and Git's line merge are one transaction. If writing the merge commit is
/// refused, the tool aborts only the merge it just opened and restores the exact clean branch
/// tip; a retry never inherits MERGE_HEAD or a staged semantic changelog.
#[test]
fn a_refused_merge_commit_aborts_the_entry_transaction_cleanly() {
    const BASE: &str = "- **Base.** Before both changes.\n\n";
    const BRANCH: &str = "- **Branch.** Added on the branch.\n\n";
    const MAIN: &str = "- **Main.** Added on main.\n\n";
    let base = fixed_changelog(&[BASE]);
    let tip = fixed_changelog(&[BRANCH, BASE]);
    let main = fixed_changelog(&[MAIN, BASE]);
    let world = divergent_changelog(&base, &tip, &main);
    let before = git(&world.work, &["rev-parse", "HEAD"]);
    let remote = origin_main(&world);
    git(&world.work, &["config", "gpg.program", "false"]);
    let refusing = |dir: &Path| {
        let mut command = git_cmd(dir);
        command.args(["-c", "commit.gpgSign=true"]);
        command
    };
    let said = RefCell::new(Vec::new());
    let code = land(
        &world.work,
        &mut Seams {
            git: &refusing,
            judge: &mut |_: &Path, _: &str, _: Poll<'_>| panic!("a refused merge is never judged"),
            branch: &mut |_: &Path, _: &str| crates(&["a"]),
            lane: &mut |_: &Path, _: &str| Lane::Pass,
            recheck: &mut |_: &Path, _: &str| panic!("a refused merge is never rechecked"),
            say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
        },
    );
    let said = said.into_inner();
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    assert!(
        said.iter()
            .any(|line| line.contains("uncommitted merge was aborted")),
        "{said:#?}"
    );
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), before);
    assert_eq!(origin_main(&world), remote);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
    assert!(
        !world.work.join(".git/MERGE_HEAD").exists(),
        "MERGE_HEAD survived the aborted transaction"
    );
}

/// A fatal Git merge never stages the already-computed semantic result. In particular an index
/// lock leaves the branch bytes, HEAD and remote unchanged and no merge in progress: git 2.56
/// writes no merge state under it, and the state git 2.43 writes land forgets.
#[test]
fn a_fatal_git_merge_does_not_stage_the_precomputed_changelog() {
    const BASE: &str = "- **Base.** Before both changes.\n\n";
    const BRANCH: &str = "- **Branch.** Added on the branch.\n\n";
    const MAIN: &str = "- **Main.** Added on main.\n\n";
    let base = fixed_changelog(&[BASE]);
    let tip = fixed_changelog(&[BRANCH, BASE]);
    let main = fixed_changelog(&[MAIN, BASE]);
    let world = divergent_changelog(&base, &tip, &main);
    let before = git(&world.work, &["rev-parse", "HEAD"]);
    let remote = origin_main(&world);
    let original = std::fs::read(world.work.join("CHANGELOG.md")).unwrap();
    let lock = world.work.join(".git/index.lock");
    std::fs::write(&lock, "held by the regression fixture\n").unwrap();

    let (code, said, judged) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| {
        panic!("a fatal merge is never judged")
    });
    assert_ne!(code, exit::PASS, "{said:#?}");
    assert!(judged.is_empty(), "{judged:#?}");
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), before);
    assert_eq!(origin_main(&world), remote);
    assert_eq!(
        std::fs::read(world.work.join("CHANGELOG.md")).unwrap(),
        original
    );
    assert!(!world.work.join(".git/MERGE_HEAD").exists());
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        "held by the regression fixture\n"
    );
    std::fs::remove_file(lock).unwrap();
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
}

/// The merge state git 2.43 leaves under a held index lock, written by hand so every git meets
/// it: with the checkout unchanged land forgets it and the tip stands; with a staged change, or
/// with no merge state at all, it says which and forgets nothing.
#[test]
fn an_own_merge_the_index_lock_kept_from_aborting_is_forgotten_only_while_nothing_changed() {
    const STATE: [&str; 3] = ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE"];
    let world = divergent_changelog(
        &fixed_changelog(&[]),
        &fixed_changelog(&["- **Branch.** On the branch.\n\n"]),
        &fixed_changelog(&["- **Main.** On main.\n\n"]),
    );
    git(&world.work, &["fetch", "-q", "origin"]);
    let tip = git(&world.work, &["rev-parse", "HEAD"]);
    let main = origin_main(&world);
    let dot_git = world.work.join(".git");
    let lock = dot_git.join("index.lock");
    let reader = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let lock_merge_state = || {
        std::fs::write(dot_git.join("MERGE_HEAD"), format!("{main}\n")).unwrap();
        std::fs::write(dot_git.join("MERGE_MSG"), "Merge origin/main\n").unwrap();
        std::fs::write(dot_git.join("MERGE_MODE"), "no-ff").unwrap();
        std::fs::write(&lock, "held by the regression fixture\n").unwrap();
    };

    lock_merge_state();
    let said = reader.abort_own_merge(&tip, &main, "probe".into());
    assert!(
        said.contains("was forgotten and the branch tip restored"),
        "{said}"
    );
    for name in STATE {
        assert!(!dot_git.join(name).exists(), "{name} survived: {said}");
    }
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), tip);
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        "held by the regression fixture\n"
    );
    std::fs::remove_file(&lock).unwrap();

    let said = reader.abort_own_merge(&tip, &main, "probe".into());
    assert!(said.contains("git wrote no merge state"), "{said}");

    std::fs::write(world.work.join("staged.txt"), "staged\n").unwrap();
    git(&world.work, &["add", "staged.txt"]);
    lock_merge_state();
    let said = reader.abort_own_merge(&tip, &main, "probe".into());
    assert!(said.contains("could not be aborted either"), "{said}");
    assert!(dot_git.join("MERGE_HEAD").exists(), "{said}");
    std::fs::remove_file(&lock).unwrap();
}

#[test]
fn a_removed_entry_stays_removed_beside_older_malformed_side_history() {
    let world = world();
    const ORIGINAL: &str = "- **Main's fix.** Before the cut.\n\n";
    const ADDITION: &str = "- **Branch entry.** Kept across main's advance.\n\n";
    let initial = fixed_changelog(&[ORIGINAL, ADDITION]);
    rival_moves(&world, "CHANGELOG.md", &initial);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(
        &world.work,
        "CHANGELOG.md",
        &initial.replacen("### Fixed\n\n", "### Fixed\n\n### Fixed\n\n", 1),
    );
    commit(&world.work, "CHANGELOG.md", &initial);
    let base = rival_moves(&world, "b/y.txt", "main advanced\n");
    git(&world.work, &["fetch", "-q", "origin"]);
    git(
        &world.work,
        &["merge", "-q", "--no-ff", "--no-edit", "origin/main"],
    );
    let branch = git(&world.work, &["rev-parse", "HEAD"]);
    let main = rival_moves(&world, "CHANGELOG.md", &fixed_changelog(&[ADDITION]));
    git(&world.work, &["fetch", "-q", "origin"]);
    assert_eq!(git(&world.work, &["merge-base", &branch, &main]), base);
    let reader = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let planned = reader.changelog_merge(&branch, &main).unwrap().unwrap();
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), branch);
    assert_eq!(origin_main(&world), main);
    // A one-sided exact-file change needs no rewrite. Exercise the native merge
    // as well, rather than assuming every correct entry result is Some.
    git(&world.work, &["merge", "-q", "--no-ff", "--no-edit", &main]);
    let merged = planned
        .unwrap_or_else(|| std::fs::read_to_string(world.work.join("CHANGELOG.md")).unwrap());
    assert_eq!(
        merged.trim(),
        git(&world.work, &["show", &format!("{main}:CHANGELOG.md")]).trim()
    );
}

#[test]
fn changelog_authorship_keeps_a_new_branch_entry_written_before_merging_main() {
    const ORIGINAL: &str = "- **Main's fix.** Before the cut.\n\n";
    const ADDITION: &str = "- **Branch entry.** Authored before the main merge.\n\n";
    const MAIN_NEW: &str = "- Main's new note.\n\n";
    let world = world();
    rival_moves(&world, "CHANGELOG.md", &fixed_changelog(&[ORIGINAL]));
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(
        &world.work,
        "CHANGELOG.md",
        &fixed_changelog(&[ORIGINAL, ADDITION]),
    );
    let base = rival_moves(&world, "b/y.txt", "main advanced\n");
    git(&world.work, &["fetch", "-q", "origin"]);
    git(
        &world.work,
        &["merge", "-q", "--no-ff", "--no-edit", "origin/main"],
    );
    let branch = git(&world.work, &["rev-parse", "HEAD"]);
    let main = rival_moves(
        &world,
        "CHANGELOG.md",
        &fixed_changelog(&[ORIGINAL, MAIN_NEW]),
    );
    git(&world.work, &["fetch", "-q", "origin"]);
    assert_eq!(git(&world.work, &["merge-base", &branch, &main]), base);
    let reader = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let merged = reader
        .changelog_merge(&branch, &main)
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(merged.contains(ADDITION.trim_end()));
    assert!(merged.contains("- Main's new note."));
}
