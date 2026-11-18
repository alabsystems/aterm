// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};

use super::*;

const R: &str = "cccccccccccccccccccccccccccccccccccccccc";
const HOP: &str = "1111111111111111111111111111111111111111";
const OTHER: &str = "2222222222222222222222222222222222222222";

/// A pump's spawner: it records each spawn's folder and task, and answers them in turn.
struct Spawns {
    answers: Vec<Result<String, String>>,
    spawned: Vec<(PathBuf, String)>,
}

impl Spawns {
    fn answering(answers: Vec<Result<String, String>>) -> Self {
        Self {
            answers,
            spawned: Vec::new(),
        }
    }

    fn opening(sid: &str) -> Self {
        Self::answering(vec![Ok(sid.into())])
    }
}

impl Spawner for Spawns {
    fn spawn(&mut self, cwd: &Path, task: &str) -> Result<String, String> {
        self.spawned.push((cwd.to_path_buf(), task.to_string()));
        if self.answers.is_empty() {
            return Err("this stand-in has no answer left".into());
        }
        self.answers.remove(0)
    }
}

fn failure(commit: &str) -> Failure {
    Failure {
        gate: "verify release: test".into(),
        step: "4/10 qualify".into(),
        log: PathBuf::from("/c/clone/.git/aterm-release/logs/run-1/04-qualify.log"),
        why: "R's tree 1234567 FAILED test under this seal (log /c/clone/.git/aterm-release/.ubd/\
              verified/t/s/test.log) — a FAIL is never re-rolled"
            .into(),
        detail: vec!["test aterm-gui render::tests::glyph_cache failed".into()],
        remedy: "land the fix on main, then targo --unverified ship release --fix <the fix's own \
                 non-merge commit>"
            .into(),
        version: "0.6.0".into(),
        commit: commit.into(),
        prover: "/c/store/trust/bin".into(),
        trustc: "41d799797a1b2c3d4e5f60718293a4b5c6d7e8f9".into(),
        at: 1_791_600_000,
        opening: None,
    }
}

#[test]
fn pump_cuts_spawn_one_repair_worker_per_input_hash() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let record = scratch.path().join("aterm-release").join(RECORD_FILE);
    let clone = scratch.path().join("clone");
    let red = failure(R);
    let hash = input_hash(&red);

    // A pump's failing cut spawns exactly one worker, in the release clone, told the
    // failing log and the cut's inputs.
    let mut first = Spawns::opening("s-0123456789abcdef0123");
    assert_eq!(
        open(&record, &clone, &red, RepairMode::Pump(&mut first)),
        Opened::Spawned {
            hash: hash.clone(),
            sid: "s-0123456789abcdef0123".into()
        }
    );
    assert_eq!(first.spawned.len(), 1, "one worker");
    let (cwd, told) = &first.spawned[0];
    assert_eq!(cwd, &clone, "the worker starts in the release clone");
    let log = red.log.display().to_string();
    let clone_words = clone.display().to_string();
    for word in [
        log.as_str(),
        "verify release: test",
        "4/10 qualify",
        "0.6.0",
        R,
        red.trustc.as_str(),
        "/c/clone/.git/aterm-release/logs/run-1",
        "glyph_cache failed",
        clone_words.as_str(),
        hash.as_str(),
        "`targo --unverified verify land`",
        "--fix <the fix's own non-merge commit>",
    ] {
        assert!(told.contains(word), "the task names {word:?}:\n{told}");
    }

    // The sid lands in the cut's repair record on disk.
    let kept = Record::load(&record).expect("the record reads back");
    assert_eq!(kept.workers.len(), 1, "{kept:?}");
    let worker = &kept.workers[0];
    assert_eq!(
        (
            worker.hash.as_str(),
            worker.sid.as_str(),
            worker.commit.as_str()
        ),
        (hash.as_str(), "s-0123456789abcdef0123", R)
    );
    assert_eq!(worker.log, red.log);
    assert!(
        std::fs::read_to_string(&record)
            .expect("the record file")
            .contains("s-0123456789abcdef0123"),
        "kept on disk, for the next run"
    );

    // A later cut that fails again on the same inputs (even at another gate) shares only the
    // record with this one: it spawns none and names the worker already on them.
    let mut again = Spawns::opening("s-ffffffffffffffffffff");
    let failed_again = Failure {
        gate: "qualify's build branch".into(),
        at: red.at + 600,
        ..red.clone()
    };
    assert_eq!(
        open(&record, &clone, &failed_again, RepairMode::Pump(&mut again)),
        Opened::Already {
            hash: hash.clone(),
            sid: "s-0123456789abcdef0123".into()
        }
    );
    assert!(again.spawned.is_empty(), "{:?}", again.spawned);
    assert_eq!(Record::load(&record).expect("record"), kept, "unchanged");

    // A different input hash (a new hop) spawns anew.
    let hop = failure(HOP);
    let mut next = Spawns::opening("s-1111111111111111aaaa");
    assert_eq!(
        open(&record, &clone, &hop, RepairMode::Pump(&mut next)),
        Opened::Spawned {
            hash: input_hash(&hop),
            sid: "s-1111111111111111aaaa".into()
        }
    );
    assert_eq!(next.spawned.len(), 1);
    assert_eq!(Record::load(&record).expect("record").workers.len(), 2);
}

