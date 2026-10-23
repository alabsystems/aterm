// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use astream_effects::{EffectRecord, ReplayDivergence};

use crate::supervise::run::approval_loop::PANIC_AT_DECIDE;
use crate::supervise::tape::codec::{End, Item, ctl_args};
use crate::supervise::tape::doors;
use crate::supervise::tape::recover::recover;
use crate::supervise::tape::replay::{Options, ReplayCore, panic_message, replay};
use crate::supervise::tape::report::{Cause, Class, Divergence, Report, Verdict};
use crate::supervise::tape::testing::{MemoryTape, assert_replays, written};

/// The scripted worker: a read box approved, a question and an idle point
/// handed over, then the session gone (the transparency suite's script).
fn script() -> Mock {
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

/// A recorded watch of [`script`]: its header's text and its tape's items,
/// after checking it replays as recorded.
fn recorded() -> (String, Vec<Item>) {
    let mut m = script();
    let run = record_watch(&mut m, &auto(30), |_| {});
    let report = assert_replays(&run.tape, &run.header, &run.sent, &run.lines);
    assert_eq!(report.verdict, Verdict::Reproduced);
    let header = run.header.encode();
    let read = recover(&run.tape.reframed(&header));
    (header, read.items)
}

/// A recorded `supervise` — a read box approved, a write box handed back to
/// the manager — as `aterm drive supervise --tape` records one: its header's
/// text, its tape's items and the review the run returned.
fn recorded_supervise() -> (String, Vec<Item>, String) {
    let mut m = fenced(vec![bash_one_row(), busy_screen(), write_prompt()]);
    let (tape, mut taped) = memory_tape(&mut m);
    let mut s = taped_session(&mut taped, &tape);
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let opts = auto(30);
    let header = crate::supervise::tape::header::Header::capture(
        crate::supervise::tape::testing::TAPE_ID,
        crate::supervise::tape::header::Entry::Supervise,
        None,
        s.setup(),
        &opts,
    );
    tape.core().reroot(header.roots.clone());
    let end = s.supervise_to(&opts, &mut std::io::sink());
    drop(s);
    drop(taped);
    tape.finish(&crate::supervise::tape::replay::supervise_end(&end));
    let (review, code) = end.expect("supervise");
    assert_eq!(code, 0);
    let header = header.encode();
    let read = recover(&tape.reframed(&header));
    (header, read.items, review)
}

/// The tape's end as it said the run returned.
fn returned(items: &[Item]) -> &str {
    items
        .iter()
        .find_map(|i| match i {
            Item::End(End::Returned { how }) => Some(how.as_str()),
            _ => None,
        })
        .expect("the run returned")
}

/// The tape of `items` under `header`, replayed.
fn replayed(header: &str, items: &[Item]) -> Report {
    replay(&written(header, items), &Options::default()).expect("the edited tape replays")
}

/// The first divergence of `report`, which must have diverged.
fn diverged(report: &Report) -> &Divergence {
    match &report.verdict {
        Verdict::Diverged(d) => d,
        other => panic!("a divergence, not {other:?}:\n{}", report.render()),
    }
}

/// The ordinal of each effect among the items (`#N`), with its index in them.
fn effects(items: &[Item]) -> Vec<(usize, u64, &EffectRecord)> {
    let mut n = 0u64;
    items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let r = item.record()?;
            n += 1;
            Some((i, n - 1, r))
        })
        .collect()
}

/// The control request a record carries, as words, when it is one.
fn request_of(record: &EffectRecord) -> Option<Vec<String>> {
    match record {
        EffectRecord::Exchange { door, request, .. } if door == doors::CTL => {
            ctl_args(request).ok()
        }
        _ => None,
    }
}

/// The record at item `i`, edited by `edit`.
fn edit(items: &mut [Item], i: usize, edit: impl FnOnce(&mut EffectRecord)) {
    match &mut items[i] {
        Item::Effect { record, .. } => edit(record),
        other => panic!("not an effect: {other:?}"),
    }
}

