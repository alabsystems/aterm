// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's harness watchdog's relaunch with `<tool> run` (HX-15f), driven through the stream's
//! own rig: a real claim on a real connection, the claimant's `claim` and `session` records
//! applied by the shipping reader, the re-exec grace run out by the shipping lapse, and the
//! ending against the kernel stand-in that holds one kept child. The rig's world records the
//! relaunch's back-off and the words handed to the typer, so nothing is slept or typed; its
//! claimant is the shared stand-in, which it reads as gone, so nothing is ever signalled.

use alab_wire::ChildProcess;
use alab_wire::record::{Kind as WireKind, Mode};

use super::*;
use crate::harness_claim::{RUN_WORD_MAX, RUN_WORDS_MAX, RunRefusal, validate_run};
use crate::harness_watchdog::tests::Kernel;
use crate::harness_watchdog::{Ending, Why, end_harness, take_snapshot};
use crate::session_timeline::{Resume, Unsupervised};

/// The child every `claim` record here keeps.
const CHILD: ChildProcess = ChildProcess {
    pid: 4243,
    start: 1_792_000_000_000_000,
};
/// Another child, the one a harness that restarted its child claims.
const NEXT_CHILD: ChildProcess = ChildProcess {
    pid: 4244,
    start: 1_792_000_000_000_001,
};
/// The vendor's conversation the claimant's `session` names.
const CONVERSATION: &str = "4f1c2a9e-0d6b-4a1e-9c3f-5b7e8d2a1c00";
/// The directory the claimant's `session` says the conversation runs in.
const PROJECT: &str = "/srv/project";

/// A claimant on [`ONE`]: its stream, its connection's serial, and the last record it sent.
struct Running {
    stream: Client,
    id: u64,
    seq: u64,
}

impl Running {
    /// Send `body` after the last record and see it taken.
    fn send(&mut self, body: Body) {
        self.seq += 1;
        assert_eq!(
            self.stream.record(self.seq, body.clone()),
            (self.seq, None),
            "{body:?}"
        );
    }
}

/// A claimant of `claim` (`claim <sid> 4 <claim>`'s words) on [`ONE`] with its stream open,
/// each of `records` sent after where the stream stands and taken.
fn harness(rig: &Rig, claim: &str, records: Vec<Body>) -> Running {
    let mut stream = rig.claimant(ONE, claim);
    let id = stream.id();
    let seq = stream.open(ONE).acked;
    let mut running = Running { stream, id, seq };
    for body in records {
        running.send(body);
    }
    running
}

/// A `claim` record keeping [`CHILD`], the claim's own `login=` kept.
fn keeping() -> Body {
    keeping_child(CHILD)
}

/// A `claim` record naming `child`, the claim's own `login=` kept.
fn keeping_child(child: ChildProcess) -> Body {
    Body::Claim {
        child: Some(child),
        scope: None,
        version: None,
        vendor: None,
        login: None,
    }
}

/// What resumes `words`, in [`PROJECT`].
fn resume(words: &[&str]) -> Resume {
    Resume {
        words: words.iter().map(ToString::to_string).collect(),
        cwd: Some(PROJECT.to_string()),
    }
}

/// The claimant's `session`: resumed by `resume`, taking `takes` of aterm's optional kinds. It
/// names an account of its own, `personal`, which the relaunch never types.
fn session(resume: &[&str], takes: &[WireKind]) -> Body {
    Body::Session {
        agent: "claude".to_string(),
        version: "0.3.0".to_string(),
        vendor: Some("2.1.330".to_string()),
        account: Some("personal".to_string()),
        conversation: Some(CONVERSATION.to_string()),
        cwd: Some(PROJECT.to_string()),
        resume: resume.iter().map(ToString::to_string).collect(),
        mode: Mode::Tui,
        takes: takes.to_vec(),
    }
}

fn state(said: State) -> Body {
    Body::State {
        state: said,
        composer: None,
    }
}

/// The claimant's connection closed and its re-exec grace ran out: the lapse hands its claim to
/// the watchdog. Its holder and its claim.
fn gone(rig: &Rig, running: Running) -> (String, Scoped) {
    drop(running.stream);
    rig.ended(running.id);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let expiry = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .supervisor_expiry()
        .expect("the grace");
    let lapsed = crate::session_timeline::publish_lapse_supervisor(&ctx, expiry)
        .published()
        .expect("the claim lapsed");
    (lapsed.holder, lapsed.universal.expect("a contract 4 claim"))
}

/// The watchdog's ending of `ended`'s harness on [`ONE`], against `kernel`.
fn end(rig: &Rig, ended: &(String, Scoped), kernel: &mut Kernel) -> Ending {
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    end_harness(
        &Arc::downgrade(&ctx),
        &ended.0,
        &ended.1,
        Why::Grace,
        rig.service.as_ref(),
        kernel,
    )
}

/// The kernel stand-in where [`CHILD`] reads at its birth until a signal ends it.
fn kernel() -> Kernel {
    Kernel::reading(CHILD.pid, CHILD.start)
}

/// The words handed to the typer since the last look, and the back-off waited before each.
fn typed(rig: &Rig) -> Vec<Vec<String>> {
    std::mem::take(
        &mut *rig
            .service
            .typed
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    )
}

fn paused(rig: &Rig) -> Vec<Duration> {
    std::mem::take(
        &mut *rig
            .service
            .paused
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    )
}

