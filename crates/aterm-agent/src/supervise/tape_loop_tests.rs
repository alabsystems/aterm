// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// A RECORDED LOOP (`run_tests.rs`'s `tape_loop` module): recording changes
// nothing the loop does — its lines and its requests are the unrecorded
// run's, even when the tape cannot be written — a recorded approval reads
// back as its decision with the screen, the act and the line, and a run that
// dies inside a frame's write leaves a tape that reads to its tear.

use crate::supervise::tape::doors;
use crate::supervise::tape::frame_tests::Memory;
use crate::supervise::tape::header::VERBATIM;
#[cfg(unix)]
use crate::supervise::tape::recover::{Tail, recover};
use crate::supervise::tape::show;

/// The scripted worker these tests watch: a read box approved, a question and
/// an idle point handed over, then the session gone.
fn decisions_script() -> Mock {
    let mut m = fenced(vec![
        busy_screen(),
        bash_one_row(),
        busy_screen(),
        question_screen(),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(3);
    m
}

/// A watch over `m` as the whole-run helpers set one up, recorded into
/// `tape` when one is given, unrecorded otherwise: its lines and code.
fn watch_on(m: &mut Mock, opts: &SuperviseOpts, tape: Option<&MemoryTape>) -> (Vec<String>, u8) {
    let mut out: Vec<u8> = Vec::new();
    let code = match tape {
        None => {
            let mut s = session(m, None);
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            s.watch(opts, &mut out)
        }
        Some(tape) => {
            let mut taped = TapedCtl::new(m, tape.fx.clone(), doors::CTL);
            let mut s = taped_session(&mut taped, tape);
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            s.watch(opts, &mut out)
        }
    };
    let text = String::from_utf8(out).expect("utf-8");
    (text.lines().map(str::to_string).collect(), code)
}

/// A fixed clock not yet fixed, its door recording into `sink`.
fn tape_on(sink: Memory) -> MemoryTape {
    MemoryTape::on(
        &Fx::over(ClockSource::Scripted(Arc::default())),
        VERBATIM,
        sink,
    )
}

/// RECORDING CHANGES NOTHING: the same scripted worker watched with the
/// recorder off and on gives the very same lines, exit code and requests —
/// an approval's guarded key, a question's and an idle point's EVENTs, the
/// waits — and the tape holds every request the worker served, in order.
#[test]
fn recording_changes_nothing_the_loop_does() {
    let mut off = decisions_script();
    let (off_lines, off_code) = watch_on(&mut off, &auto(30), None);
    let mut on = decisions_script();
    let tape = tape_on(Memory::default());
    let (on_lines, on_code) = watch_on(&mut on, &auto(30), Some(&tape));
    assert_eq!(on_lines, off_lines);
    assert_eq!(on_code, off_code);
    assert_eq!(on.requests, off.requests);
    assert_eq!(on.claims, off.claims);
    let read = tape.finish("exit 1");
    let taped: Vec<String> = read
        .items
        .iter()
        .filter_map(|i| match i.record() {
            Some(astream_effects::EffectRecord::Exchange { door, request, .. })
                if door == doors::CTL =>
            {
                crate::supervise::tape::codec::ctl_args(request)
                    .ok()
                    .map(|w| w.join(" "))
            }
            _ => None,
        })
        .collect();
    // The claim's renewals too, which skip the session's own `call`: the
    // door wraps the transport.
    let sent: Vec<String> = on.order.iter().map(|(r, _)| r.clone()).collect();
    assert_eq!(taped, sent, "every request, the claim's included, in order");
    assert!(taped.iter().any(|r| r.starts_with("meta set supervisor ")));
}

/// A TAPE THAT CANNOT BE WRITTEN — every write refused, from its first — is
/// said once and changes nothing: the run's lines, code and requests are the
/// unrecorded run's.
#[test]
fn an_unwritable_tape_changes_nothing_the_loop_does() {
    let mut off = decisions_script();
    let (off_lines, off_code) = watch_on(&mut off, &auto(30), None);
    let sink = Memory::default();
    sink.taken().fail_after = Some(0);
    let tape = tape_on(sink);
    let mut on = decisions_script();
    let (on_lines, on_code) = watch_on(&mut on, &auto(30), Some(&tape));
    assert_eq!(on_lines, off_lines);
    assert_eq!(on_code, off_code);
    assert_eq!(on.requests, off.requests);
    let said = tape.said.text();
    assert_eq!(
        said.matches("the loop goes on without it").count(),
        1,
        "{said}"
    );
    assert!(tape.sink.bytes().is_empty(), "nothing reached the file");
}

/// "WHY DID IT PRESS 1?" from the tape alone: the recorded run's decisions
/// come back as `tape show --decisions` reads them — the approval with the
/// screen it judged (the box's rows), its rule, the guarded key that
/// followed and its reply, and the line the loop printed; the question and
/// the idle point handed over (no act after either).
#[test]
fn a_recorded_approval_reads_back_as_its_decision() {
    let mut m = decisions_script();
    let tape = tape_on(Memory::default());
    let (lines, _) = watch_on(&mut m, &auto(30), Some(&tape));
    let _ = tape.finish("exit 1");
    let read = show::open(&tape.sink.bytes(), false).expect("the tape reads");
    let decisions = show::decisions(&read);
    let approval = decisions
        .iter()
        .find(|d| d.door == doors::DECIDE_APPROVAL)
        .unwrap_or_else(|| panic!("an approval's mark: {decisions:#?}"));
    assert_eq!(approval.seq, 102);
    assert!(approval.rule.is_some(), "{approval:#?}");
    assert!(
        approval.outcome.starts_with("Approve {"),
        "{}",
        approval.outcome
    );
    assert_eq!(
        approval.screen.as_deref(),
        Some(bash_one_row().as_slice()),
        "the screen it judged, read back from the reply that carried it"
    );
    assert_eq!(
        approval.act.as_ref().map(|w| w.join(" ")).as_deref(),
        Some("key if-human=7 if-gen=1.102 if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1")
    );
    assert!(
        approval
            .reply
            .as_deref()
            .is_some_and(|r| r.starts_with("0 OK")),
        "{approval:#?}"
    );
    assert_eq!(approval.journal.as_deref(), Some(lines[0].as_str()));
    assert_eq!(lines[0], "APPROVED seq=102 git log --oneline -5");
    // The question is decided by the turn-end policy (the box is no
    // approval): its mark holds no act of its own.
    assert!(
        decisions
            .iter()
            .any(|d| d.door == doors::DECIDE_TURN_END && d.seq == 104),
        "{decisions:#?}"
    );
    // The turn-end note before it — the fold and the reading, for `why` —
    // holds every `Instant` as an offset from the tape's origin, never this
    // process's raw clock.
    let notes: Vec<&str> = read
        .items
        .iter()
        .filter_map(|i| match i {
            crate::supervise::tape::codec::Item::Note { kind, text } if kind == "turn_end" => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(!notes.is_empty(), "a turn-end note");
    for note in notes {
        assert!(!note.contains("Instant {"), "a raw clock: {note}");
        assert!(note.contains("point_at: Some(+"), "{note}");
    }
}

/// What the crashing child of [`a_crashed_run_leaves_a_tape_that_reads_to_its_tear`]
/// is told, and where its tape goes.
#[cfg(unix)]
const CRASH_CHILD: &str = "ATERM_TAPE_CRASH_CHILD";
#[cfg(unix)]
const CRASH_TEST: &str =
    "supervise::run::tests::tape_loop::a_crashed_run_leaves_a_tape_that_reads_to_its_tear";

/// A tape file that dies inside its `crash_at`-th frame's write: half the
/// frame reaches the disk, then the process is killed (`SIGKILL`, no
/// destructor runs) — a crash inside a frame's one `write(2)`.
#[cfg(unix)]
struct DyingFile {
    file: std::fs::File,
    frames: usize,
    crash_at: usize,
}

#[cfg(unix)]
impl crate::supervise::tape::frame::FrameSink for DyingFile {
    fn write_frame(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write as _;
        self.frames += 1;
        if self.frames == self.crash_at {
            self.file.write_all(&bytes[..bytes.len() / 2])?;
            crate::durable::sync_all(&self.file)?;
            aterm_uds::process::kill_self_now()
        }
        self.file.write_all(bytes)
    }

    fn sync(&mut self) -> std::io::Result<()> {
        aterm_durable::ordered(&self.file)
    }
}

/// The transport of the crashing child: every request appended to a log
/// before it is served, so the parent knows what the child had sent when it
/// died.
#[cfg(unix)]
struct Logged<'a> {
    inner: &'a mut Mock,
    log: std::fs::File,
}

#[cfg(unix)]
impl Ctl for Logged<'_> {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        use std::io::Write as _;
        let _ = writeln!(self.log, "{}", args.join(" "));
        self.inner.call(args)
    }
}

/// A RUN THAT DIES INSIDE A FRAME'S WRITE leaves a tape that reads to its
/// tear: a child of this test binary records a watch to a file and is killed
/// halfway through its fourth frame. The tape it leaves reads back (its
/// header, its whole frames), its torn tail dropped and said, no end on it;
/// and every control request on it is one the child sent, in order. The
/// child runs under a one-minute hang detector.
#[cfg(unix)]
#[test]
fn a_crashed_run_leaves_a_tape_that_reads_to_its_tear() {
    if let Some(dir) = std::env::var_os(CRASH_CHILD) {
        let dir = PathBuf::from(dir);
        let file = crate::supervise::tape::store::create(&dir.join("run.atape")).expect("tape");
        let sink = DyingFile {
            file,
            frames: 0,
            crash_at: 4,
        };
        let writer = crate::supervise::tape::frame::TapeWriter::start(
            Box::new(sink),
            "run.atape",
            crate::supervise::tape::testing::HEADER,
            Box::new(std::io::sink()),
        );
        let fx = Fx::over(ClockSource::Scripted(Arc::default()));
        let (origin, _) = fx.peek();
        let fx = fx.recording(crate::supervise::tape::core::TapeCore::start(
            writer, VERBATIM, origin, "test",
        ));
        let mut m = decisions_script();
        let log = std::fs::File::create(dir.join("requests.log")).expect("log");
        let mut logged = Logged { inner: &mut m, log };
        let mut taped = TapedCtl::new(&mut logged, fx.clone(), doors::CTL);
        let mut s = session(&mut taped, None);
        s.set_fx(fx);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let _ = s.watch(&auto(30), &mut std::io::sink());
        panic!("the child was to die inside its fourth frame");
    }
    let dir = crate::supervise::ScratchDir::new("tape-crash", "child");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    child
        .args(["--exact", CRASH_TEST, "--nocapture", "--test-threads", "1"])
        .env(CRASH_CHILD, dir.as_os_str());
    let output = aterm_uds::deadline::output_until(
        &mut child,
        std::time::Instant::now() + Duration::from_secs(60),
    )
    .expect("the crashing child hung or could not run");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(
            output.status.signal(),
            Some(9),
            "the child died by SIGKILL: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let bytes = std::fs::read(dir.join("run.atape")).expect("the tape");
    let read = recover(&bytes);
    assert!(
        matches!(read.tail, Tail::Torn { at } if at < bytes.len()),
        "{:?}",
        read.tail
    );
    assert_eq!(read.frames, 3, "the header and two whole records frames");
    assert_eq!(
        read.header.as_deref(),
        Some(crate::supervise::tape::testing::HEADER)
    );
    assert!(
        !read
            .items
            .iter()
            .any(|i| matches!(i, crate::supervise::tape::codec::Item::End(_))),
        "a run that died says no end"
    );
    let sent: Vec<String> = std::fs::read_to_string(dir.join("requests.log"))
        .expect("the child's log")
        .lines()
        .map(str::to_string)
        .collect();
    let taped: Vec<String> = read
        .items
        .iter()
        .filter_map(|i| match i.record() {
            Some(astream_effects::EffectRecord::Exchange { door, request, .. })
                if door == doors::CTL =>
            {
                crate::supervise::tape::codec::ctl_args(request)
                    .ok()
                    .map(|w| w.join(" "))
            }
            _ => None,
        })
        .collect();
    assert!(!taped.is_empty(), "whole frames hold requests");
    assert!(
        sent.starts_with(&taped),
        "the tape's requests are a prefix of what the child sent:\n{taped:#?}\n{sent:#?}"
    );
}

/// A window host that keeps what it was told and answers as a quiet host
/// does: the hosted store's test's.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[derive(Debug, Default)]
struct Kept {
    told: std::sync::Mutex<Vec<String>>,
}

#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
impl IdleHost for Kept {
    fn wants(&self) -> bool {
        self.told.lock().unwrap().push("wants".to_string());
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn task_face(&self, face: crate::harness::acts::TaskFace) {
        self.told.lock().unwrap().push(format!("face {face:?}"));
    }
}

/// A HOSTED RUN UNDER THE DEVELOPMENT SEAM (`tape::store::run_hosted_taped`,
/// the window host's path when `ATERM_HARNESS_TAPE` is set): the run is
/// taped to the store beside its ledger — `<state>/drive/tapes/<sid>/`, the
/// file `0600`, every directory `0700` — with its window host's answers and
/// notifications on it, and the tape, read back from the store, REPLAYS:
/// the requests the worker was sent, asked again from the tape alone (the
/// replay's host holds none to ask: `tape::host::ReplayHost`). A run a
/// seamless update's Commit stopped ends its tape SUSPENDED, and replays
/// all the same.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[test]
fn a_hosted_run_is_taped_into_its_store_and_replays() {
    use crate::supervise::tape::codec::{End, Item};
    use crate::supervise::tape::replay::{Options, replay};
    use crate::supervise::tape::report::Verdict;
    use crate::supervise::tape::store;

    let guard = crate::supervise::ScratchDir::new("tape-hosted", "store");
    let state = guard.canonicalize().expect("owned canonical fixture");
    let ledger = approvals::path_under(&state, Some("@s-1")).expect("the ledger's drive/");
    // The budget spent at once (a stop's `Ok`, as a Commit's is), and a run
    // the worker's session ends under.
    for (max, suspended) in [(Duration::ZERO, true), (Duration::from_secs(30), false)] {
        let host = Arc::new(Kept::default());
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ledger: Some(ledger.clone()),
            journal: Some(approvals::journal_beside(&ledger)),
            max,
            ..SuperviseOpts::hosted()
        };
        let mut m = Mock::new(vec![busy_screen(), idle_screen()]);
        m.vanish_after = Some(3);
        let from = m.order.len();
        let setup = {
            let mut s = Session::new(&mut m, Some("@s-1".to_string()));
            s.set_supervisor_name(Some("aterm-harness".to_string()));
            s.setup().clone()
        };
        let before = store::list(&store::store_under(&state), None).len();
        let mut out: Vec<u8> = Vec::new();
        let ended = store::run_hosted_taped(
            &mut m,
            setup,
            &opts,
            Arc::new(AtomicBool::new(false)),
            &mut out,
            Box::new(std::io::sink()),
            || suspended,
        );
        let told = host.told.lock().unwrap().clone();
        assert!(
            !told.is_empty(),
            "the host was told the task's face: {ended:?}"
        );
        let tapes = store::list(&store::store_under(&state), Some("@s-1"));
        assert_eq!(tapes.len(), before + 1, "one tape a run: {tapes:#?}");
        let newest = &tapes[0];
        assert_eq!(
            newest.path.parent(),
            Some(state.join("drive").join("tapes").join("s-1").as_path())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &std::path::Path| {
                std::fs::metadata(p).expect("stat").permissions().mode() & 0o777
            };
            assert_eq!(mode(&newest.path), 0o600, "the tape is private");
            assert_eq!(mode(&state.join("drive").join("tapes")), 0o700);
            assert_eq!(mode(&state.join("drive").join("tapes").join("s-1")), 0o700);
        }
        let bytes = std::fs::read(&newest.path).expect("the tape");
        let read = recover(&bytes);
        assert_eq!(read.tail, Tail::Clean);
        assert!(
            read.items.iter().any(|i| matches!(
                i.record(),
                Some(astream_effects::EffectRecord::Emit { door, .. }) if door == "host.task_face"
            )),
            "the host's notification is on the tape"
        );
        let end = read.items.iter().rev().find_map(|i| match i {
            Item::End(end) => Some(end.clone()),
            _ => None,
        });
        if suspended {
            assert_eq!(end, Some(End::Suspended), "{ended:?}");
        } else {
            assert!(matches!(end, Some(End::Returned { .. })), "{end:?}");
        }
        let report = replay(&bytes, &Options::default()).expect("replayed");
        assert_eq!(report.verdict, Verdict::Reproduced, "{}", report.render());
        let asked: Vec<String> = report.requests.iter().map(|w| w.join(" ")).collect();
        assert_eq!(asked, sent(&m, from), "the replay asks what the run asked");
    }
}

/// A window host that panics while it is told the task's face.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[derive(Debug, Default)]
struct PanicsTold;

#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
impl IdleHost for PanicsTold {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn task_face(&self, face: crate::harness::acts::TaskFace) {
        let _ = face;
        panic!("a planted panic in the host, told the task's face");
    }
}

/// A HOST THAT PANICS WHILE IT IS TOLD REPLAYS TO PANICKED AT THAT DOOR, as a
/// host that panics while it is asked does: the hosted run's tape holds the
/// notification — said before the host was told — as its last record, and
/// the door and request the run died at (`NOTE_UNWOUND_IN`, written by
/// `Fx::host_tell`); its replay, which tells no host, reproduces every record
/// and stops at that very notification: PANICKED at `host.task_face`, the
/// recorded message, exit 4. NEGATIVE CONTROL: the same tape without its
/// note — what the taped host wrote before it said where a notification died
/// — goes on past the notification to a door the tape no longer holds, and
/// is DIVERGED, as behaviour changed.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[test]
fn a_host_that_panics_while_it_is_told_replays_to_panicked_at_that_door() {
    use crate::supervise::tape::codec::Item;
    use crate::supervise::tape::core::NOTE_UNWOUND_IN;
    use crate::supervise::tape::replay::{Options, panic_message, replay};
    use crate::supervise::tape::report::Verdict;
    use crate::supervise::tape::store;
    use crate::supervise::tape::testing::written;

    let guard = crate::supervise::ScratchDir::new("tape-hosted", "told-panic");
    let state = guard.canonicalize().expect("owned canonical fixture");
    let ledger = approvals::path_under(&state, Some("@s-1")).expect("the ledger's drive/");
    let opts = SuperviseOpts {
        idle_host: Some(Arc::new(PanicsTold) as Arc<dyn IdleHost>),
        ledger: Some(ledger.clone()),
        journal: Some(approvals::journal_beside(&ledger)),
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let mut m = Mock::new(vec![busy_screen(), idle_screen()]);
    m.vanish_after = Some(3);
    let setup = {
        let mut s = Session::new(&mut m, Some("@s-1".to_string()));
        s.set_supervisor_name(Some("aterm-harness".to_string()));
        s.setup().clone()
    };
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store::run_hosted_taped(
            &mut m,
            setup,
            &opts,
            Arc::new(AtomicBool::new(false)),
            &mut Vec::new(),
            Box::new(std::io::sink()),
            || false,
        )
    }));
    let message = panic_message(ran.expect_err("the host's planted panic").as_ref());
    let tapes = store::list(&store::store_under(&state), Some("@s-1"));
    assert_eq!(tapes.len(), 1, "{tapes:#?}");
    let bytes = std::fs::read(&tapes[0].path).expect("the tape");
    let report = replay(&bytes, &Options::default()).expect("replayed");
    assert_eq!(
        report.verdict,
        Verdict::Panicked {
            message,
            door: Some("host.task_face".to_string()),
        },
        "{}",
        report.render()
    );
    assert_eq!(report.consumed, report.effects, "every record reproduced");
    assert_eq!(report.code(), 4);

    let read = recover(&bytes);
    let header = read.header.expect("the tape's header");
    let unsaid: Vec<Item> = read
        .items
        .into_iter()
        .filter(|i| !matches!(i, Item::Note { kind, .. } if kind == NOTE_UNWOUND_IN))
        .collect();
    let report = replay(&written(&header, &unsaid), &Options::default()).expect("replayed");
    assert!(
        matches!(report.verdict, Verdict::Diverged(_)),
        "{}",
        report.render()
    );
}