/// A RECORDED WATCH REPLAYS: the run's own tape is REPRODUCED, every effect
/// consumed, the run's end the recorded one — in its file form and in its
/// text form alike.
#[test]
fn a_recorded_watch_replays_in_either_form() {
    let (header, items) = recorded();
    let report = replayed(&header, &items);
    assert_eq!(report.verdict, Verdict::Reproduced, "{}", report.render());
    assert_eq!(report.consumed, report.effects);
    assert_eq!(report.returned.as_deref(), Some("exit 1"));
    assert!(
        report
            .lines
            .iter()
            .any(|l| l.starts_with("APPROVED seq=102 "))
    );
    let rendered = report.render();
    let listed: Vec<&str> = rendered
        .lines()
        .filter_map(|l| l.strip_prefix("  > "))
        .collect();
    let acts: Vec<String> = report
        .requests
        .iter()
        .filter(|w| {
            let w: Vec<&str> = w.iter().map(String::as_str).collect();
            crate::supervise::tape::ctl::is_act(&w)
        })
        .map(|w| w.join(" "))
        .collect();
    assert_eq!(listed, acts, "{rendered}");
    assert!(
        listed.iter().any(|a| a.starts_with("key ")),
        "the box's press is listed:\n{rendered}"
    );
    assert!(acts.len() < report.requests.len(), "reads are not listed");
    let text = crate::supervise::tape::text::render(Some(&header), &items);
    let again = replay(text.as_bytes(), &Options::default()).expect("the text form replays");
    assert_eq!(again, report, "the text form replays as the file form");
}

/// A RECORDED SUPERVISE REPLAYS, ITS REVIEW COMPARED: a `supervise` returns
/// the review the manager reads as its value, not as lines through the `out`
/// door, so its tape's end holds that review under the exit code, and the
/// replay is REPRODUCED only when the replayed loop returns the very same
/// text — twice alike.
#[test]
fn a_recorded_supervise_replays_and_its_review_is_compared() {
    let (header, items, review) = recorded_supervise();
    assert!(review.starts_with("prompt\n"), "{review}");
    assert_eq!(
        returned(&items),
        format!("exit 0\n{review}"),
        "the tape's end holds the review the manager read"
    );
    let report = replayed(&header, &items);
    assert_eq!(report.verdict, Verdict::Reproduced, "{}", report.render());
    assert_eq!(report.entry, "supervise");
    assert_eq!(report.consumed, report.effects);
    assert_eq!(report.returned.as_deref(), Some(returned(&items)));
    assert_eq!(replayed(&header, &items), report, "replayed twice alike");
    assert!(
        report.render().contains("\n  = command rm -rf target\n"),
        "the report shows the review:\n{}",
        report.render()
    );

    let mut edited = items.clone();
    let end = edited
        .iter_mut()
        .find_map(|i| match i {
            Item::End(End::Returned { how }) => Some(how),
            _ => None,
        })
        .expect("the end");
    *end = end.replacen("rm -rf target", "rm -rf elsewhere", 1);
    let report = replayed(&header, &edited);
    let d = diverged(&report);
    assert_eq!(d.class, Class::Output, "{d:?}");
    assert!(
        matches!(&d.cause, Cause::OtherEnd { recorded, replayed }
            if recorded.contains("elsewhere") && replayed.contains("rm -rf target")),
        "{d:?}"
    );
    assert_eq!(report.consumed, report.effects, "every door agreed");
}

/// ONE REPLY BYTE FLIPPED: the reply's form byte (a control reply's
/// ok-or-error tag) makes a reply this build cannot read — DIVERGED at that
/// very door, as drift; a byte flipped in the JUDGED SCREEN (the box's
/// command) makes the loop decide on another screen — DIVERGED at the
/// decision, as a decision.
#[test]
fn one_reply_byte_flipped_diverges() {
    let (header, items) = recorded();
    let all = effects(&items);
    let (i, n, _) = *all
        .iter()
        .find(|(_, _, r)| request_of(r).is_some_and(|w| w[0] == "status"))
        .expect("a status read");
    let mut tagged = items.clone();
    edit(&mut tagged, i, |r| {
        if let EffectRecord::Exchange { reply, .. } = r {
            reply[0] = 9;
        }
    });
    let report = replayed(&header, &tagged);
    let d = diverged(&report);
    assert_eq!((d.at, d.door.as_deref()), (n, Some(doors::CTL)), "{d:?}");
    assert_eq!(d.class, Class::Drift);
    assert!(matches!(d.cause, Cause::Undecodable { .. }), "{d:?}");

    let (i, n, _) = *all
        .iter()
        .find(|(_, _, r)| {
            matches!(r, EffectRecord::Exchange { door, reply, .. }
                if door == doors::CTL && reply.windows(7).any(|w| w == b"git log"))
        })
        .expect("the box's read");
    let mut flipped = items.clone();
    edit(&mut flipped, i, |r| {
        if let EffectRecord::Exchange { reply, .. } = r {
            let at = reply
                .windows(7)
                .position(|w| w == b"git log")
                .expect("the command");
            reply[at] = b'j';
        }
    });
    let report = replayed(&header, &flipped);
    let d = diverged(&report);
    assert!(d.at > n, "after the read it was served: {d:?}");
    assert_eq!(d.class, Class::Decision, "{d:?}");
}