#[test]
fn caller_cuts_return_repair_context_without_starting_or_recording_a_worker() {
    for in_session in [false, true] {
        assert!(spawner(in_session, false).is_none());
        for original in [None, Some("not a worker record\n")] {
            let scratch = aterm_tempfile::tempdir().expect("scratch");
            let record = scratch.path().join(RECORD_FILE);
            if let Some(text) = original {
                std::fs::write(&record, text).expect("the caller's unreadable record");
            }
            let red = failure(OTHER);
            let opened = open(&record, scratch.path(), &red, RepairMode::Caller);
            let Opened::Caller { hash, briefing } = &opened else {
                panic!("the caller gets the failure context: {opened:?}");
            };
            assert_eq!(*hash, input_hash(&red));
            for expected in [
                red.gate.as_str(),
                red.step.as_str(),
                red.version.as_str(),
                red.commit.as_str(),
                red.trustc.as_str(),
                red.log.to_str().expect("fixture log"),
                red.detail[0].as_str(),
            ] {
                assert!(briefing.contains(expected), "missing {expected:?}");
            }
            assert!(!briefing.contains("`FIX <") && !briefing.contains("`NO-FIX <"));
            assert!(!briefing.contains("aterm ctl @self spawn"));
            let said = opened.lines(&record);
            assert_eq!(said[1..].join("\n"), briefing.trim_end_matches('\n'));
            match original {
                Some(text) => assert_eq!(std::fs::read_to_string(&record).unwrap(), text),
                None => assert!(!record.exists(), "no worker record was created"),
            }
        }
    }
}

#[test]
fn a_spawn_that_fails_is_said_and_kept_nowhere() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let record = scratch.path().join(RECORD_FILE);
    let red = failure(R);
    let mut refused = Spawns::answering(vec![Err("ERR spawn agent=claude: no such window".into())]);
    let opened = open(
        &record,
        scratch.path(),
        &red,
        RepairMode::Pump(&mut refused),
    );
    assert_eq!(
        opened,
        Opened::Failed {
            hash: input_hash(&red),
            why: "ERR spawn agent=claude: no such window".into()
        }
    );
    assert!(opened.lines(&record)[0].starts_with("repair: no worker opened"));
    assert!(!record.exists(), "a failed spawn records no worker");

    let mut working = Spawns::opening("s-2222222222222222bbbb");
    assert!(matches!(
        open(
            &record,
            scratch.path(),
            &red,
            RepairMode::Pump(&mut working)
        ),
        Opened::Spawned { .. }
    ));
    assert_eq!(working.spawned.len(), 1, "the next failing cut asks again");
}

#[test]
fn an_unreadable_record_starts_no_worker() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let record = scratch.path().join(RECORD_FILE);
    std::fs::write(&record, "[[worker]\nnot a record").expect("write");
    let mut spawns = Spawns::opening("s-3333333333333333cccc");
    let opened = open(
        &record,
        scratch.path(),
        &failure(R),
        RepairMode::Pump(&mut spawns),
    );
    assert!(
        matches!(&opened, Opened::Failed { why, .. } if why.contains("is not a repair record")),
        "{opened:?}"
    );
    assert!(
        spawns.spawned.is_empty(),
        "it cannot tell whether one was opened"
    );
    assert_eq!(
        std::fs::read_to_string(&record).expect("read"),
        "[[worker]\nnot a record"
    );
}