/// The verified tool of registry row `row` (the seeds' here: a test reads no store).
fn tool(row: &str) -> String {
    aterm_agent::harness::registry::current()
        .entry(row)
        .unwrap_or_else(|| panic!("the seed row {row}"))
        .tool
        .clone()
}

/// `<tool of row> run <flags…> -- <resume…>`.
fn line(row: &str, flags: &[&str], resume: &[&str]) -> Vec<String> {
    let mut words = vec![tool(row), "run".to_string()];
    words.extend(flags.iter().map(ToString::to_string));
    words.push("--".to_string());
    words.extend(resume.iter().map(ToString::to_string));
    words
}

/// Whether [`ONE`] reads `unsupervised`.
fn unsupervised(rig: &Rig) -> bool {
    rig.service
        .handle(ONE)
        .ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .unsupervised
        .is_some()
}

/// The child the live claim on [`ONE`] keeps.
fn kept_child(rig: &Rig) -> crate::harness_claim::KeptChild {
    rig.service
        .handle(ONE)
        .ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .live_scoped(crate::metrics::now_us())
        .and_then(|scoped| scoped.child)
        .expect("the kept child")
}

/// HX-15f EXIT TEST: A DEAD HARNESS TAB IS RELAUNCHED WITH `<tool> run` FROM THE KEPT CHILD'S
/// RECORD (D21, D22). A contract 4 claimant keeps a child, whose built-in relaunch snapshot is
/// taken once while the claim keeps it; the claimant sends no `session`. Its re-exec grace runs
/// out and the watchdog ends it: once the kept child is read gone by its identity, the tab is
/// relaunched by typing the claim's row's verified tool, `run`, `--` and the snapshot's resume
/// arguments, after the first step of the back-off (1 s).
/// NEGATIVE HALF: the same child is not read again; a child the claim does not keep, or one
/// that does not read at its birth, leaves no snapshot; nothing is typed before the ending.
#[test]
fn a_dead_harness_tab_is_relaunched_with_tool_run_from_the_kept_childs_record() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let running = harness(&rig, "state", vec![keeping()]);
    let kept = kept_child(&rig);
    assert_eq!((kept.pid, kept.birth.0), (CHILD.pid, CHILD.start));

    let reads = AtomicU64::new(0);
    let read = |sid: &str, pid: u32| {
        assert_eq!(sid, ONE);
        reads.fetch_add(1, Ordering::Relaxed);
        Some(resume(&["--resume", &format!("{CONVERSATION}-of-{pid}")]))
    };
    // NEGATIVE: a child that does not read at its birth is not read; one the claim does not
    // keep is read and not kept.
    let mut moved = Kernel::reading(CHILD.pid, CHILD.start + 1);
    assert!(!take_snapshot(&ctx.meta, ONE, kept, &read, &mut moved));
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    let stranger = crate::harness_claim::KeptChild {
        pid: CHILD.pid + 1,
        ..kept
    };
    let mut other = Kernel::reading(stranger.pid, CHILD.start);
    assert!(!take_snapshot(&ctx.meta, ONE, stranger, &read, &mut other));
    assert!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .relaunch
            .snapshot
            .is_none()
    );
    // The kept child's snapshot, read once.
    assert!(take_snapshot(&ctx.meta, ONE, kept, &read, &mut kernel()));
    assert!(!take_snapshot(&ctx.meta, ONE, kept, &read, &mut kernel()));
    assert_eq!(
        reads.load(Ordering::Relaxed),
        2,
        "the stranger's and the child's"
    );
    assert!(typed(&rig).is_empty(), "nothing is typed before the ending");

    let ended = gone(&rig, running);
    let mut kernel = kernel();
    assert!(matches!(
        end(&rig, &ended, &mut kernel),
        Ending::Ended { child: Some(Ok(_)) }
    ));
    assert_eq!(
        kernel.sent.iter().map(|(_, sig)| *sig).collect::<Vec<_>>(),
        [libc::SIGTERM],
        "the kept child was ended first"
    );
    let resume = format!("{CONVERSATION}-of-{}", CHILD.pid);
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", &resume])]
    );
    assert_eq!(paused(&rig), [Duration::from_secs(1)]);
    assert!(
        unsupervised(&rig),
        "the tab reads unsupervised until it is claimed"
    );
}

/// HX-15f EXIT TEST: A `session` RECORD GIVES THE RESUME AND THE CLAIM'S `login=` GIVES THE
/// ACCOUNT (D21). The claimant claims with `login=work`, keeps a child whose snapshot is taken,
/// and sends a `session` naming other resume arguments and its own account: the relaunch types
/// `--account work` and the `session`'s resume, which wins over the snapshot.
/// NEGATIVE HALF: the account the `session` names is never typed, and a claim with no
/// `login=` types no `--account`.
#[test]
fn a_session_record_gives_the_resume_and_the_claims_login_gives_the_account() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let running = harness(
        &rig,
        "state login=work",
        vec![keeping(), session(&["--resume", CONVERSATION], &[])],
    );
    let snapshot = |_: &str, _: u32| Some(resume(&["--resume", "snapshot"]));
    assert!(take_snapshot(
        &ctx.meta,
        ONE,
        kept_child(&rig),
        &snapshot,
        &mut kernel()
    ));
    let ended = gone(&rig, running);
    assert_eq!(ended.1.claim.login.as_deref(), Some("work"));
    assert!(matches!(
        end(&rig, &ended, &mut kernel()),
        Ending::Ended { .. }
    ));
    let words = typed(&rig);
    assert_eq!(
        words,
        [line(
            "claude-code-alab",
            &["--account", "work"],
            &["--resume", CONVERSATION]
        )]
    );
    assert!(
        !words[0].iter().any(|word| word == "personal"),
        "the session's own account is never typed"
    );

    // NEGATIVE: a claim with no `login=` names no account, whatever the `session` says.
    let running = harness(&rig, "state", vec![keeping()]);
    let ended = gone(&rig, running);
    assert_eq!(ended.1.claim.login, None);
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", CONVERSATION])]
    );
}