/// ONE MONO DROPPED: the loop's next monotonic read finds another kind of
/// record where its read was — astream's kind mismatch, at that place.
#[test]
fn one_mono_dropped_is_a_kind_mismatch() {
    let (header, items) = recorded();
    let all = effects(&items);
    let (i, n, _) = *all
        .iter()
        .filter(|(_, _, r)| matches!(r, EffectRecord::Mono(_)))
        .nth(3)
        .expect("a fourth monotonic read");
    let mut dropped = items.clone();
    dropped.remove(i);
    let report = replayed(&header, &dropped);
    let d = diverged(&report);
    assert_eq!(d.at, n, "{d:?}");
    assert!(
        matches!(
            &d.cause,
            Cause::Refused(ReplayDivergence::KindMismatch {
                expected: "Mono",
                ..
            })
        ),
        "{d:?}"
    );
    assert_eq!(d.class, Class::Drift);
}

/// TWO REQUESTS SWAPPED: the loop asks the first of them where the tape holds
/// the second — DIVERGED at the first, its request named on both sides.
#[test]
fn two_requests_swapped_diverge_at_the_first() {
    let (header, items) = recorded();
    let all = effects(&items);
    let reads: Vec<&(usize, u64, &EffectRecord)> = all
        .iter()
        .filter(|(_, _, r)| request_of(r).is_some_and(|w| w[0] == "status" || w[0] == "meta"))
        .collect();
    let (a, na, _) = *reads[0];
    let (b, _, _) = **reads
        .iter()
        .find(|(_, _, r)| request_of(r) != request_of(reads[0].2))
        .expect("another read");
    let mut swapped = items.clone();
    swapped.swap(a, b);
    let report = replayed(&header, &swapped);
    let d = diverged(&report);
    assert_eq!(d.at, na, "{d:?}");
    assert!(
        matches!(&d.cause, Cause::Refused(ReplayDivergence::RequestDiverged { door, .. }) if door == doors::CTL),
        "{d:?}"
    );
}

/// A PRINTED LINE CHANGED on the tape: the loop prints the line it prints,
/// the tape holds another — an OUTPUT divergence at the `out` door.
#[test]
fn a_printed_line_changed_is_an_output_divergence() {
    let (header, items) = recorded();
    let (i, n, _) = *effects(&items)
        .iter()
        .find(|(_, _, r)| matches!(r, EffectRecord::Exchange { door, .. } if door == doors::OUT))
        .expect("a printed line");
    let mut changed = items.clone();
    edit(&mut changed, i, |r| {
        if let EffectRecord::Exchange { request, .. } = r {
            *request = b"APPROVED seq=102 something else".to_vec();
        }
    });
    let report = replayed(&header, &changed);
    let d = diverged(&report);
    assert_eq!((d.at, d.door.as_deref()), (n, Some(doors::OUT)), "{d:?}");
    assert_eq!(d.class, Class::Output);
}

/// ONE HEADER KEY CHANGED: the policy that approved the box is replaced by
/// one that approves nothing — the loop decides otherwise on the very screen
/// the recorded loop approved: a DECISION divergence, at the decision.
#[test]
fn one_header_key_changed_is_a_decision_divergence() {
    let (header, items) = recorded();
    let approve = header
        .lines()
        .find(|l| l.starts_with("approve="))
        .expect("the approval policy's key");
    let changed = header.replacen(approve, "approve=none", 1);
    assert_ne!(changed, header, "{approve}");
    let report = replayed(&changed, &items);
    let d = diverged(&report);
    assert_eq!(d.class, Class::Decision, "{d:?}");
    assert_eq!(d.door.as_deref(), Some(doors::DECIDE_APPROVAL), "{d:?}");
}