#[test]
fn the_input_hash_is_the_cuts_inputs_alone() {
    let red = failure(R);
    let hash = input_hash(&red);
    assert_eq!(hash.len(), 64);
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
    let same = Failure {
        gate: "the preflight's commit-only gates".into(),
        step: "1/10 checks".into(),
        log: PathBuf::from("/elsewhere.log"),
        why: "another why".into(),
        detail: Vec::new(),
        remedy: String::new(),
        prover: "/another/bin".into(),
        at: 0,
        ..red.clone()
    };
    assert_eq!(input_hash(&same), hash, "what failed is no input");
    for other in [
        Failure {
            version: "0.7.0".into(),
            ..red.clone()
        },
        Failure {
            commit: HOP.into(),
            ..red.clone()
        },
        Failure {
            trustc: "0".repeat(40),
            ..red.clone()
        },
    ] {
        assert_ne!(input_hash(&other), hash, "{other:?}");
    }
}

#[test]
fn the_task_fits_one_spawn_and_sanitizes_its_payload() {
    let clone = Path::new("/c/a clone's/path");
    let huge = Failure {
        why: format!("{}\n{}", "x".repeat(200_000), "line\n".repeat(5_000)),
        detail: vec!["nul\0here\rdetail".to_owned(); 2_000],
        remedy: "remedy\n".repeat(50),
        ..failure(R)
    };
    let told = task(clone, &huge, &input_hash(&huge));
    assert!(told.len() <= MAX_TASK_BYTES, "{} bytes", told.len());
    assert!(!told.contains('\0') && !told.contains('\r'));
    assert!(told.ends_with('\n'));
    assert!(told.contains("more line(s), in the log"));
    assert!(told.contains(clone.to_str().unwrap()));
    assert_eq!(spawn_args(clone)[3], format!("cwd={}", clone.display()));
    assert_eq!(
        sid_of("OK s-0123456789abcdef0123\n"),
        Some("s-0123456789abcdef0123".into())
    );
    assert_eq!(sid_of("OK s-1 more words"), Some("s-1".into()));
    assert_eq!(sid_of("ERR spawn agent=claude: unsupervised"), None);
    assert_eq!(sid_of("OK"), None);
}

/// The refusal's own words take only the bytes the rest leaves: excerpts of 3- and 4-byte text
/// that would fill the cap many times over leave what failed, the log, the inputs and every step
/// of what to do whole, and a field that holds a newline stays one line after its label.
#[test]
fn multibyte_excerpts_leave_the_inputs_and_what_to_do_whole() {
    let clone = Path::new("/c/clone");
    let huge = Failure {
        why: format!("{}\n", "─".repeat(300)).repeat(400),
        detail: vec!["𝄞".repeat(300); 2_000],
        remedy: format!("{}\n", "—".repeat(600)).repeat(50),
        log: PathBuf::from("/c/clone/.git/aterm-release/logs/run\nsecond-line\r\0/04-qualify.log"),
        step: "4/10 qualify\nsecond-line".into(),
        ..failure(R)
    };
    let hash = input_hash(&huge);
    let told = task(clone, &huge, &hash);
    assert!(told.len() <= MAX_TASK_BYTES, "{} bytes", told.len());
    assert!(!told.contains('\0') && !told.contains('\r'));
    for word in [
        "What failed: verify release: test, at step 4/10 qualify second-line of",
        "The step's log, which ends with that detail: /c/clone/.git/aterm-release/logs/run \
         second-line  /04-qualify.log",
        "- version: 0.6.0",
        R,
        huge.trustc.as_str(),
        hash.as_str(),
        "- release clone: /c/clone",
        "2. Leave the release clone as it is",
        "`targo --unverified verify land`",
        "4. Do not run `ship release`, publish, or read the release credentials.",
        "--fix <the fix's own non-merge commit>",
        "5. If the cause is not in the source",
    ] {
        assert!(told.contains(word), "the task names {word:?}:\n{told}");
    }
    for (title, row) in [("Why:", "─"), ("Detail:", "𝄞"), ("The cut's remedy:", "—")] {
        let at = told.find(title).expect(title);
        assert!(told[at..].contains(row), "{title} keeps some of its text");
    }
    assert_eq!(
        told.matches("more line(s), in the log").count(),
        3,
        "each excerpt says what it left out:\n{told}"
    );
}