/// HX-15f EXIT TEST: A HARNESS THAT RELEASED OR ENDED IS NOT RELAUNCHED (D23). A claimant that
/// released its claim itself (`meta unset supervisor`) ended normally, and so did one whose
/// stream's last `state` was `ended`: the watchdog's ending of either types nothing, and the
/// tab is left `unsupervised`.
/// NEGATIVE HALF: the same claimant ended by aterm (its grace lapsed) with its last `state`
/// `working` is relaunched.
#[test]
fn a_harness_that_released_or_ended_is_not_relaunched() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let resume = ["--resume", CONVERSATION];

    // Released: its own `meta unset supervisor`.
    let running = harness(
        &rig,
        "state",
        vec![keeping(), session(&resume, &[]), state(State::Working)],
    );
    let scoped = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .live_scoped(crate::metrics::now_us())
        .cloned()
        .expect("the claim");
    let holder = format!("claude-code-alab@{}", scoped.proven.pid);
    assert_eq!(
        rig.service.meta(&format!("meta {ONE} unset supervisor")),
        "OK"
    );
    assert_eq!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .relaunch
            .released
            .as_ref(),
        Some(&scoped.proven),
        "an explicit release"
    );
    drop(running.stream);
    rig.ended(running.id);
    assert!(matches!(
        end(&rig, &(holder, scoped), &mut kernel()),
        Ending::Ended { .. }
    ));
    assert!(
        typed(&rig).is_empty(),
        "a released claimant is not relaunched"
    );
    assert!(unsupervised(&rig));

    // Ended: its last `state` said so.
    let running = harness(&rig, "state", vec![keeping(), state(State::Ended)]);
    assert!(rig.state(ONE).ended);
    let ended = gone(&rig, running);
    assert!(matches!(
        end(&rig, &ended, &mut kernel()),
        Ending::Ended { .. }
    ));
    assert!(typed(&rig).is_empty(), "an ended stream is not relaunched");
    assert!(paused(&rig).is_empty());
    assert!(unsupervised(&rig));

    // NEGATIVE: aterm's own release (the lapse) of a claimant that last said `working`.
    let running = harness(&rig, "state", vec![keeping(), state(State::Working)]);
    assert!(!rig.state(ONE).ended);
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(typed(&rig), [line("claude-code-alab", &[], &resume)]);
}

/// HX-15f EXIT TEST: A RELAUNCH ARGUMENT THAT FAILS VALIDATION TYPES NOTHING (D21). A `session`
/// whose resume holds a terminal escape, and one whose resume word is over the bound, are each
/// applied as the codec reads them; the relaunch's words fail
/// [`crate::harness_claim::validate_run`], so nothing is waited out or typed and the tab is left
/// `unsupervised`. Each refusal of the validation is named.
/// NEGATIVE HALF: the same claimant's resume within the bounds is typed.
#[test]
fn a_relaunch_argument_that_fails_validation_types_nothing() {
    let rig = Rig::new(2);
    for resume in [
        "x\u{1b}]0;pwned\u{7}".to_string(),
        "x".repeat(RUN_WORD_MAX + 1),
        "caf\u{e9}".to_string(),
    ] {
        let running = harness(&rig, "state", vec![session(&["--resume", &resume], &[])]);
        let ended = gone(&rig, running);
        assert!(matches!(
            end(&rig, &ended, &mut kernel()),
            Ending::Ended { child: None }
        ));
        assert!(typed(&rig).is_empty(), "{resume:?} is never typed");
        assert!(paused(&rig).is_empty(), "nothing was waited out for it");
        assert!(unsupervised(&rig));
    }

    let tool = tool("claude-code-alab");
    let words = |rest: &[&str]| {
        std::iter::once(tool.clone())
            .chain(rest.iter().map(ToString::to_string))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        validate_run(&words(&["run", "--", "--resume", "a b"]), &tool),
        Ok(())
    );
    assert_eq!(
        validate_run(&words(&["run", "--", ""]), &tool),
        Err(RunRefusal::Word { at: 3 })
    );
    assert_eq!(
        validate_run(&words(&["run", "--", "a\nb"]), &tool),
        Err(RunRefusal::Word { at: 3 })
    );
    let many = vec!["x"; RUN_WORDS_MAX];
    assert_eq!(validate_run(&words(&many), &tool), Err(RunRefusal::TooMany));

    // NEGATIVE: within the bounds, the same claimant is relaunched.
    let running = harness(&rig, "state", vec![session(&["--resume", "x y"], &[])]);
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", "x y"])]
    );
}

