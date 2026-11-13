// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};

use super::*;

const R: &str = "cccccccccccccccccccccccccccccccccccccccc";
const HOP: &str = "1111111111111111111111111111111111111111";
const OTHER: &str = "2222222222222222222222222222222222222222";

/// A spawner inside a session or outside one: it records each spawn's folder and task, and
/// answers them in turn.
struct Spawns {
    in_session: bool,
    answers: Vec<Result<String, String>>,
    spawned: Vec<(PathBuf, String)>,
}

impl Spawns {
    fn answering(answers: Vec<Result<String, String>>) -> Self {
        Self {
            in_session: true,
            answers,
            spawned: Vec::new(),
        }
    }

    fn opening(sid: &str) -> Self {
        Self::answering(vec![Ok(sid.into())])
    }

    fn outside() -> Self {
        Self {
            in_session: false,
            ..Self::answering(Vec::new())
        }
    }
}

impl Spawner for Spawns {
    fn in_session(&self) -> bool {
        self.in_session
    }

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
    }
}

#[test]
fn failed_cut_spawns_one_repair_worker() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let record = scratch.path().join("aterm-release").join(RECORD_FILE);
    let clone = scratch.path().join("clone");
    let red = failure(R);
    let hash = input_hash(&red);

    // A failing cut in a session spawns exactly one worker, in the release clone, told the
    // failing log and the cut's inputs.
    let mut first = Spawns::opening("s-0123456789abcdef0123");
    assert_eq!(
        open(&record, &clone, &red, &mut first),
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
        open(&record, &clone, &failed_again, &mut again),
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
        open(&record, &clone, &hop, &mut next),
        Opened::Spawned {
            hash: input_hash(&hop),
            sid: "s-1111111111111111aaaa".into()
        }
    );
    assert_eq!(next.spawned.len(), 1);
    assert_eq!(Record::load(&record).expect("record").workers.len(), 2);

    // Outside an aterm session it prints the command instead: nothing spawns and nothing is
    // recorded, so a run inside one still opens the worker.
    let other = failure(OTHER);
    let mut outside = Spawns::outside();
    let opened = open(&record, &clone, &other, &mut outside);
    let Opened::Command { command, .. } = &opened else {
        panic!("outside a session the command is given: {opened:?}");
    };
    assert!(outside.spawned.is_empty(), "{:?}", outside.spawned);
    assert!(
        command.starts_with(&format!(
            "aterm ctl @self spawn agent=claude {} - <<'ATERM_REPAIR_TASK'\n",
            shell_word(&format!("cwd={}", clone.display()))
        )),
        "{command}"
    );
    assert!(
        command.ends_with(&format!(
            "{}ATERM_REPAIR_TASK",
            task(&clone, &other, &input_hash(&other))
        )),
        "the here-document is the task: {command}"
    );
    let said = opened.lines(&record);
    assert!(
        said[0].starts_with("repair: not in an aterm session"),
        "{said:?}"
    );
    assert_eq!(
        said[1..].join("\n"),
        *command,
        "the command is printed whole"
    );
    assert_eq!(Record::load(&record).expect("record").workers.len(), 2);
}

#[test]
fn a_spawn_that_fails_is_said_and_kept_nowhere() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let record = scratch.path().join(RECORD_FILE);
    let red = failure(R);
    let mut refused = Spawns::answering(vec![Err("ERR spawn agent=claude: no such window".into())]);
    let opened = open(&record, scratch.path(), &red, &mut refused);
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
        open(&record, scratch.path(), &red, &mut working),
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
    let opened = open(&record, scratch.path(), &failure(R), &mut spawns);
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
fn the_task_fits_one_spawn_and_the_command_reads_it_back() {
    let clone = Path::new("/c/a clone's/path");
    let huge = Failure {
        why: format!("{}\n{}", "x".repeat(200_000), "line\n".repeat(5_000)),
        detail: vec![format!("nul\0here\r{TASK_END}"); 2_000],
        remedy: format!("{TASK_END}\n").repeat(50),
        ..failure(R)
    };
    let told = task(clone, &huge, &input_hash(&huge));
    assert!(told.len() <= MAX_TASK_BYTES, "{} bytes", told.len());
    assert!(!told.contains('\0') && !told.contains('\r'));
    assert!(told.ends_with('\n'));
    assert!(
        told.lines().all(|line| line != TASK_END),
        "no line ends the here-document early"
    );
    assert!(told.contains("more line(s), in the log"));
    let command = command(clone, &told);
    assert!(
        command.starts_with(
            "aterm ctl @self spawn agent=claude 'cwd=/c/a clone'\\''s/path' - \
             <<'ATERM_REPAIR_TASK'\n"
        ),
        "{command}"
    );
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
        log: PathBuf::from(format!(
            "/c/clone/.git/aterm-release/logs/run\n{TASK_END}\r\0/04-qualify.log"
        )),
        step: format!("4/10 qualify\n{TASK_END}"),
        ..failure(R)
    };
    let hash = input_hash(&huge);
    let told = task(clone, &huge, &hash);
    assert!(told.len() <= MAX_TASK_BYTES, "{} bytes", told.len());
    assert!(!told.contains('\0') && !told.contains('\r'));
    assert!(
        told.lines().all(|line| line != TASK_END),
        "no line ends the here-document early:\n{told}"
    );
    for word in [
        "What failed: verify release: test, at step 4/10 qualify ATERM_REPAIR_TASK of",
        "The step's log, which ends with that detail: /c/clone/.git/aterm-release/logs/run \
         ATERM_REPAIR_TASK  /04-qualify.log",
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
    let opened = open(&record, scratch.path(), &red, &mut spawns);
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
        open(&record, scratch.path(), &red, &mut spawns),
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
