// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::supervise::fx::plant_clock_read;
use crate::supervise::tape::acts::Candidate;
use crate::supervise::tape::doors;
use crate::supervise::tape::replay::{Options, replay};
use crate::supervise::tape::report::{ActSide, Cause, Class, Mode, Report, Verdict};

/// The scripted worker: a Read outside every allowed root, a read-only Bash
/// box, then the session gone.
fn script() -> Mock {
    let mut m = fenced(vec![
        busy_screen(),
        read_box(),
        busy_screen(),
        bash_one_row(),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(3);
    m
}

/// The engine tests' options under `approve = "all"`.
fn approve_all() -> SuperviseOpts {
    let mut opts = auto(30);
    opts.policy.approve = Approve::All;
    opts
}

/// A `/` home the Read rule reads `~` against on every host.
fn owner_home(s: &mut Session<'_, TapedCtl<'_, Mock>>) {
    s.set_approval_env(ApprovalEnv {
        home: Some(PathBuf::from("/Users/_owner")),
        ..ApprovalEnv::of_process()
    });
}

/// A run of [`script`] recorded under `approve = "all"`: its tape, and the
/// keys the scripted worker was pressed.
fn recorded() -> (Vec<u8>, Vec<String>) {
    let (bytes, keys, _) = recorded_under_header(|h| h.to_string());
    (bytes, keys)
}

/// [`recorded`] with its header's text made by `header` from the real one,
/// which is handed back too.
fn recorded_under_header(header: impl Fn(&str) -> String) -> (Vec<u8>, Vec<String>, String) {
    let mut m = script();
    let run = record_watch(&mut m, &approve_all(), owner_home);
    let keys: Vec<String> = run.sent.iter().filter(|r| is_press(r)).cloned().collect();
    let text = run.header.encode();
    (run.tape.reframed(&header(&text)), keys, text)
}

/// Whether `request`, as the scripted worker took it, is a press: its verb,
/// after any `@session` address, is `key`.
fn is_press(request: &str) -> bool {
    request.split(' ').find(|w| !w.starts_with('@')) == Some("key")
}

/// A candidate of `sets` over the recorded policy.
fn candidate(sets: &[(&str, &str)]) -> Candidate {
    under(sets).candidate.expect("a candidate")
}

/// The shadow of `bytes` under `sets`.
fn shadowed(bytes: &[u8], sets: &[(&str, &str)]) -> crate::supervise::tape::shadow::Shadow {
    crate::supervise::tape::shadow::shadow(bytes, &candidate(sets))
        .unwrap_or_else(|e| panic!("the tape shadows: {e}"))
}

/// A counterfactual under the policy the recorded one becomes with `sets`.
fn under(sets: &[(&str, &str)]) -> Options {
    Options {
        candidate: Some(Candidate {
            file: None,
            sets: sets
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }),
        ..Options::default()
    }
}

/// The tape `bytes` replayed with `options`.
fn replayed(bytes: &[u8], options: &Options) -> Report {
    replay(bytes, options).unwrap_or_else(|e| panic!("the tape does not replay: {e}"))
}

/// Both sides of the first divergent act, when the verdict is one.
fn divergent_act(report: &Report) -> Option<(Option<&ActSide>, Option<&ActSide>)> {
    match &report.verdict {
        Verdict::Diverged(d) => match &d.cause {
            Cause::ActDiffers { recorded, asked } => Some((recorded.as_ref(), asked.as_ref())),
            _ => None,
        },
        _ => None,
    }
}

/// WOULD IT HAVE PRESSED UNDER "SAFE"? The run recorded under "all" pressed
/// both boxes; replayed under `approve = "safe"`, the candidate takes another
/// act at the first press — it escalates the Read instead — and the replay
/// stops there: DIVERGED, a decision, the recorded press and the candidate's
/// badge both said, and the decision before each.
#[test]
fn a_safe_policy_diverges_at_the_press_all_took() {
    let (bytes, keys) = recorded();
    assert_eq!(
        keys.len(),
        2,
        "the recorded run pressed both boxes: {keys:?}"
    );
    let report = replayed(&bytes, &under(&[("approve", "safe")]));
    assert_eq!(report.mode, Mode::Acts);
    let Some((Some(recorded), Some(asked))) = divergent_act(&report) else {
        panic!("a divergent act, both sides: {}", report.render());
    };
    assert!(
        recorded.words.split(' ').any(|w| w == "key"),
        "the recorded act is the press: {recorded:?}"
    );
    assert!(
        asked
            .words
            .starts_with("meta set attention owner=supervisor claude read: ~/.ssh/config"),
        "the candidate's act is the Read's badge: {asked:?}"
    );
    let Verdict::Diverged(d) = &report.verdict else {
        unreachable!()
    };
    assert_eq!((d.class, report.code()), (Class::Decision, 1));
    assert_eq!(Some(d.at), recorded.at, "it diverged at the recorded press");
    let acts = report.acts.as_ref().expect("a counterfactual's account");
    assert_eq!(
        acts.candidate,
        ["approve=safe".to_string()],
        "the candidate said"
    );
    assert_eq!(acts.account.taken, 0, "no act taken before the press");
    let approval = |decided: &[crate::supervise::tape::report::Decided]| {
        decided
            .iter()
            .find(|d| d.door == doors::DECIDE_APPROVAL)
            .cloned()
    };
    let candidate = approval(&acts.candidate_decisions)
        .unwrap_or_else(|| panic!("the candidate decided the box: {}", report.render()));
    assert!(
        candidate.outcome.starts_with("Escalate"),
        "the candidate escalated: {candidate:?}"
    );
    let recorded_decision = approval(&acts.recorded_decisions)
        .unwrap_or_else(|| panic!("the recorded decision: {}", report.render()));
    assert!(
        recorded_decision.outcome.starts_with("Approve"),
        "the recorded run approved: {recorded_decision:?}"
    );
    assert!(
        acts.last_screen
            .as_ref()
            .is_some_and(|rows| rows.iter().any(|r| r.contains(".ssh/config"))),
        "the last screen the candidate read is the Read's box: {:?}",
        acts.last_screen
    );
}

/// THE NEGATIVE CONTROL: a candidate that changes only what this run never
/// consults (the continuation's text, with the continuation off) takes every
/// act the recorded run took, reads exactly what it read: REPRODUCED.
#[test]
fn a_key_the_run_never_consults_reproduces() {
    let (bytes, _) = recorded();
    let report = replayed(&bytes, &under(&[("harness.continue_text", "keep at it")]));
    assert_eq!(report.verdict, Verdict::Reproduced, "{}", report.render());
    let acts = report.acts.as_ref().expect("a counterfactual's account");
    assert_eq!((acts.account.taken, acts.account.acts), (2, 2));
    assert!(acts.account.exact(), "{:?}", acts.account);
}

/// A key the policy's reader refuses is no counterfactual: the replay is
/// refused, naming it, rather than run under the recorded policy.
#[test]
fn a_candidate_the_reader_refuses_is_no_replay() {
    let (bytes, _) = recorded();
    for sets in [&[("approve", "maybe")][..], &[("no_such_key", "1")]] {
        let err = replay(&bytes, &under(sets)).expect_err("refused");
        assert!(err.contains(sets[0].0), "{sets:?}: {err}");
    }
}

/// A BUILD THAT READS THE CLOCK MORE OFTEN (a planted variant of the loop:
/// one clock read more before each screen) still takes every act the
/// recorded run took — REPRODUCED in acts mode, its extra reads served by
/// the virtual clock — while a strict replay drifts at its first extra read.
#[test]
fn an_extra_clock_read_passes_acts_and_drifts_strict() {
    let (bytes, _) = recorded();
    plant_clock_read(true);
    let strict = replay(&bytes, &Options::default());
    let acts = replay(&bytes, &under(&[]));
    plant_clock_read(false);
    let strict = strict.expect("a strict replay");
    match &strict.verdict {
        Verdict::Diverged(d) => assert_eq!(d.class, Class::Drift, "{}", strict.render()),
        other => panic!("the strict replay drifts: {other:?}"),
    }
    let acts = acts.expect("a counterfactual");
    assert_eq!(acts.verdict, Verdict::Reproduced, "{}", acts.render());
    let account = &acts.acts.as_ref().expect("an account").account;
    assert!(account.virtual_clock > 0, "{account:?}");
    assert_eq!(account.taken, 2, "{account:?}");
}

/// THE HOSTED LOOP'S TAPE — what `aterm drive replay` replays in a shipped
/// build: a hosted run under the default policy ("all") pressed the Read;
/// under `approve = "safe"` the candidate escalates it instead, and the
/// replay stops at the press.
#[test]
fn a_hosted_tape_diverges_under_safe_at_the_press() {
    let dir = crate::supervise::test_scratch_path("ledger", "counterfactual-hosted");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let ledger = dir.join("s-1.jsonl");
    let mut m = fenced(vec![busy_screen(), read_box(), busy_screen()]);
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            answer_questions: false,
            ..SupervisorConfig::default()
        },
        ..auto(30)
    };
    let (r, _, bytes) = host_stops_taped(&mut m, Some("@s-1"), ledger, &opts, false);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r, Ok(()));
    assert!(
        m.requests.iter().any(|q| q.contains(" key ")),
        "the hosted run pressed the Read: {:#?}",
        m.requests
    );
    let report = replayed(&bytes, &under(&[("approve", "safe")]));
    assert_eq!(report.entry, "hosted");
    let Some((Some(recorded), Some(asked))) = divergent_act(&report) else {
        panic!("a divergent act, both sides: {}", report.render());
    };
    assert!(recorded.words.contains(" key "), "{recorded:?}");
    assert!(
        asked.words.contains("meta set attention owner=supervisor"),
        "{asked:?}"
    );
}