/// HX-15f EXIT TEST: A VENDOR WITH NO RECORD READER IS LEFT UNSUPERVISED (D22). A Grok harness
/// (no built-in snapshot reader before HX-19) keeps a child whose snapshot reads nothing and
/// sends no `session`: the watchdog ends it and its kept child, and the tab is left
/// `unsupervised` with nothing waited out or typed.
/// NEGATIVE HALF: once the same row's harness sends a `session`, it is relaunched with that
/// row's own tool.
#[test]
fn a_vendor_with_no_record_reader_is_left_unsupervised() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let running = harness(&rig, "state grok-alab", vec![keeping()]);
    let no_reader = |_: &str, _: u32| None;
    assert!(!take_snapshot(
        &ctx.meta,
        ONE,
        kept_child(&rig),
        &no_reader,
        &mut kernel()
    ));
    let ended = gone(&rig, running);
    assert_eq!(ended.1.claim.row.id, "grok-alab");
    let mut kernel = kernel();
    assert!(matches!(
        end(&rig, &ended, &mut kernel),
        Ending::Ended { child: Some(Ok(_)) }
    ));
    assert_eq!(kernel.sent.len(), 1, "its kept child was ended");
    assert!(typed(&rig).is_empty());
    assert!(paused(&rig).is_empty());
    assert!(unsupervised(&rig));

    // NEGATIVE: with a `session`, the row's own tool relaunches it.
    let running = harness(
        &rig,
        "state grok-alab",
        vec![keeping(), session(&["--resume", CONVERSATION], &[])],
    );
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut self::kernel());
    let words = typed(&rig);
    assert_eq!(words, [line("grok-alab", &[], &["--resume", CONVERSATION])]);
    assert_ne!(tool("grok-alab"), tool("claude-code-alab"));
}

/// HX-15f EXIT TEST: A RECORD'S FIRST WORD IS NEVER RUN (D21). A `session` whose agent word and
/// whose resume both name a shell command: the line the relaunch types starts with the claim's
/// row's verified tool and `run`, and the record's words come only after `--`, as arguments
/// of that tool. The validation refuses any line that starts otherwise.
/// NEGATIVE HALF: a line whose first word is the record's, or whose second is not `run`, fails
/// validation.
#[test]
fn a_records_first_word_is_never_run() {
    let rig = Rig::new(2);
    let command = ["/bin/sh", "-c", "touch /tmp/hx15f-pwned"];
    let mut said = session(&command, &[]);
    if let Body::Session { agent, .. } = &mut said {
        *agent = "sh".to_string();
    }
    let running = harness(&rig, "state", vec![said]);
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    let words = typed(&rig);
    let tool = tool("claude-code-alab");
    assert_eq!(words, [line("claude-code-alab", &[], &command)]);
    let typed = &words[0];
    assert_eq!(
        typed[..3],
        [tool.clone(), "run".to_string(), "--".to_string()]
    );
    assert_ne!(typed[0], "sh");

    // NEGATIVE: only the row's tool, then `run`, starts a line.
    let line = |words: &[&str]| words.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        validate_run(&line(&["/bin/sh", "-c", "touch"]), &tool),
        Err(RunRefusal::NotTheTool)
    );
    assert_eq!(
        validate_run(&line(&[tool.as_str(), "-c", "touch"]), &tool),
        Err(RunRefusal::NotRun)
    );
    assert_eq!(
        validate_run(&line(&["sh", "run", "--"]), "sh -c"),
        Err(RunRefusal::NotTheTool),
        "a tool that is no word"
    );
}

/// HX-15f EXIT TEST: THE RELAUNCH BACKS OFF AND TYPES `--model` ONLY FOR A MODEL RESTART (D21,
/// alab-wire's `relaunch::Ladder`). Four endings of the same claimant: the first relaunch waits
/// 1 s, the next 10 s and the next 60 s on the awake clock that does not move; after 600 s of
/// that clock's health the streak is forgiven and the next waits 1 s again. A `restart
/// model=opus` written for the claimant before its second ending rides that relaunch as
/// `--model opus`, once.
/// NEGATIVE HALF: no other relaunch types `--model`.
#[test]
fn the_relaunch_backs_off_and_types_model_only_for_a_model_restart() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let resume = ["--resume", CONVERSATION];
    let mut first = true;
    let mut round = |model: Option<&str>| {
        let records = if std::mem::take(&mut first) {
            vec![session(&resume, &[])]
        } else {
            Vec::new()
        };
        let running = harness(&rig, "state,relaunch", records);
        if let Some(model) = model {
            assert!(crate::restart_act::record_scoped_restart(
                &ctx,
                "model",
                None,
                Some(model)
            ));
        }
        let ended = gone(&rig, running);
        end(&rig, &ended, &mut kernel());
        let words = typed(&rig);
        assert_eq!(words.len(), 1, "{words:?}");
        (paused(&rig), words.into_iter().next().unwrap_or_default())
    };

    let plain = line("claude-code-alab", &[], &resume);
    assert_eq!(round(None), (vec![Duration::from_secs(1)], plain.clone()));
    assert_eq!(
        round(Some("opus")),
        (
            vec![Duration::from_secs(10)],
            line("claude-code-alab", &["--model", "opus"], &resume)
        )
    );
    assert_eq!(
        round(None),
        (vec![Duration::from_secs(60)], plain.clone()),
        "the model rode one relaunch"
    );
    let healthy = u64::try_from(alab_wire::relaunch::HEALTHY.as_millis()).expect("ms");
    rig.service.clock.fetch_add(healthy, Ordering::AcqRel);
    assert_eq!(
        round(None),
        (vec![Duration::from_secs(1)], plain),
        "forgiven after a healthy run"
    );
}