/// A worker that started is said even when the record cannot keep it: the next failing cut on
/// these inputs then asks again.
///
/// Unix's: the save fails in a directory its mode bits leave read-only, which Windows has no
/// counterpart of (a directory's read-only attribute does not refuse a new file in it).
#[cfg(unix)]
#[test]
fn a_started_worker_the_record_cannot_keep_is_said_unrecorded() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let dir = scratch.path().join("aterm-release");
    std::fs::create_dir(&dir).expect("dir");
    let record = dir.join(RECORD_FILE);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("read-only");
    let red = failure(R);
    let mut spawns = Spawns::opening("s-4444444444444444dddd");
    let opened = open(&record, scratch.path(), &red, RepairMode::Pump(&mut spawns));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("writable");
    let Opened::Unrecorded { hash, sid, why } = &opened else {
        panic!("a save that fails after the spawn is Unrecorded: {opened:?}");
    };
    assert_eq!(
        (hash.as_str(), sid.as_str()),
        (input_hash(&red).as_str(), "s-4444444444444444dddd")
    );
    assert!(why.contains(&record.display().to_string()), "{why}");
    assert_eq!(spawns.spawned.len(), 1);
    assert!(!record.exists());
    assert!(opened.lines(&record)[0].contains("could not keep it"));
}

/// The record keeps the newest 64 workers: a 65th drops the oldest.
#[test]
fn the_record_keeps_the_newest_sixty_four_workers() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let record = scratch.path().join(RECORD_FILE);
    let red = failure(R);
    let row = |n: usize| Worker {
        hash: format!("{n:064x}"),
        sid: format!("s-{n:020}"),
        gate: red.gate.clone(),
        step: red.step.clone(),
        log: red.log.clone(),
        version: red.version.clone(),
        commit: red.commit.clone(),
        trustc: red.trustc.clone(),
        at: red.at,
    };
    let full = Record {
        workers: (0..MAX_WORKERS).map(row).collect(),
    };
    full.save(&record).expect("save");
    let mut spawns = Spawns::opening("s-5555555555555555eeee");
    assert!(matches!(
        open(&record, scratch.path(), &red, RepairMode::Pump(&mut spawns)),
        Opened::Spawned { .. }
    ));
    let kept = Record::load(&record).expect("record");
    assert_eq!(kept.workers.len(), MAX_WORKERS);
    assert_eq!(kept.workers[0], row(1), "the oldest went");
    assert_eq!(
        kept.workers
            .last()
            .map(|w| (w.hash.as_str(), w.sid.as_str())),
        Some((input_hash(&red).as_str(), "s-5555555555555555eeee"))
    );
}

/// THE REPORT'S LAST VERDICT LINE IS WHAT THE PUMP READS. `FIX` names one full commit, `NO-FIX`
/// a reason; a report with neither says nothing, and prose that mentions a fix is no verdict.
#[test]
fn a_reports_last_verdict_line_names_the_fix_or_the_reason() {
    let rows: [(&str, Option<Result<&str, &str>>); 9] = [
        (&format!("Landed it.\nFIX {HOP}\n"), Some(Ok(HOP))),
        (
            &format!("FIX {OTHER}\nthen redone\n`FIX {HOP}`."),
            Some(Ok(HOP)),
        ),
        (&format!("- **FIX: {HOP}**"), Some(Ok(HOP))),
        (&format!("FIX {}", HOP.to_ascii_uppercase()), Some(Ok(HOP))),
        (
            "NO-FIX the notary service is down",
            Some(Err("the notary service is down")),
        ),
        ("NO-FIX", Some(Err("no reason given"))),
        ("FIX 1111111", None),
        ("The FIX is on main: see the log", None),
        ("", None),
    ];
    for (report, expected) in rows {
        let expected = expected.map(|v| v.map(str::to_string).map_err(str::to_string));
        assert_eq!(verdict(report), expected, "{report:?}");
    }
}