/// THE SHADOW UNDER "SAFE" LISTS EXACTLY THE AFFECTED BOXES: every decision
/// on the tape is decided again, and the one point the candidate decides
/// otherwise is the Read "all" pressed — the read-only Bash box after it is
/// decided the same, and labelled TAINT, as every row after the first
/// difference is; the turn ends, decided again in a strict replay on this
/// (the recording) build, are the same.
#[test]
fn the_shadow_lists_exactly_the_boxes_safe_decides_otherwise() {
    let (bytes, _) = recorded();
    let shadow = shadowed(&bytes, &[("approve", "safe")]);
    assert_eq!(shadow.code(), 1, "{}", shadow.render());
    let otherwise: Vec<&crate::supervise::tape::shadow::ShadowRow> = shadow
        .rows
        .iter()
        .filter(|r| r.differs == Some(true))
        .collect();
    assert_eq!(otherwise.len(), 1, "{}", shadow.render());
    let read = otherwise[0];
    assert_eq!(read.door, doors::DECIDE_APPROVAL);
    assert!(read.recorded.starts_with("Approve"), "{read:?}");
    assert!(
        read.candidate
            .as_deref()
            .is_some_and(|c| c.starts_with("Escalate")),
        "{read:?}"
    );
    assert!(read.needs.is_empty(), "{read:?}");
    let first = shadow
        .rows
        .iter()
        .position(|r| r.differs == Some(true))
        .expect("a difference");
    for (at, row) in shadow.rows.iter().enumerate() {
        assert_eq!(row.taint, at > first, "row {at}: {row:?}");
        assert!(
            row.differs.is_some(),
            "every point decided again on this build: {row:?}"
        );
    }
    let bash = shadow
        .rows
        .iter()
        .filter(|r| r.door == doors::DECIDE_APPROVAL)
        .nth(1)
        .expect("the Bash box's row");
    assert_eq!((bash.differs, bash.taint), (Some(false), true), "{bash:?}");
    assert!(
        shadow.rows.iter().any(|r| r.door == doors::DECIDE_TURN_END),
        "{}",
        shadow.render()
    );
}