/// HX-15f EXIT TEST: A RELAUNCH READS `session` AFTER ITS LOG SEGMENT IS DELETED (D31). The
/// claimant's `session` is kept, its latest line only, in the current-state record and outside
/// the log's retention: the session's log is replaced by an empty one (its segments deleted),
/// and the relaunch still types that record's resume.
/// NEGATIVE HALF: the `session` wrote no row, and the replaced log holds none of the run the
/// stream opened.
#[test]
fn a_relaunch_reads_session_after_its_log_segment_is_deleted() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    // A first applied record opens the agent run of the claim's row (D15), which writes its row.
    let mut running = harness(&rig, "state", vec![Body::Beat]);
    let rows = || {
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slog()
            .log()
            .len()
    };
    let before = rows();
    running.send(session(&["--resume", "older"], &[]));
    running.send(session(&["--resume", CONVERSATION], &[WireKind::Prompt]));
    assert_eq!(rows(), before, "a `session` writes no row");
    let ended = gone(&rig, running);
    {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
        let kept = timeline
            .session_of(&ended.1.proven)
            .expect("the latest session")
            .clone();
        assert_eq!(kept.resume, ["--resume", CONVERSATION], "its latest line");
        assert!(kept.line.contains(r#""k":"session""#), "{}", kept.line);
        assert!(timeline.slog().log().len() > 0, "the run's rows");
        timeline.slog_mut().replace_log(aterm_slog::Log::new(2));
        assert_eq!(timeline.slog().log().len(), 0, "every segment deleted");
    }
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", CONVERSATION])]
    );
}

/// HX-15f EXIT TEST: NOTHING IS TYPED WHILE THE KEPT CHILD LIVES (design 556-557: two vendor
/// processes never run one conversation). The kernel refuses the watchdog's `SIGTERM` to the
/// kept child, which goes on reading at its birth: the tab is handed over, and each attempt of
/// the relaunch, after its back-off (1 s, then 10 s), finds the child alive and types nothing,
/// until the tab no longer stands handed over (a claim placed and gone meanwhile).
/// NEGATIVE HALF: the same claimant's child that is read gone is relaunched over.
#[test]
fn nothing_is_typed_while_the_kept_child_lives() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let records = || vec![keeping(), session(&["--resume", CONVERSATION], &[])];
    let running = harness(&rig, "state", records());
    let ended = gone(&rig, running);
    let mut deaf = kernel();
    deaf.refuses = true;
    meanwhile(&rig, 2, move || {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unsupervised = None;
    });
    assert!(matches!(
        end(&rig, &ended, &mut deaf),
        Ending::Ended {
            child: Some(Err(_))
        }
    ));
    assert_eq!(deaf.sent.len(), 1, "one SIGTERM, refused");
    assert!(
        typed(&rig).is_empty(),
        "the kept child still reads at its birth"
    );
    assert_eq!(
        paused(&rig),
        [Duration::from_secs(1), Duration::from_secs(10)],
        "looked at again after each pause"
    );
    *rig.service
        .meanwhile
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;

    // NEGATIVE: the child ends, and the relaunch follows.
    let running = harness(&rig, "state", records());
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", CONVERSATION])]
    );
}

/// The `prompt` record aterm offers its stream, saying `text`.
fn prompt(text: &str) -> String {
    Record {
        seq: 1,
        ms: 0,
        body: Body::Prompt {
            text: text.to_string(),
            by: alab_wire::record::By::Person,
        },
    }
    .line()
    .expect("a prompt line")
}

