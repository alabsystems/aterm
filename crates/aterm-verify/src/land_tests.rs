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
fn native_judgment_bootstrap_uses_its_selected_build_directory() {
    let env = crate::EnvSnapshot::capture();
    let workspace = aterm_tempfile::manifest_dir()
        .join("../..")
        .canonicalize()
        .unwrap();
    let tools = crate::preflight(&env, &workspace).unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let physical = scratch.path().canonicalize().unwrap();
    let root = physical.as_path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::create_dir(root.join(".cargo")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='land-placement-probe'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(root.join("build.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"{}\", env!(\"OUT_DIR\")); }\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".cargo/config.toml"),
        "[alias]\nverify=['run','--quiet','--']\n",
    )
    .unwrap();
    let selected = root.join(".ubd/bootstrap.noindex");
    let config = format!("build.build-dir='{}'", selected.display());
    let run = |placement| {
        let mut command = judgment_command(
            &tools.targo,
            root,
            "main",
            &root.join("events"),
            aterm_capacity::Admit::Wait,
            placement,
        );
        command
            .env_remove(aterm_toolchain::build_cache::BUILD_DIR_ENV)
            .env_remove("CARGO_TARGET_DIR");
        let out =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
    };
    let plain = run(None);
    assert!(
        plain.starts_with(root.join("target")),
        "negative control: {plain:?}"
    );
    let placed = run(Some(config.as_str()));
    assert!(
        placed.starts_with(&selected),
        "selected bootstrap was ignored: {placed:?}"
    );
    assert!(!placed.starts_with(root.join("target")));
}

#[path = "land_rejudge_tests.rs"]
mod rejudge;

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
        None,
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
/// it is merged and the branch's delta from the moved main judged again, and nothing is composed.
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

/// Main's changelog before a release cut, an entry the branch writes into it, and the cut.
const CHANGELOG: &str = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- **Main's fix.** Before \
                         the cut.\n\n## [0.1.0] - 2026-11-01\n\n### Fixed\n\n- **The first fix.** \
                         Shipped.\n";
const ENTRY: &str = "- **The branch's fix.** Written before the cut,\n  landed after it.\n\n";

fn with_entry(changelog: &str) -> String {
    changelog.replacen("### Fixed\n\n", &format!("### Fixed\n\n{ENTRY}"), 1)
}

fn cut(changelog: &str) -> String {
    changelog.replacen(
        "## [Unreleased]\n\n",
        "## [Unreleased]\n\n## [0.2.0] - 2026-11-08\n\n",
        1,
    )
}

/// A CHANGELOG ENTRY A RELEASE CUT STRANDED LANDS UNDER `[Unreleased]`: main cut 0.2.0 under the
/// entry the branch wrote into `[Unreleased]`, so git's merge puts the entry under 0.2.0, and the
/// landing moves it back to the top of `### Fixed` under `[Unreleased]`, leaving 0.2.0 as it
/// shipped. Each place a merge meets the cut moves it: the tool's first merge (amended, so the
/// landing is that merge restated), a re-judge's merge after main cut during the run, and a merge
/// made by hand before the run (a commit of its own: the hand merge is the person's).
/// NEGATIVE CONTROL (2026-11-08): with the first merge's `rehome_changelog` call removed, the
/// first-merge and hand-merge landings each hold the entry under `## [0.2.0]`; with the
/// re-judge's removed, the re-judge landing does.
#[test]
fn an_entry_a_release_cut_stranded_lands_under_unreleased() {
    let expected = cut(CHANGELOG).replacen(
        "## [Unreleased]\n\n",
        &format!("## [Unreleased]\n\n### Fixed\n\n{ENTRY}"),
        1,
    );
    for place in ["first merge", "re-judge merge", "hand merge"] {
        let world = world();
        rival_moves(&world, "CHANGELOG.md", CHANGELOG);
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
        let feature = commit(&world.work, "CHANGELOG.md", &with_entry(CHANGELOG));
        if place != "re-judge merge" {
            rival_moves(&world, "CHANGELOG.md", &cut(CHANGELOG));
        }
        if place == "hand merge" {
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
            assert!(
                std::fs::read_to_string(world.work.join("CHANGELOG.md"))
                    .expect("changelog")
                    .contains("## [0.2.0] - 2026-11-08\n\n### Fixed\n\n- **The branch's fix.**"),
                "{place}: git's merge strands the entry under the cut"
            );
        }
        let (code, said, _) = land_with(&world, crates(&["a"]), Lane::Pass, |_, n| {
            if place == "re-judge merge" && n == 0 {
                rival_moves(&world, "CHANGELOG.md", &cut(CHANGELOG));
            }
            green()
        });
        assert_eq!(code, exit::PASS, "{place}: {said:#?}");
        let landing = origin_main(&world);
        assert_eq!(
            git(&world.work, &["show", &format!("{landing}:CHANGELOG.md")]),
            expected.trim_end(),
            "{place}: {said:#?}"
        );
        assert!(
            said.iter().any(|line| line.starts_with(
                "verify land: moved 1 CHANGELOG entry this branch adds from under a release cut \
                 after it was written to [Unreleased]: \"The branch's fix.\" (merging"
            )),
            "{place}: {said:#?}"
        );
        let message = git(&world.work, &["log", "-1", "--format=%B", &landing]);
        let parents = git(&world.work, &["rev-list", "--parents", "-n", "1", &landing]);
        match place {
            "hand merge" => {
                let moved = git(&world.work, &["rev-parse", &format!("{landing}^1")]);
                assert_eq!(
                    git(&world.work, &["log", "-1", "--format=%s", &moved]),
                    "verify land: the CHANGELOG entries this branch adds sit under [Unreleased]"
                );
                assert_eq!(
                    git(
                        &world.work,
                        &["log", "-1", "--format=%s", &format!("{moved}^1")]
                    ),
                    "Merge main by hand"
                );
            }
            _ => {
                assert!(
                    message.starts_with(
                        "Merge origin/main into feature\n\nverify land moved 1 CHANGELOG entry"
                    ),
                    "{place}: the tool's merge carries the move: {message}"
                );
                assert!(
                    parents.starts_with(&format!("{landing} {feature} ")),
                    "{place}: {parents}"
                );
            }
        }
        assert_eq!(git(&world.work, &["status", "--porcelain"]), "", "{place}");
    }
}

/// THE LANDING MOVES ONLY WHAT A CUT STRANDED, over real git: a branch that reorders a release's
/// entries (`diff.interHunkContext` joining its hunks) lands them as it wrote them; under git's
/// union driver, a merge that brings back an entry's old words main rewrote at the cut moves the
/// branch's own entry and drops those old words; and a commit of the move
/// that fails leaves the checkout as the merge left it, committed and clean. CONTROL:
/// [`an_entry_a_release_cut_stranded_lands_under_unreleased`], where the entry moves.
/// NEGATIVE CONTROLS (2026-11-08): judging an entry main holds like any other lands the reorder
/// changed; without dropping words no commit of the branch wrote, main's old words land; and
/// without putting the file back, the refused commit leaves the checkout changed.
#[test]
fn a_landing_moves_only_what_a_release_cut_stranded() {
    let released = cut(CHANGELOG).replace(
        "- **Main's fix.** Before the cut.\n",
        "- **Main's fix.** Before the cut.\n\n- **Main's other fix.** Also shipped.\n",
    );
    let world = world();
    rival_moves(&world, "CHANGELOG.md", &released);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    git(&world.work, &["config", "diff.interHunkContext", "3"]);
    let reordered = released.replace(
        "- **Main's fix.** Before the cut.\n\n- **Main's other fix.** Also shipped.\n",
        "- **Main's other fix.** Also shipped.\n\n- **Main's fix.** Before the cut.\n",
    );
    commit(&world.work, "CHANGELOG.md", &reordered);
    let (code, said, _) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        git(
            &world.work,
            &["show", &format!("{}:CHANGELOG.md", origin_main(&world))]
        ),
        reordered.trim_end(),
        "a reorder lands as written: {said:#?}"
    );
    assert!(
        !said.iter().any(|l| l.contains("CHANGELOG entr")),
        "{said:#?}"
    );

    let union = self::world();
    let attributes = "CHANGELOG.md merge=union\n";
    rival_moves(&union, ".gitattributes", attributes);
    let old = "- **Main's fix.** Before the cut.";
    rival_moves(&union, "CHANGELOG.md", CHANGELOG);
    git(&union.work, &["fetch", "-q", "origin"]);
    git(&union.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&union.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    rival_moves(
        &union,
        "CHANGELOG.md",
        &cut(CHANGELOG).replace(old, "- **Main's fix.** Its words, rewritten at the cut."),
    );
    let (code, said, _) = land_with(&union, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let landed = git(
        &union.work,
        &["show", &format!("{}:CHANGELOG.md", origin_main(&union))],
    );
    let unreleased = landed
        .split("## [0.2.0]")
        .next()
        .expect("the head")
        .to_owned();
    assert!(
        unreleased.contains("- **The branch's fix.**") && !landed.contains(old),
        "the branch's entry moves and main's old words, brought back, go:\n{landed}\n{said:#?}"
    );
    assert!(
        said.iter()
            .any(|l| l.contains("main no longer holds, which a merge brought back")),
        "{said:#?}"
    );

    let refused = self::world();
    rival_moves(&refused, "CHANGELOG.md", CHANGELOG);
    git(&refused.work, &["fetch", "-q", "origin"]);
    git(&refused.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(&refused.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    rival_moves(&refused, "CHANGELOG.md", &cut(CHANGELOG));
    // The merge commits; the amend that would carry the move is refused by a failing signer.
    git(&refused.work, &["config", "gpg.program", "false"]);
    // Each git command once the branch holds a merge signs its commits.
    let refusing = |dir: &Path| {
        let mut cmd = git_cmd(dir);
        if !git(dir, &["rev-list", "--merges", "-n", "1", "HEAD"]).is_empty() {
            cmd.args(["-c", "commit.gpgSign=true"]);
        }
        cmd
    };
    let said = RefCell::new(Vec::new());
    let code = land(
        &refused.work,
        &mut Seams {
            git: &refusing,
            judge: &mut |_: &Path, _: &str| green(),
            branch: &mut |_: &Path, _: &str| crates(&["a"]),
            lane: &mut |_: &Path, _: &str| Lane::Pass,
            say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
        },
    );
    let said = said.into_inner();
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    assert!(
        said.iter()
            .any(|l| l.contains("CHANGELOG.md is as the merge left it")),
        "{said:#?}"
    );
    assert_eq!(
        git(&refused.work, &["status", "--porcelain"]),
        "",
        "{said:#?}"
    );
    assert_eq!(
        git(&refused.work, &["show", "HEAD:CHANGELOG.md"]),
        std::fs::read_to_string(refused.work.join("CHANGELOG.md"))
            .expect("changelog")
            .trim_end()
    );
}

/// WHERE AN ENTRY CAME FROM IS THE BRANCH'S OWN COMMITS, NOT ONE MERGE BASE, over real git and a
/// whole landing: a branch whose last merge is a stacked branch's (that merge's base already holds
/// the branch's entry) lands its entry; a change landed, reverted on main and written again lands
/// its entry again; an entry the branch restores under its release stays under it, as does one it
/// wrote under `[Unreleased]` and then moved under a release cut before, and one under a release
/// the branch cut itself; an entry a squash of main stranded (a commit of one parent replaying
/// the cut) moves, as does one the branch removed after a hand merge stranded it and then brought
/// back by a revert, one it wrote under a release by mistake and then moved up, and one written
/// beside a release main pulled and cut again under its number (a re-spin); and main's old words
/// that a hand merge's union kept beside its rewrite go once a cut strands them with the branch's
/// own entry, which moves. CONTROL:
/// [`an_entry_a_release_cut_stranded_lands_under_unreleased`], where the branch's entry moves.
/// NEGATIVE CONTROLS (2026-11-08): with no commit's entries read, the stacked branch's entry is
/// dropped; with every written entry read as written under `[Unreleased]`, the restored one
/// moves; with the releases beside a write not read, the backfilled one moves back; with
/// releases main does not name counted, the own release's entry moves; with a later write
/// replacing an earlier one, the reverted entry stays under the release; with a move up not read
/// as a write, the moved-up entry stays; with a release read by its version alone, the re-spun
/// one stays; without dropping words no commit of the branch wrote, main's old words land.
#[test]
fn an_entrys_origin_is_the_branchs_own_commits() {
    let landed = |world: &World| {
        let (code, said, _) = land_with(world, crates(&["a"]), Lane::Pass, |_, _| green());
        assert_eq!(code, exit::PASS, "{said:#?}");
        (
            git(
                &world.work,
                &["show", &format!("{}:CHANGELOG.md", origin_main(world))],
            ),
            said,
        )
    };
    let start = |world: &World, changelog: &str| {
        rival_moves(world, "CHANGELOG.md", changelog);
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    };

    let stacked = world();
    start(&stacked, CHANGELOG);
    commit(&stacked.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    git(&stacked.work, &["checkout", "-q", "-b", "stack"]);
    commit(&stacked.work, "a/x.txt", "a2\n");
    git(&stacked.work, &["checkout", "-q", "feature"]);
    rival_moves(&stacked, "b/y.txt", "b2\n");
    git(&stacked.work, &["fetch", "-q", "origin"]);
    git(
        &stacked.work,
        &["merge", "-q", "--no-ff", "-m", "main", "origin/main"],
    );
    git(
        &stacked.work,
        &["merge", "-q", "--no-ff", "-m", "stack", "stack"],
    );
    let (changelog, said) = landed(&stacked);
    assert_eq!(
        changelog,
        with_entry(CHANGELOG).trim_end(),
        "stacked: {said:#?}"
    );

    let again = world();
    start(&again, CHANGELOG);
    commit(&again.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    landed(&again);
    rival_moves(&again, "CHANGELOG.md", CHANGELOG);
    git(&again.work, &["fetch", "-q", "origin"]);
    git(
        &again.work,
        &["merge", "-q", "--no-ff", "-m", "the revert", "origin/main"],
    );
    commit(&again.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    let (changelog, said) = landed(&again);
    assert_eq!(
        changelog,
        with_entry(CHANGELOG).trim_end(),
        "again: {said:#?}"
    );

    let restored = world();
    let shipped = cut(CHANGELOG);
    let first = "- **The first fix.** Shipped.\n";
    start(&restored, &shipped.replace(first, ""));
    commit(&restored.work, "CHANGELOG.md", &shipped);
    let (changelog, said) = landed(&restored);
    assert_eq!(changelog, shipped.trim_end(), "restored: {said:#?}");

    let backfilled = world();
    start(&backfilled, CHANGELOG);
    commit(&backfilled.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    let first = "- **The first fix.**";
    let shipped_in = CHANGELOG.replacen(first, &format!("{ENTRY}{first}"), 1);
    commit(&backfilled.work, "CHANGELOG.md", &shipped_in);
    rival_moves(&backfilled, "CHANGELOG.md", &cut(CHANGELOG));
    let (changelog, said) = landed(&backfilled);
    assert_eq!(
        changelog,
        cut(&shipped_in).trim_end(),
        "backfilled: {said:#?}"
    );

    let rehomed = cut(CHANGELOG).replacen(
        "## [Unreleased]\n\n",
        &format!("## [Unreleased]\n\n### Fixed\n\n{ENTRY}"),
        1,
    );
    let squashed = world();
    start(&squashed, CHANGELOG);
    commit(&squashed.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    rival_moves(&squashed, "CHANGELOG.md", &cut(CHANGELOG));
    git(&squashed.work, &["fetch", "-q", "origin"]);
    git(&squashed.work, &["merge", "-q", "--squash", "origin/main"]);
    git(&squashed.work, &["commit", "-q", "-m", "main, squashed"]);
    assert_eq!(
        git(&squashed.work, &["show", "HEAD:CHANGELOG.md"]),
        cut(&with_entry(CHANGELOG)).trim_end(),
        "the squash replays the cut over the entry"
    );
    let (changelog, said) = landed(&squashed);
    assert_eq!(changelog, rehomed.trim_end(), "squashed: {said:#?}");

    let moved_up = world();
    start(&moved_up, CHANGELOG);
    commit(&moved_up.work, "CHANGELOG.md", &shipped_in);
    commit(&moved_up.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    rival_moves(&moved_up, "CHANGELOG.md", &cut(CHANGELOG));
    let (changelog, said) = landed(&moved_up);
    assert_eq!(changelog, rehomed.trim_end(), "moved up: {said:#?}");

    let respun = world();
    let next = "## [Unreleased]\n\n### Fixed\n\n- **Main's next.** After 0.2.0.\n\n";
    let cut_then_next = cut(CHANGELOG).replacen("## [Unreleased]\n\n", next, 1);
    start(&respun, &cut_then_next);
    commit(&respun.work, "CHANGELOG.md", &with_entry(&cut_then_next));
    let pulled = cut_then_next.replacen("## [0.2.0] - 2026-11-08\n\n", "", 1);
    rival_moves(&respun, "CHANGELOG.md", &pulled);
    let recut = pulled.replacen(
        "## [Unreleased]\n\n",
        "## [Unreleased]\n\n## [0.2.0] - 2026-11-10\n\n",
        1,
    );
    rival_moves(&respun, "CHANGELOG.md", &recut);
    let (changelog, said) = landed(&respun);
    assert_eq!(
        changelog,
        recut
            .replacen(
                "## [Unreleased]\n\n",
                &format!("## [Unreleased]\n\n### Fixed\n\n{ENTRY}"),
                1
            )
            .trim_end(),
        "respun: {said:#?}"
    );

    let reverted = world();
    start(&reverted, CHANGELOG);
    commit(&reverted.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    rival_moves(&reverted, "CHANGELOG.md", &cut(CHANGELOG));
    git(&reverted.work, &["fetch", "-q", "origin"]);
    git(
        &reverted.work,
        &["merge", "-q", "--no-ff", "-m", "by hand", "origin/main"],
    );
    commit(&reverted.work, "CHANGELOG.md", &cut(CHANGELOG));
    git(&reverted.work, &["revert", "--no-edit", "HEAD"]);
    let (changelog, said) = landed(&reverted);
    assert_eq!(changelog, rehomed.trim_end(), "reverted: {said:#?}");

    let own_release = world();
    start(&own_release, CHANGELOG);
    commit(&own_release.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    let released = with_entry(CHANGELOG).replacen(
        "## [Unreleased]\n\n",
        "## [Unreleased]\n\n## [0.3.0] - 2026-11-09\n\n",
        1,
    );
    commit(&own_release.work, "CHANGELOG.md", &released);
    let (changelog, said) = landed(&own_release);
    assert_eq!(changelog, released.trim_end(), "own release: {said:#?}");

    let revived = world();
    rival_moves(&revived, ".gitattributes", "CHANGELOG.md merge=union\n");
    start(&revived, CHANGELOG);
    commit(&revived.work, "CHANGELOG.md", &with_entry(CHANGELOG));
    let old = "- **Main's fix.** Before the cut.";
    let rewritten = CHANGELOG.replace(old, "- **Main's fix.** Its words, rewritten.");
    rival_moves(&revived, "CHANGELOG.md", &rewritten);
    git(&revived.work, &["fetch", "-q", "origin"]);
    git(
        &revived.work,
        &["merge", "-q", "--no-ff", "-m", "by hand", "origin/main"],
    );
    assert!(
        std::fs::read_to_string(revived.work.join("CHANGELOG.md"))
            .expect("changelog")
            .contains(old),
        "the union merge keeps main's old words beside its rewrite"
    );
    rival_moves(&revived, "CHANGELOG.md", &cut(&rewritten));
    let (changelog, said) = landed(&revived);
    assert!(
        !changelog.contains(old)
            && changelog
                .split("## [0.2.0]")
                .next()
                .is_some_and(|unreleased| unreleased.contains("- **The branch's fix.**")),
        "revived:\n{changelog}\n{said:#?}"
    );
}