/// A WORKER IS WORKING UNTIL ITS TASK ENDS, and an ended worker with no verdict line fixed
/// nothing. What cannot be read is unread: never a fix and never an end.
#[test]
fn a_workers_report_is_read_from_its_task_state_then_its_verdict() {
    use crate::pump::Report;
    let unread = |_| -> Result<String, String> { panic!("a working task's result is not read") };
    assert_eq!(
        report_from(Ok("OK state=working rev=3 from=s-1".into()), unread),
        Report::Working
    );
    assert_eq!(
        report_from(Ok("OK state=done rev=9".into()), |_| Ok(format!(
            "done\nFIX {HOP}"
        ))),
        Report::Fix(HOP.into())
    );
    assert_eq!(
        report_from(Ok("OK state=exited rev=9".into()), |_| Ok(
            "NO-FIX a credential".into()
        )),
        Report::NoFix("a credential".into())
    );
    assert!(matches!(
        report_from(Ok("OK state=stalled rev=2".into()), |_| Ok("ran out".into())),
        Report::NoFix(why) if why.contains("stalled") && why.contains("none of its replies")
    ));
    assert_eq!(
        report_from(Err("no aterm answered".into()), unread),
        Report::Unread("no aterm answered".into())
    );
    assert!(matches!(
        report_from(Ok("OK rev=1".into()), unread),
        Report::Unread(why) if why.contains("names no state")
    ));
    assert_eq!(
        report_from(Ok("OK state=done".into()), |_| Err(
            "result unavailable".into()
        )),
        Report::Unread("result unavailable".into())
    );

    // A worker that failed, lost its task or lost its session has ended: it holds no red line
    // as a worker still on it, and it is not unread for good.
    // NEGATIVE CONTROL: read as Working (or Unread), the pump waits on it without end.
    assert_eq!(
        report_from(Ok("OK state=failed rev=4".into()), |_| Ok(format!(
            "it broke\nFIX {HOP}"
        ))),
        Report::Fix(HOP.into()),
        "a failed task that still named its fix"
    );
    assert!(matches!(
        report_from(Ok("OK state=failed rev=4".into()), |_| Ok("it broke".into())),
        Report::NoFix(why) if why.contains("failed")
    ));
    assert_eq!(
        report_from(Ok("OK state=- rev=0".into()), unread),
        Report::NoFix("its session runs no task".into())
    );
    let gone = "aterm ctl @s-1 task exited exit status: 1: aterm-ctl: ERR no such session";
    assert!(matches!(
        report_from(Err(gone.into()), unread),
        Report::NoFix(why) if why.contains("its session is gone")
    ));
    assert!(matches!(
        report_from(Ok("OK state=done".into()), |_| Err(gone.into())),
        Report::NoFix(why) if why.contains("its session is gone")
    ));
    // A state this does not know is still working: only a named end is one.
    assert_eq!(
        report_from(Ok("OK state=paused".into()), unread),
        Report::Working
    );

    // R43: under Autopilot Land the last reply answers the land check; the fix was named in
    // the reply before it. The newest verdict line across the whole exchange is the answer.
    let land = |full: bool| -> Result<String, String> {
        Ok(if full {
            format!(
                "What to do: ... end your report with one line: `NO-FIX <what it is>`.\n\
                 Fixed the cause.\nFIX {HOP}\n[aterm harness] Land check: ... reply with only \
                 LANDED\nLANDED\n"
            )
        } else {
            "LANDED\n".into()
        })
    };
    assert_eq!(
        report_from(Ok("OK state=done rev=7".into()), land),
        Report::Fix(HOP.into()),
        "a fix named before the land check's LANDED"
    );
    // The newest line wins: a NO-FIX after an earlier FIX is the worker's last word.
    assert_eq!(
        report_from(Ok("OK state=done rev=7".into()), |full: bool| Ok(if full {
            format!("FIX {HOP}\nthe fix did not hold\nNO-FIX this machine\nLANDED\n")
        } else {
            "LANDED".into()
        })),
        Report::NoFix("this machine".into())
    );
    // The last reply's own verdict is taken without reading the exchange.
    assert_eq!(
        report_from(Ok("OK state=done rev=7".into()), |full: bool| {
            assert!(!full, "a verdict in the last reply reads no more");
            Ok(format!("FIX {HOP}"))
        }),
        Report::Fix(HOP.into())
    );
    // NEGATIVE CONTROL: an exchange with neither line (the task prompt's wording alone) is
    // still no fix, and so is one that cannot be read.
    assert!(matches!(
        report_from(Ok("OK state=done rev=7".into()), |full: bool| Ok(if full {
            "end your report with one line: `NO-FIX <what it is>`.\nworked on it\nLANDED\n".into()
        } else {
            "LANDED".into()
        })),
        Report::NoFix(why) if why.contains("none of its replies")
    ));
    assert!(matches!(
        report_from(Ok("OK state=done rev=7".into()), |full: bool| if full {
            Err("ERR result unavailable why=no-transcript".into())
        } else {
            Ok("LANDED".into())
        }),
        Report::NoFix(why) if why.contains("could not be read") && why.contains("no-transcript")
    ));
}