/// Offer `offered` (kind, line) to [`ONE`]'s open stream and wake its lane; return once its lane
/// took them all (a hang detector).
fn offer(rig: &Rig, offered: &[(&'static str, String)]) {
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    {
        let mut meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        for (kind, line) in offered {
            assert!(
                meta.offer_stream_record(*kind, line.clone(), crate::metrics::now_us()),
                "the stream is open"
            );
        }
    }
    ctx.watchers.ring_stream();
    aterm_spec::wait::until_within(SETTLE, "the lane takes what was offered", || {
        rig.state(ONE).offered.is_empty().then_some(())
    });
}

/// The prompts `stream` heard of aterm's, up to the acknowledgement of a title sent as `seq`:
/// every record the lane wrote before that acknowledgement.
fn prompts_heard(stream: &mut Client, seq: u64) -> Vec<String> {
    assert_eq!(stream.record(seq, title()), (seq, None));
    let heard: Vec<Record> = stream.heard.drain(..).collect();
    assert!(
        heard
            .iter()
            .all(|record| !matches!(record.body, Body::Touch { .. })),
        "{heard:?}"
    );
    heard
        .into_iter()
        .filter_map(|record| match record.body {
            Body::Prompt { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

/// HX-15f EXIT TEST: AN OPTIONAL KIND IS SENT ONLY WHEN THE LATEST `session` LISTS IT (D27).
/// aterm's `prompt`, `answer`, `touch`, `interrupt` and `set` go to a stream only where the
/// claimant's latest `session` takes their kind: with no `session` an offered `prompt` is not
/// sent; once a `session` takes `prompt`, a `prompt` is sent and a `touch` beside it is not;
/// once a later `session` takes nothing, no `prompt` is sent again.
/// NEGATIVE HALF: the `prompt` the `session` takes arrives, numbered as aterm's next record.
#[test]
fn an_optional_kind_is_sent_only_when_the_latest_session_lists_it() {
    let rig = Rig::new(2);
    let touch = Record {
        seq: 1,
        ms: 0,
        body: Body::Touch { ask: 1 },
    }
    .line()
    .expect("a touch line");
    let mut running = harness(&rig, "state", Vec::new());
    let stream = &mut running.stream;

    offer(&rig, &[("prompt", prompt("before any session"))]);
    assert!(prompts_heard(stream, 1).is_empty(), "no session, no prompt");

    assert_eq!(
        stream.record(2, session(&["--resume", CONVERSATION], &[WireKind::Prompt])),
        (2, None)
    );
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let proven = rig.state(ONE).proven;
    assert!(
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .session_takes(&proven, "prompt")
    );
    let sent = rig.state(ONE).sent;
    offer(
        &rig,
        &[("touch", touch.clone()), ("prompt", prompt("taken"))],
    );
    let own = stream.own();
    assert_eq!(
        (own.seq, own.body),
        (
            sent + 1,
            Body::Prompt {
                text: "taken".to_string(),
                by: alab_wire::record::By::Person,
            }
        ),
        "the prompt, not the touch before it"
    );
    assert!(prompts_heard(stream, 3).is_empty());

    assert_eq!(stream.record(4, session(&[], &[])), (4, None));
    offer(&rig, &[("prompt", prompt("after the latest session"))]);
    assert!(
        prompts_heard(stream, 5).is_empty(),
        "the latest session takes no prompt"
    );
}

/// From the `at`-th pause of a relaunch on (counted from 1, as this is called), run `act` at
/// each: what a person or another ending does on [`ONE`] while the relaunch waits.
fn meanwhile(rig: &Rig, at: usize, mut act: impl FnMut() + Send + 'static) {
    let mut seen = 0;
    *rig.service
        .meanwhile
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(move || {
        seen += 1;
        if seen >= at {
            act();
        }
    }));
}

/// The directory each relaunch handed the typer since the last look.
fn typed_cwd(rig: &Rig) -> Vec<Option<String>> {
    std::mem::take(
        &mut *rig
            .service
            .typed_cwd
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    )
}

/// HX-15f REVIEW: A `session` WITH NO RESUME NAMES NO CONVERSATION (D22). A harness that sent
/// `session` before its vendor had a conversation (`resume: []`) and never another: the
/// relaunch resumes from the kept child's snapshot, in the snapshot's directory, and never types
/// `<tool> run --`, a fresh launch that would silently start another conversation. With no
/// snapshot either, nothing is typed.
/// NEGATIVE HALF: a `session` that names its resume wins over the snapshot, in the `session`'s
/// directory; the validation refuses a line with no resume word, and one out of the grammar.
#[test]
fn a_session_with_no_resume_names_none_and_the_snapshot_resumes() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let running = harness(&rig, "state", vec![keeping(), session(&[], &[])]);
    let snapshot = |_: &str, _: u32| {
        Some(Resume {
            words: vec!["--resume".to_string(), "of-the-child".to_string()],
            cwd: Some("/srv/child".to_string()),
        })
    };
    assert!(take_snapshot(
        &ctx.meta,
        ONE,
        kept_child(&rig),
        &snapshot,
        &mut kernel()
    ));
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", "of-the-child"])]
    );
    assert_eq!(typed_cwd(&rig), [Some("/srv/child".to_string())]);
    assert_eq!(paused(&rig), [Duration::from_secs(1)]);

    // No snapshot (no kept child) and a `session` with no resume: nothing is typed.
    let running = harness(&rig, "state", vec![session(&[], &[])]);
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert!(typed(&rig).is_empty(), "a fresh launch is never typed");
    assert!(paused(&rig).is_empty());
    assert!(unsupervised(&rig));

    // NEGATIVE: a `session` naming its resume wins, in its own directory.
    let running = harness(
        &rig,
        "state",
        vec![keeping(), session(&["--resume", CONVERSATION], &[])],
    );
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", CONVERSATION])]
    );
    assert_eq!(typed_cwd(&rig), [Some(PROJECT.to_string())]);
    let tool = tool("claude-code-alab");
    let words = |rest: &[&str]| {
        std::iter::once(tool.clone())
            .chain(rest.iter().map(ToString::to_string))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        validate_run(&words(&["run", "--"]), &tool),
        Err(RunRefusal::NoResume)
    );
    assert_eq!(
        validate_run(&words(&["run", "--model", "opus", "--"]), &tool),
        Err(RunRefusal::NoResume)
    );
    assert!(matches!(
        validate_run(&words(&["run", "--bogus", "x", "--", "y"]), &tool),
        Err(RunRefusal::Grammar(_))
    ));
    assert!(matches!(
        validate_run(&words(&["run", "--resume", "x"]), &tool),
        Err(RunRefusal::Grammar(_))
    ));
}

/// HX-15f REVIEW: A RELAUNCH THAT TYPED NOTHING IS TRIED AGAIN ON THE LADDER (alab-wire's
/// `relaunch::Ladder`). The typer reads no shell in front of the tab at the first attempt (a
/// straggler of the dead harness leads its group): the relaunch waits the ladder's next step
/// (10 s) and types the line at the second.
/// NEGATIVE HALF: a line typed at once is typed once, after 1 s.
#[test]
fn a_relaunch_that_typed_nothing_is_tried_again_on_the_ladder() {
    let rig = Rig::new(2);
    let records = || vec![session(&["--resume", CONVERSATION], &[])];
    let running = harness(&rig, "state", records());
    rig.service
        .typer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push_back(aterm_agent::harness::relaunch_type::RunLineRefusal::Unread(
            "not-a-shell",
        ));
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    let resumed = line("claude-code-alab", &[], &["--resume", CONVERSATION]);
    assert_eq!(typed(&rig), [resumed.clone(), resumed.clone()]);
    assert_eq!(
        paused(&rig),
        [Duration::from_secs(1), Duration::from_secs(10)]
    );

    // NEGATIVE: typed at the first attempt (the ladder, forgiven by a healthy run, starts again).
    let healthy = u64::try_from(alab_wire::relaunch::HEALTHY.as_millis()).expect("ms");
    rig.service.clock.fetch_add(healthy, Ordering::AcqRel);
    let running = harness(&rig, "state", records());
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(typed(&rig), [resumed]);
    assert_eq!(paused(&rig), [Duration::from_secs(1)]);
}

/// HX-15f REVIEW: THE RELAUNCH RESUMES WHAT THE KEPT CHILD RUNS AS ITS HARNESS ENDS (D22). The
/// snapshot read as the claim kept the child proved no conversation yet; the watchdog reads it
/// again as it ends the harness, while the child still reads at its birth, and the relaunch
/// resumes that. A child whose conversation moved since its claim (the person resumed another)
/// is resumed on the one it runs at the end.
/// NEGATIVE HALF: an end-time read that proves no conversation leaves the claim-time snapshot
/// standing.
#[test]
fn the_relaunch_resumes_what_the_kept_child_runs_as_its_harness_ends() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let at_end = |rig: &Rig, resumed: Option<Resume>| {
        *rig.service
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = resumed;
    };

    // No conversation when the claim kept the child.
    let running = harness(&rig, "state", vec![keeping()]);
    let none = |_: &str, _: u32| None;
    assert!(!take_snapshot(
        &ctx.meta,
        ONE,
        kept_child(&rig),
        &none,
        &mut kernel()
    ));
    at_end(&rig, Some(resume(&["--resume", "begun-since"])));
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", "begun-since"])]
    );
    assert_eq!(typed_cwd(&rig), [Some(PROJECT.to_string())]);

    // A conversation the person left since the claim.
    let running = harness(&rig, "state", vec![keeping_child(NEXT_CHILD)]);
    let left = |_: &str, _: u32| Some(resume(&["--resume", "left-since"]));
    assert!(take_snapshot(
        &ctx.meta,
        ONE,
        kept_child(&rig),
        &left,
        &mut Kernel::reading(NEXT_CHILD.pid, NEXT_CHILD.start)
    ));
    at_end(&rig, Some(resume(&["--resume", "runs-now"])));
    let ended = gone(&rig, running);
    end(
        &rig,
        &ended,
        &mut Kernel::reading(NEXT_CHILD.pid, NEXT_CHILD.start),
    );
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", "runs-now"])]
    );

    // NEGATIVE: the end-time read proves none; the claim-time snapshot stands.
    let running = harness(&rig, "state", vec![keeping()]);
    let kept = |_: &str, _: u32| Some(resume(&["--resume", "kept-at-claim"]));
    assert!(take_snapshot(
        &ctx.meta,
        ONE,
        kept_child(&rig),
        &kept,
        &mut kernel()
    ));
    at_end(&rig, None);
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line(
            "claude-code-alab",
            &[],
            &["--resume", "kept-at-claim"]
        )]
    );
}