/// A PANIC IN THE POLICY REPRODUCES: a run that panics where it decides a box
/// (a planted panic, this thread's) leaves a tape that ends unwound; its
/// replay panics at the same place, every record consumed — PANICKED, the
/// same message.
#[test]
fn a_recorded_panic_replays_to_panicked() {
    let mut m = script();
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())));
    let header;
    {
        let mut taped = TapedCtl::new(&mut m, tape.fx.clone(), doors::CTL);
        let mut s = session(&mut taped, None);
        s.set_fx(tape.fx.clone());
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let opts = auto(30);
        header = run_header(&s, &opts);
        tape.core().reroot(header.roots.clone());
        PANIC_AT_DECIDE.with(|p| p.set(true));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s.watch(&opts, &mut std::io::sink())
        }));
        PANIC_AT_DECIDE.with(|p| p.set(false));
        let message = panic_message(ran.expect_err("the planted panic").as_ref());
        tape.fx.finish(End::Unwound { message });
    }
    let bytes = tape.reframed(&header.encode());
    PANIC_AT_DECIDE.with(|p| p.set(true));
    let report = std::panic::catch_unwind(|| replay(&bytes, &Options::default()));
    PANIC_AT_DECIDE.with(|p| p.set(false));
    let report = report
        .expect("the replay catches the loop's panic")
        .expect("replays");
    assert_eq!(
        report.verdict,
        Verdict::Panicked {
            message: "a planted panic in the policy, at the decision on seq=102".to_string(),
            door: None,
        },
        "{}",
        report.render()
    );
    assert_eq!(report.consumed, report.effects);
    assert_eq!(report.code(), 4);
    let unplanted = replay(&bytes, &Options::default()).expect("replays");
    assert!(
        matches!(unplanted.verdict, Verdict::Diverged(_)),
        "{}",
        unplanted.render()
    );
}

/// The scripted worker's transport, which panics at its `at`-th call
/// (counting from 0) — inside the control door's live call, as a probe's body
/// or a real transport could.
struct PanicsAt<'m> {
    inner: &'m mut Mock,
    at: usize,
    calls: usize,
}

impl Ctl for PanicsAt<'_> {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let n = self.calls;
        self.calls += 1;
        if n == self.at {
            panic!("a planted panic in the transport, at request {n}");
        }
        self.inner.call(args)
    }
}

/// A PANIC INSIDE A DOOR REPRODUCES: a run whose transport panics inside the
/// control door's live call leaves no record of that round trip — there is no
/// value to tape — but its tape says the door and the request it died at
/// (`NOTE_UNWOUND_IN`); its replay, every record before it reproduced,
/// reaches that door asking that very request: PANICKED inside `ctl`, with
/// the recorded message, exit 4.
#[test]
fn a_panic_inside_a_door_replays_to_panicked_at_that_door() {
    use crate::supervise::tape::core::{NOTE_UNWOUND_IN, unwound_in_text};
    let mut m = script();
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())));
    let header;
    {
        let mut panics = PanicsAt {
            inner: &mut m,
            at: 6,
            calls: 0,
        };
        let mut taped = TapedCtl::new(&mut panics, tape.fx.clone(), doors::CTL);
        let mut s = session(&mut taped, None);
        s.set_fx(tape.fx.clone());
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let opts = auto(30);
        header = run_header(&s, &opts);
        tape.core().reroot(header.roots.clone());
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s.watch(&opts, &mut std::io::sink())
        }));
        let message = panic_message(ran.expect_err("the planted panic").as_ref());
        tape.fx.finish(End::Unwound { message });
    }
    let header = header.encode();
    let items = recover(&tape.reframed(&header)).items;
    let report = replayed(&header, &items);
    assert_eq!(
        report.verdict,
        Verdict::Panicked {
            message: "a planted panic in the transport, at request 6".to_string(),
            door: Some(doors::CTL.to_string()),
        },
        "{}",
        report.render()
    );
    assert_eq!(report.consumed, report.effects);
    assert_eq!(report.code(), 4);

    let note = items
        .iter()
        .position(|i| matches!(i, Item::Note { kind, .. } if kind == NOTE_UNWOUND_IN))
        .expect("the tape says where the run died");
    let mut other = items.clone();
    other[note] = Item::Note {
        kind: NOTE_UNWOUND_IN.to_string(),
        text: unwound_in_text(
            doors::CTL,
            &crate::supervise::tape::codec::ctl_request(&["nope"]),
        ),
    };
    let d = diverged(&replayed(&header, &other)).clone();
    assert!(
        matches!(d.cause, Cause::Refused(ReplayDivergence::Exhausted { .. })),
        "{d:?}"
    );
    let mut unsaid = items.clone();
    unsaid.remove(note);
    let d = diverged(&replayed(&header, &unsaid)).clone();
    assert!(
        matches!(d.cause, Cause::Refused(ReplayDivergence::Exhausted { .. })),
        "{d:?}"
    );
}