/// THE SHADOW'S NEGATIVE CONTROLS: under the recorded policy itself, and
/// under a key the run never consults, every point is decided again and
/// decided as recorded — exit 0, no row TAINT.
#[test]
fn the_shadow_under_an_unchanged_policy_decides_every_point_as_recorded() {
    let (bytes, _) = recorded();
    for sets in [&[][..], &[("continue_text", "keep at it")]] {
        let shadow = shadowed(&bytes, sets);
        assert_eq!(shadow.code(), 0, "{sets:?}: {}", shadow.render());
        assert!(
            shadow
                .rows
                .iter()
                .all(|r| r.differs == Some(false) && !r.taint),
            "{sets:?}: {}",
            shadow.render()
        );
    }
}

/// ON ANOTHER BUILD the boxes are still decided again — from their marks'
/// facts, no loop run — while each turn end is listed with why it is not:
/// its fold is rebuilt only by a strict replay on the recording build.
#[test]
fn another_builds_shadow_decides_the_boxes_and_names_why_not_the_turn_ends() {
    let (bytes, _, _) = recorded_under_header(|text| {
        text.lines()
            .map(|l| {
                if l.starts_with("build=") {
                    "build=9.9.0+gelsewhere".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    });
    let shadow = shadowed(&bytes, &[("approve", "safe")]);
    assert_eq!(shadow.recorded_by.as_deref(), Some("9.9.0+gelsewhere"));
    for row in &shadow.rows {
        if row.door == doors::DECIDE_TURN_END {
            assert_eq!(row.differs, None, "{row:?}");
            assert!(
                row.note
                    .as_deref()
                    .is_some_and(|n| n.contains("9.9.0+gelsewhere")),
                "{row:?}"
            );
        } else {
            assert!(row.differs.is_some(), "{row:?}");
        }
    }
    assert_eq!(shadow.differs(), 1, "{}", shadow.render());
}

/// A TURN END THE CANDIDATE DECIDES OTHERWISE: a run that typed the usage
/// limit's continuation (`resume_limits`) shadowed with the switch off — the
/// turn end is decided again beside the replayed loop, on the fold it
/// rebuilt, and decided otherwise there, by another rule — and replayed in
/// acts mode, it stops at the continuation the recorded run typed.
#[test]
fn a_turn_end_the_candidate_decides_otherwise_is_shadowed_and_stops_the_acts() {
    let mut m = Mock::new(vec![
        busy_screen(),
        weekly_limited(),
        switched(),
        busy_screen(),
        answered("keep going"),
    ]);
    m.vanish_after = Some(2);
    m.turn_releases = Some(2);
    let run = record_watch(&mut m, &resuming(&auto(3 * 86_400), None), |_| {});
    let bytes = run.tape.reframed(&run.header.encode());
    let shadow = shadowed(&bytes, &[("resume_limits", "false")]);
    let resumed: Vec<&crate::supervise::tape::shadow::ShadowRow> = shadow
        .rows
        .iter()
        .filter(|r| r.recorded_rule.as_deref() == Some("usage-resume@v1"))
        .collect();
    assert_eq!(resumed.len(), 1, "{}", shadow.render());
    let row = resumed[0];
    assert_eq!(row.door, doors::DECIDE_TURN_END);
    assert_eq!(row.differs, Some(true), "{}", shadow.render());
    assert_ne!(
        row.candidate_rule.as_deref(),
        Some("usage-resume@v1"),
        "{row:?}"
    );
    let report = replayed(&bytes, &under(&[("resume_limits", "false")]));
    let Some((Some(recorded), Some(asked))) = divergent_act(&report) else {
        panic!("the continuation is the divergent act: {}", report.render());
    };
    assert!(
        recorded.words.starts_with("turn ") && recorded.words.ends_with("keep going"),
        "{recorded:?}"
    );
    assert!(
        asked
            .words
            .starts_with("meta set attention owner=supervisor"),
        "the candidate hands the wall to a person instead: {asked:?}"
    );
}

/// The act a counterfactual's report names as the recorded run's where the
/// candidate did not take it: a divergent act's recorded side, the act it
/// went on without, or the act it stopped short of.
fn recorded_act(report: &Report) -> Option<&ActSide> {
    match &report.verdict {
        Verdict::Diverged(d) | Verdict::Stale(d) => match &d.cause {
            Cause::ActDiffers { recorded, .. } => recorded.as_ref(),
            Cause::ActSkipped { recorded, .. } => Some(recorded),
            _ => report.acts.as_ref()?.recorded_next.as_ref(),
        },
        _ => None,
    }
}

/// A turn that ended after 3 minutes of work, the worker's last words
/// `said`, at an empty composer in bypass mode: a point `continue@v1` types
/// its continuation at.
fn worked_turn(said: &str) -> Vec<String> {
    let mut r = rows(&[
        &format!("⏺ {said}"),
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// AUTOPILOT OFF WHERE THE RECORDED RUN PRESSED: the candidate parks — it
/// gives its claim back, a release the recorded run made only at its end —
/// and never presses.
#[test]
fn a_candidate_that_parks_diverges_at_the_press_it_never_takes() {
    let (bytes, _) = recorded();
    let report = replayed(&bytes, &under(&[("autopilot", "off")]));
    assert_eq!(report.code(), 1, "{}", report.render());
    let Verdict::Diverged(d) = &report.verdict else {
        unreachable!()
    };
    assert_eq!(d.class, Class::Decision, "{}", report.render());
    let press = recorded_act(&report)
        .unwrap_or_else(|| panic!("the recorded press named: {}", report.render()));
    assert!(
        press.words.split(' ').any(|w| w == "key"),
        "the recorded act is the press: {press:?}"
    );
    assert_eq!(Some(d.at), press.at, "it diverged at the recorded press");
    let acts = report.acts.as_ref().expect("a counterfactual's account");
    assert_eq!(acts.account.taken, 0, "{}", report.render());
    assert!(
        acts.recorded_decisions
            .iter()
            .any(|d| d.door == doors::DECIDE_APPROVAL && d.outcome.starts_with("Approve")),
        "the recorded decision named: {}",
        report.render()
    );
}

/// The same on the hosted loop's tape: under Autopilot off the candidate
/// gives its claim back and parks where the hosted run pressed the Read, and
/// the replay stops at that press — never STALE at the claim's release.
#[test]
fn a_hosted_candidate_that_parks_diverges_at_the_press() {
    let dir = crate::supervise::test_scratch_path("ledger", "counterfactual-parks");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let ledger = dir.join("s-1.jsonl");
    let mut m = fenced(vec![busy_screen(), read_box(), busy_screen()]);
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            answer_questions: false,
            ..SupervisorConfig::default()
        },
        ..auto(30)
    };
    let (r, _, bytes) = host_stops_taped(&mut m, Some("@s-1"), ledger, &opts, false);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r, Ok(()));
    let report = replayed(&bytes, &under(&[("autopilot", "off")]));
    assert_eq!(report.code(), 1, "{}", report.render());
    let press = recorded_act(&report)
        .unwrap_or_else(|| panic!("the recorded press named: {}", report.render()));
    assert!(press.words.contains(" key "), "{press:?}");
}

/// THE CONTINUATION SWITCHED OFF: a run that typed Cruise's continuation
/// (`config::CONTINUE_TEXT`, `[aterm harness] Autopilot is Cruise: keep
/// going.
#[test]
fn a_candidate_that_skips_the_continuation_diverges_at_it() {
    let mut m = Mock::new(vec![
        busy_screen(),
        worked_turn("Fixed the parser; the suite is green."),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30)
    };
    let run = record_watch(&mut m, &opts, |_| {});
    assert!(
        run.lines
            .iter()
            .any(|l| l.starts_with("CONTINUED seq=102 rule=continue@v1")),
        "{:#?}",
        run.lines
    );
    let bytes = run.tape.reframed(&run.header.encode());
    assert_eq!(
        replayed(&bytes, &Options::default()).verdict,
        Verdict::Reproduced
    );
    let report = replayed(&bytes, &under(&[("continue", "false")]));
    assert_eq!(report.code(), 1, "{}", report.render());
    let Verdict::Diverged(d) = &report.verdict else {
        unreachable!()
    };
    assert_eq!(d.class, Class::Decision, "{}", report.render());
    let turn = recorded_act(&report)
        .unwrap_or_else(|| panic!("the continuation named: {}", report.render()));
    assert!(
        turn.words.starts_with("turn ")
            && turn
                .words
                .ends_with(crate::supervise::config::CONTINUE_TEXT),
        "{turn:?}"
    );
    assert_eq!(Some(d.at), turn.at);
    let acts = report.acts.as_ref().expect("a counterfactual's account");
    assert!(
        acts.recorded_decisions
            .iter()
            .any(|d| d.rule.as_deref() == Some("continue@v1")),
        "the recorded decision named: {}",
        report.render()
    );
}

/// [`recorded`] with its items edited by `edit`: the run as its tape would
/// read had it gone otherwise (a press it did not make, an end it never
/// wrote).
fn recorded_edited(edit: impl FnOnce(&mut Vec<crate::supervise::tape::codec::Item>)) -> Vec<u8> {
    let (bytes, _) = recorded();
    let read = crate::supervise::tape::recover::recover(&bytes);
    let header = read.header.expect("the tape's header");
    let mut items = read.items;
    edit(&mut items);
    crate::supervise::tape::testing::written(&header, &items)
}

/// Drop the last key press on a tape's items (the read-only Bash box's).
fn without_the_last_press(items: &mut Vec<crate::supervise::tape::codec::Item>) {
    let press = last_press(items);
    items.remove(press);
}

/// Where a tape's last key press is among its items.
fn last_press(items: &[crate::supervise::tape::codec::Item]) -> usize {
    items
        .iter()
        .rposition(|i| {
            matches!(i.record(), Some(astream_effects::EffectRecord::Exchange { door, request, .. })
                if door == doors::CTL
                    && crate::supervise::tape::codec::ctl_args(request)
                        .is_ok_and(|w| w.iter().any(|w| w == "key")))
        })
        .expect("a press")
}

/// Drop a tape's end (a run killed, or still running).
fn without_the_end(items: &mut Vec<crate::supervise::tape::codec::Item>) {
    items.retain(|i| !matches!(i, crate::supervise::tape::codec::Item::End(_)));
}

/// A TAPE WITH NO END in acts mode: the recorded run stands for one that did
/// not press the Bash box and went on running (its press removed, and then
/// its end).
#[test]
fn a_tape_with_no_end_is_ended_only_when_spent() {
    for tape in [
        recorded_edited(without_the_last_press),
        recorded_edited(|items| {
            without_the_last_press(items);
            without_the_end(items);
        }),
    ] {
        let report = replayed(&tape, &under(&[("continue_text", "keep at it")]));
        assert_eq!(report.code(), 1, "{}", report.render());
        let Some((None, Some(asked))) = divergent_act(&report) else {
            panic!("an act past the recorded run's last: {}", report.render());
        };
        assert!(
            asked.words.contains("git"),
            "the Bash box's press: {asked:?}"
        );
        assert!(report.consumed < report.effects, "{}", report.render());
    }
    let killed = recorded_edited(|items| {
        let press = last_press(items);
        items.truncate(press + 1);
    });
    let report = replayed(&killed, &under(&[]));
    assert_eq!(report.verdict, Verdict::Ended, "{}", report.render());
    let acts = report.acts.as_ref().expect("an account");
    assert_eq!((acts.account.taken, acts.account.acts), (2, 2));
}

/// THE SHADOW SAYS WHAT IT COULD NOT DECIDE: on another build, under the
/// recorded policy, every box is decided again and as recorded, but no turn
/// end is — so the exit is 3 (the tape cannot say there), never the 0 of
/// "every point decided as recorded".
#[test]
fn a_shadow_that_could_not_decide_every_point_exits_3() {
    let (bytes, _, _) = recorded_under_header(|text| {
        text.lines()
            .map(|l| {
                if l.starts_with("build=") {
                    "build=9.9.0+gelsewhere".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    });
    let shadow = shadowed(&bytes, &[]);
    assert_eq!(shadow.differs(), 0, "{}", shadow.render());
    assert!(
        shadow.rows.iter().any(|r| r.differs.is_none()),
        "{}",
        shadow.render()
    );
    assert_eq!(shadow.code(), 3, "{}", shadow.render());
    let (bytes, _) = recorded();
    assert_eq!(shadowed(&bytes, &[]).code(), 0);

    let read: Vec<String> = read_box()
        .into_iter()
        .map(|row| row.replace("~/.ssh/config", "/outside/project/readme.txt"))
        .collect();
    let mut m = fenced(vec![busy_screen(), read, busy_screen(), idle_screen()]);
    m.vanish_after = Some(2);
    let run = record_watch(&mut m, &approve_all(), owner_home);
    assert!(
        run.sent.iter().any(|q| is_press(q)),
        "the recorded run pressed: {:#?}",
        run.sent
    );
    let bytes = run.tape.reframed(&run.header.encode());
    let tape = crate::supervise::tape::recover::recover(&bytes);
    assert!(
        !tape.items.iter().any(|item| matches!(item.record(),
            Some(astream_effects::EffectRecord::Exchange { door, .. })
                if door == doors::PROBE_REAL_COMPONENTS)),
        "the recording contains no result for the candidate's physical lookup"
    );
    let unknown = shadowed(
        &bytes,
        &[("approve", "safe"), ("trust_roots", "/outside/project")],
    );
    let row = unknown
        .rows
        .iter()
        .find(|r| r.door == doors::DECIDE_APPROVAL)
        .expect("the read's approval point");
    assert_eq!(row.differs, None, "{row:?}");
    assert!(row.candidate.is_none() && row.note.is_some(), "{row:?}");
    assert!(
        row.needs
            .iter()
            .any(|n| n.contains(doors::PROBE_REAL_COMPONENTS)),
        "{row:?}"
    );
    assert_eq!(unknown.differs(), 0);
    assert_eq!(unknown.code(), 3);
    assert!(unknown.rows.iter().all(|row| !row.taint));
    assert_eq!(
        shadowed(&bytes, &[]).code(),
        0,
        "the recorded policy is known"
    );
}

/// `--shadow` OVER SEVERAL TAPES says each one: a tape that does not shadow
/// (no decision on it) is named under its own heading with why, and the
/// others' reports are kept — the exit the worst of them (a point decided
/// otherwise first, then a tape that did not shadow).
#[test]
fn a_shadow_batch_keeps_every_tapes_report() {
    let dir = crate::supervise::ScratchDir::new("drive-replay", "shadow-batch");
    let with = dir.join("with.atape");
    let without = dir.join("without.atape");
    let (bytes, _) = recorded();
    std::fs::write(&with, &bytes).expect("written");
    let first_effect = recorded_edited(|items| {
        let first = items
            .iter()
            .position(|i| i.record().is_some())
            .expect("an effect");
        items.truncate(first + 1);
    });
    std::fs::write(&without, &first_effect).expect("written");
    let (with, without) = (
        with.to_string_lossy().into_owned(),
        without.to_string_lossy().into_owned(),
    );
    let args = |line: &[&str]| line.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let one = crate::drive_cli::replay_reply(&args(&[&with, "--shadow", "--set", "approve=safe"]))
        .expect("shadowed");
    assert_eq!(one.code, 1, "{}", one.text);
    let both = crate::drive_cli::replay_reply(&args(&[
        &with,
        &without,
        "--shadow",
        "--set",
        "approve=safe",
    ]))
    .expect("a batch is a report");
    assert_eq!(both.code, 1, "{}", both.text);
    assert!(both.text.contains(&one.text), "{}", both.text);
    assert!(
        both.text.contains("no decisions on this tape"),
        "{}",
        both.text
    );
    let failed = crate::drive_cli::replay_reply(&args(&[&without, &with, "--shadow"]))
        .expect("a batch is a report");
    assert_eq!(failed.code, 2, "{}", failed.text);
    assert!(failed.text.contains("SHADOW"), "{}", failed.text);
}