/// HX-15f REVIEW: A CLAIM WHOSE CHILD aterm DID NOT KEEP IS NOT RELAUNCHED (design 556-557). The
/// claim names a child aterm's reads do not keep, and the claimant's `session` names its resume:
/// nothing reads that vendor gone, so the watchdog's ending types nothing and waits out nothing;
/// the tab stays `unsupervised`.
/// NEGATIVE HALF: a claim that names no child is relaunched from its `session`.
#[test]
fn a_claim_whose_child_aterm_did_not_keep_is_not_relaunched() {
    let rig = Rig::new(2);
    let unkept = ChildProcess {
        pid: UNKEPT,
        start: CHILD.start,
    };
    let records = |child: Option<ChildProcess>| {
        child
            .map(keeping_child)
            .into_iter()
            .chain([session(&["--resume", CONVERSATION], &[])])
            .collect()
    };
    let running = harness(&rig, "state", records(Some(unkept)));
    let ended = gone(&rig, running);
    assert_eq!(ended.1.claim.child.map(|word| word.pid), Some(UNKEPT));
    assert_eq!(ended.1.child, None, "aterm kept no child");
    assert!(matches!(
        end(&rig, &ended, &mut kernel()),
        Ending::Ended { child: None }
    ));
    assert!(typed(&rig).is_empty(), "nothing reads that vendor gone");
    assert!(paused(&rig).is_empty());
    assert!(unsupervised(&rig));

    // NEGATIVE: no child named.
    let running = harness(&rig, "state", records(None));
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", CONVERSATION])]
    );
}

/// HX-15f REVIEW: A MODEL RESTART THE HARNESS OBEYED RIDES NO RELAUNCH (D21). `restart
/// model=opus` is written for the claimant, which obeys: it restarts its child and claims again.
/// Its later crash relaunches it with no `--model`, which would override a model the person
/// chose since.
/// NEGATIVE HALF: the same restart, never obeyed, rides the relaunch.
#[test]
fn a_model_restart_the_harness_obeyed_rides_no_relaunch() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let resume = ["--resume", CONVERSATION];
    let mut running = harness(
        &rig,
        "state,relaunch",
        vec![keeping(), session(&resume, &[])],
    );
    assert!(crate::restart_act::record_scoped_restart(
        &ctx,
        "model",
        None,
        Some("opus")
    ));
    running.send(keeping_child(NEXT_CHILD));
    let ended = gone(&rig, running);
    end(
        &rig,
        &ended,
        &mut Kernel::reading(NEXT_CHILD.pid, NEXT_CHILD.start),
    );
    assert_eq!(typed(&rig), [line("claude-code-alab", &[], &resume)]);

    // NEGATIVE: never obeyed.
    let running = harness(&rig, "state,relaunch", vec![keeping()]);
    assert!(crate::restart_act::record_scoped_restart(
        &ctx,
        "model",
        None,
        Some("opus")
    ));
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &["--model", "opus"], &resume)]
    );
}