/// ONE RED OF `--open-next` IS ONE FAILURE AT EVERY TIP. Each try judges whatever commit main's
/// tip then is, so its hash is of the version it opens after alone: a second red at another tip
/// names the worker already on it and opens none. A cut's red of the same version and commit
/// is another failure, and its worker is of a cut where the opening's is not.
/// NEGATIVE CONTROL: hashed as a cut's inputs, each tip is a new hash and a new worker.
#[test]
fn a_red_opening_has_one_worker_at_every_tip() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let record = scratch.path().join("aterm-release").join(RECORD_FILE);
    let clone = scratch.path().join("clone");
    let opening = |tip: &str| Failure {
        opening: Some("0.5.0".into()),
        ..failure(tip)
    };
    assert_eq!(input_hash(&opening(R)), opening_hash("0.5.0"));
    assert_eq!(input_hash(&opening(R)), input_hash(&opening(HOP)));
    assert_ne!(opening_hash("0.5.0"), opening_hash("0.6.0"));
    assert_ne!(input_hash(&opening(R)), input_hash(&failure(R)));

    let mut first = Spawns::opening("s-0123456789abcdef0123");
    assert!(matches!(
        open(&record, &clone, &opening(R), RepairMode::Pump(&mut first)),
        Opened::Spawned { .. }
    ));
    let mut again = Spawns::opening("s-ffffffffffffffffffff");
    assert_eq!(
        open(&record, &clone, &opening(HOP), RepairMode::Pump(&mut again)),
        Opened::Already {
            hash: opening_hash("0.5.0"),
            sid: "s-0123456789abcdef0123".into()
        }
    );
    assert!(again.spawned.is_empty(), "no second worker");

    // The line of the version it opened then goes red at that same commit: a cut's failure.
    let mut cut = Spawns::opening("s-11111111111111111111");
    assert!(matches!(
        open(&record, &clone, &failure(R), RepairMode::Pump(&mut cut)),
        Opened::Spawned { .. }
    ));
    let kept = Record::load(&record).unwrap();
    assert_eq!(
        kept.workers.iter().map(Worker::of_cut).collect::<Vec<_>>(),
        [false, true]
    );
}

/// A pump's cut opens its worker inside or outside a session. Outside one it reaches the
/// newest local aterm with no `@self`, in a tab it does not raise.
/// NEGATIVE CONTROL: with `@self` the spawn outside a session names no session and fails, so
/// an unattended red would open no worker.
#[test]
fn pump_cuts_dispatch_repairs_inside_and_outside_sessions() {
    for (in_session, pump, expected) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (false, true, true),
    ] {
        assert_eq!(
            unattended(in_session, pump),
            expected,
            "{in_session} {pump}"
        );
    }
    for in_session in [false, true] {
        assert!(spawner(in_session, true).is_some(), "the pump's cut spawns");
        assert!(
            spawner(in_session, false).is_none(),
            "the caller handles its repair"
        );
    }
    let words = unattended_args(Path::new("/c/clone"));
    assert_eq!(
        words,
        ["spawn", "raise=f", "agent=claude", "cwd=/c/clone", "-"]
    );
    assert!(!words.iter().any(|word| word.starts_with('@')));
    assert_eq!(spawn_args(Path::new("/c/clone"))[0], "@self");
}

/// The task tells the worker the line the pump reads.
#[test]
fn the_task_asks_for_the_verdict_line() {
    let failure = failure(R);
    let task = task(Path::new("/c/clone"), &failure, &input_hash(&failure));
    assert!(task.contains("`FIX <its 40 hex digits>`"), "{task}");
    assert!(task.contains("`NO-FIX <what it is>`"), "{task}");
}