/// `--until door=N` stops the replay once effect `#N` is consumed: REACHED,
/// everything up to it reproduced, nothing after it served.
#[test]
fn until_stops_the_replay_after_its_door() {
    let (header, items) = recorded();
    let report = replay(
        &written(&header, &items),
        &Options {
            until: Some(7),
            ..Options::default()
        },
    )
    .expect("replays");
    assert_eq!(
        report.verdict,
        Verdict::Reached { door: 7 },
        "{}",
        report.render()
    );
    assert_eq!(report.consumed, 8);
    assert_eq!(report.code(), 0);
}

/// A TAPE CUT SHORT — a run killed mid-frame — reproduces to its tear: ENDED,
/// every effect on it consumed, the loop stopped where the tape stops.
#[test]
fn a_torn_tape_reproduces_to_its_tear() {
    let (header, items) = recorded();
    let bytes = written(&header, &items);
    let cut = &bytes[..bytes.len() - 5];
    let report = replay(cut, &Options::default()).expect("a torn tape replays");
    assert_eq!(report.verdict, Verdict::Ended, "{}", report.render());
    assert_eq!(report.consumed, report.effects);
    assert!(
        report.effects < effects(&items).len(),
        "the tear lost the last frame"
    );
    assert!(report.tail.is_some(), "the tear is said");
}

/// A REPLAY NEVER CALLS ITS TRANSPORT: the recorded tape replayed through the
/// recording's own wrapper over a transport that panics if it is called —
/// the replaying door never makes the live call — reproduces every effect.
#[test]
fn a_replaying_door_never_calls_the_transport() {
    struct Refuses;
    impl Ctl for Refuses {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            panic!("a replay called its transport: {args:?}")
        }
    }
    let (header, items) = recorded();
    let records: Vec<EffectRecord> = items.iter().filter_map(Item::record).cloned().collect();
    let total = records.len();
    let base = crate::supervise::test_scratch_path("replay", "absent");
    let decoded =
        crate::supervise::tape::header::Header::decode_remapped(&header, &base).expect("decodes");
    let recorded_roots = crate::supervise::tape::header::Header::decode(&header)
        .expect("decodes")
        .roots;
    let core = ReplayCore::start(
        records,
        decoded.roots.clone(),
        recorded_roots,
        &decoded.tape,
        None,
    );
    let fx = Fx::replay(Arc::clone(&core));
    let mut refuses = Refuses;
    let mut taped = TapedCtl::new(&mut refuses, fx.clone(), doors::CTL);
    let mut s = Session::from_setup(&mut taped, decoded.setup.clone());
    s.set_fx(fx);
    let code = s.watch(&decoded.opts, &mut std::io::sink());
    assert_eq!(code, 1);
    assert_eq!(
        core.consumed(),
        (total, total),
        "every effect, and no transport"
    );
}

/// The tapes no replay can stand on are refused, each named: bytes that are
/// no tape, and roots that would land on a directory that exists.
#[test]
fn a_tape_no_replay_can_stand_on_is_refused() {
    let (header, items) = recorded();
    assert!(replay(b"not a tape", &Options::default()).is_err());
    let dir = crate::supervise::ScratchDir::new("replay", "base");
    let err = replay(
        &written(&header, &items),
        &Options {
            base: Some(dir.to_path_buf()),
            ..Options::default()
        },
    )
    .expect_err("refused");
    assert!(err.contains("must not exist"), "{err}");
}