/// HX-15f REVIEW: A RELAUNCH WHOSE HANDOVER WAS SUPERSEDED TYPES NOTHING. While the relaunch
/// waits its back-off, a claim is placed on the tab and gone again (the person ran the harness
/// themselves and quit it): the tab no longer stands handed over by this ending, so nothing is
/// typed into the person's shell. Likewise where another ending handed the tab over meanwhile:
/// that ending's relaunch is the tab's. The attempt counted on the session as its wait started,
/// so an ending that overlaps it backs off on it.
/// NEGATIVE HALF: with nothing happening meanwhile, the line is typed.
#[test]
fn a_relaunch_whose_handover_was_superseded_types_nothing() {
    let rig = Rig::new(2);
    let records = || vec![session(&["--resume", CONVERSATION], &[])];
    let meta = |rig: &Rig| Arc::clone(&rig.service.handle(ONE).ctx.meta);

    // A claim placed and gone again meanwhile.
    let running = harness(&rig, "state", records());
    let ended = gone(&rig, running);
    let seen = Arc::new(Mutex::new(None));
    let (lock, streak) = (meta(&rig), Arc::clone(&seen));
    meanwhile(&rig, 1, move || {
        let mut meta = lock.lock().unwrap_or_else(PoisonError::into_inner);
        *streak.lock().unwrap_or_else(PoisonError::into_inner) = Some(meta.relaunch.streak);
        meta.unsupervised = None;
    });
    end(&rig, &ended, &mut kernel());
    assert!(typed(&rig).is_empty(), "the person's shell is left alone");
    assert_eq!(paused(&rig), [Duration::from_secs(1)]);
    assert_eq!(
        *seen.lock().unwrap_or_else(PoisonError::into_inner),
        Some(1),
        "the attempt counted as its wait started"
    );

    // Another ending's handover meanwhile.
    let running = harness(&rig, "state", records());
    let ended = gone(&rig, running);
    let lock = meta(&rig);
    meanwhile(&rig, 1, move || {
        lock.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unsupervised = Some(Unsupervised {
            at_us: crate::metrics::now_us(),
            holder: "claude-code-alab@1".to_string(),
        });
    });
    end(&rig, &ended, &mut kernel());
    assert!(typed(&rig).is_empty(), "the other ending relaunches");
    assert_eq!(paused(&rig).len(), 1);

    // NEGATIVE: nothing meanwhile.
    *rig.service
        .meanwhile
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
    let running = harness(&rig, "state", records());
    let ended = gone(&rig, running);
    end(&rig, &ended, &mut kernel());
    assert_eq!(
        typed(&rig),
        [line("claude-code-alab", &[], &["--resume", CONVERSATION])]
    );
}

/// HX-15f REVIEW: A KEPT CHILD'S SNAPSHOT IS TAKEN AS ITS CLAIM KEEPS IT AND AT EACH CHANGE OF
/// CHILD (D22), through the claim's own placement ([`crate::harness_watchdog::child_kept`]): the
/// session's children are read through a stand-in reader, and a claim record keeping [`CHILD`]
/// leaves its snapshot on the session; the harness's claim keeping [`NEXT_CHILD`] replaces it
/// with that child's.
/// NEGATIVE HALF: a renewal naming the same child reads nothing again.
#[test]
fn a_kept_childs_snapshot_is_taken_as_its_claim_keeps_it_and_at_each_change_of_child() {
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let reads = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&reads);
    crate::harness_watchdog::tap::reads(
        &ctx.meta,
        Arc::new(move |sid: &str, pid: u32| {
            assert_eq!(sid, ONE);
            counted.fetch_add(1, Ordering::AcqRel);
            Some(resume(&["--resume", &format!("of-{pid}")]))
        }),
    );
    let snapshot = |pid: u32| {
        aterm_spec::wait::until_within(SETTLE, "the kept child's snapshot", || {
            ctx.meta
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .relaunch
                .snapshot
                .clone()
                .filter(|(child, _)| child.pid == pid)
        })
    };
    let mut running = harness(&rig, "state", vec![keeping()]);
    let (child, resumed) = snapshot(CHILD.pid);
    assert_eq!(child, kept_child(&rig));
    assert_eq!(
        resumed.words,
        ["--resume", format!("of-{}", CHILD.pid).as_str()]
    );
    assert_eq!(reads.load(Ordering::Acquire), 1);

    running.send(keeping_child(NEXT_CHILD));
    let (_, resumed) = snapshot(NEXT_CHILD.pid);
    assert_eq!(
        resumed.words,
        ["--resume", format!("of-{}", NEXT_CHILD.pid).as_str()]
    );
    assert_eq!(reads.load(Ordering::Acquire), 2);

    // NEGATIVE: the same child again reads nothing.
    running.send(keeping_child(NEXT_CHILD));
    assert_eq!(snapshot(NEXT_CHILD.pid).0.pid, NEXT_CHILD.pid);
    assert_eq!(reads.load(Ordering::Acquire), 2);
}