/// NEGATIVE CONTROL FOR THE HOST'S DOOR: a header edited to say the run had a
/// window host, over a tape whose run had none, is not refused — a run with
/// a host replays now — and DIVERGES at the host's first call: the replayed
/// loop tells its host the task's face as it starts (`host.task_face`),
/// where the tape holds the claim.
#[test]
fn a_host_the_tape_never_answered_diverges_at_its_first_call() {
    let (header, items) = recorded();
    let hosted = header.replacen("idle_host=false", "idle_host=true", 1);
    assert_ne!(hosted, header);
    let report = replay(&written(&hosted, &items), &Options::default()).expect("replayed");
    let Verdict::Diverged(d) = &report.verdict else {
        panic!("diverged: {report:?}");
    };
    assert_eq!(d.door.as_deref(), Some("host.task_face"), "{d:?}");
    assert!(
        matches!(
            d.cause,
            Cause::Refused(ReplayDivergence::KindMismatch { .. })
        ),
        "{d:?}"
    );
    assert!(report.requests.is_empty(), "nothing asked past it");
}

/// THE APPROVAL POLICY'S HOME IS A VALUE, AND ITS LOOKS THERE ARE THE TAPE'S
/// (`tape::header`'s module docs).
#[cfg(unix)]
#[test]
fn the_policys_home_is_a_value_and_its_looks_there_are_the_tapes() {
    let dir = crate::supervise::ScratchDir::new("replay", "home");
    let home = crate::supervise::test_canonical(&dir).join("owner");
    let file = home.join("proj").join("notes.md");
    std::fs::create_dir_all(home.join("proj")).expect("proj");
    std::fs::write(&file, "notes\n").expect("notes");
    let shown = file.to_str().expect("a UTF-8 scratch path").to_string();
    let read: Vec<String> = read_box()
        .into_iter()
        .map(|r| {
            if r == "   ~/.ssh/config" {
                format!("   {shown}")
            } else {
                r
            }
        })
        .collect();
    let mut m = fenced(vec![busy_screen(), read, busy_screen(), idle_screen()]);
    m.vanish_after = Some(3);
    let mut opts = auto(30);
    opts.policy.set("approve", "safe").expect("approve");
    opts.policy
        .set("trust_roots", "~/proj")
        .expect("trust_roots");
    let run = record_watch(&mut m, &opts, |s| {
        s.set_approval_env(ApprovalEnv {
            home: Some(home.clone()),
            uid: 502,
            tmpdir: None,
            worker: WorkerSource::Fixed(Ok(crate::supervise::policy::WorkerEnv::hermetic(None))),
        });
    });
    assert!(
        run.lines
            .iter()
            .any(|l| l.starts_with("APPROVED ") && l.ends_with(&shown)),
        "the live looks prove the Read: {:?}",
        run.lines
    );
    let header = run.header.encode();
    let items = recover(&run.tape.reframed(&header)).items;
    for door in [doors::PROBE_REAL_COMPONENTS, doors::PROBE_HIDDEN_TWIN] {
        let asked: Vec<&[u8]> = items
            .iter()
            .filter_map(|i| match i.record() {
                Some(EffectRecord::Exchange {
                    door: d, request, ..
                }) if d == door => Some(request.as_slice()),
                _ => None,
            })
            .collect();
        assert!(!asked.is_empty(), "{door} was looked at");
        assert!(
            asked.iter().all(|r| *r == b"@home/proj/notes.md"),
            "{door}: {:?}",
            asked
                .iter()
                .map(|r| String::from_utf8_lossy(r))
                .collect::<Vec<_>>()
        );
    }

    drop(dir);
    assert!(!home.exists(), "the home is gone before the replay");
    let report = replayed(&header, &items);
    assert_eq!(
        report.verdict,
        Verdict::Reproduced,
        "replayed from the tape alone:\n{}",
        report.render()
    );

    let base = Path::new("/nonexistent/aterm-replay-home/absent");
    let moved = crate::supervise::tape::header::Header::decode_remapped(&header, base)
        .expect("decodes remapped")
        .roots
        .iter()
        .find(|(id, _)| *id == "home")
        .map(|(_, dir)| dir.to_path_buf())
        .expect("a home root");
    let mut section = String::new();
    let remapped_home: String = header
        .lines()
        .map(|l| {
            if l.starts_with('[') {
                section = l.to_string();
            }
            if section == "[env]" && l.starts_with("home=") {
                format!("home={}\n", moved.display())
            } else {
                format!("{l}\n")
            }
        })
        .collect();
    assert_ne!(remapped_home, header);
    let report = replayed(&remapped_home, &items);
    let d = diverged(&report);
    assert_eq!(d.class, Class::Decision, "{d:?}");
    assert_eq!(d.door.as_deref(), Some(doors::DECIDE_APPROVAL), "{d:?}");
}
