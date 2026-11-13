// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn frame(wall: f64, rows: &str, i: usize) -> Frame {
    Frame {
        wall,
        t: Some(1),
        seq: i64::try_from(i).unwrap(),
        rows: rows.lines().map(str::to_string).collect(),
        i,
    }
}

#[test]
fn the_idle_a_decision_lets_go_on_is_found_journaled_with_it() {
    use std::time::{Duration, Instant};
    let owned = aterm_tempfile::tempdir().expect("owned scratch");
    let path = std::fs::canonicalize(owned.path())
        .expect("a physical scratch")
        .join("journal");
    let line = |kind: &str, line: &str| {
        format!(
            "{}\n",
            J::obj([("t", J::Int(1)), ("kind", J::s(kind)), ("line", J::s(line))]).py()
        )
    };
    std::fs::write(&path, line("event", "EVENT idle seq=1")).expect("a journal");
    let mut ctx = Ctx::new("t", 1, "lab", "", LabKw::default(), true, false);
    ctx.jr = Some(Journal::start(path.clone(), None));
    let held = |ctx: &Ctx, n: usize| {
        let by = Instant::now() + Duration::from_secs(60);
        while ctx.journal().len() < n {
            assert!(Instant::now() < by, "the tail never held {n} line(s)");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    held(&ctx, 1);
    let start = |ctx: &mut Ctx| -> R {
        let both = format!(
            "{}{}{}",
            line("event", "EVENT idle seq=1"),
            line("chose", "CHOSE seq=2 rule=answer-recommended@v1"),
            line("event", "EVENT idle seq=3 ⏺ Done.")
        );
        let mut journal = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("the journal");
        std::io::Write::write_all(&mut journal, both.as_bytes()).expect("appended");
        held(ctx, 4);
        Ok(())
    };
    corpus_decision(&mut ctx, start, "CHOSE", 60.0).expect("not interrupted");
    assert_eq!(ctx.sumd.get("error"), None, "{}", ctx.sumd.py());
    assert_eq!(ctx.sumd.get("decided").and_then(J::as_str), Some("CHOSE"));
}

#[test]
fn the_survey_arm_returns_only_once_claude_code_reported_a_reread() {
    use crate::settings_witness::{HANG, LOG, REWRITE_EVERY, SteppedWait, Wait as _};
    use std::cell::Cell;
    let owned = aterm_tempfile::tempdir().expect("owned scratch");
    let w = std::fs::canonicalize(owned.path()).expect("a physical scratch");
    for (what, answers, same_file) in [
        ("reread", true, true),
        ("no line", false, true),
        ("another file", true, false),
    ] {
        let run = Run {
            w: w.join(what),
            ..Run::default()
        };
        std::fs::create_dir_all(run.w.join("home/.claude")).expect("the settings' dir");
        std::fs::write(run.settings(), "{}\n").expect("the settings");
        let settings = run.settings().to_string_lossy().into_owned();
        let names = if same_file {
            settings.clone()
        } else {
            run.w
                .join("proj/.claude/settings.json")
                .to_string_lossy()
                .into_owned()
        };
        let log = run.w.join(LOG);
        let pauses = Cell::new(0);
        let wait = SteppedWait::new(REWRITE_EVERY / 3, || {
            let settings = J::parse(&crate::util::scratch_text(&run.settings()))
                .expect("the settings just written");
            assert_eq!(
                settings
                    .get("env")
                    .and_then(|env| env.get("CLAUDE_FORCE_DISPLAY_SURVEY")),
                Some(&J::s("1")),
                "the actual writer set the flag before pausing"
            );
            pauses.set(pauses.get() + 1);
            if answers && pauses.get() == 4 {
                let line = J::obj([
                    ("hook_event_name", J::s("ConfigChange")),
                    ("file_path", J::s(&names)),
                ]);
                std::fs::write(&log, format!("{}\n", line.py())).expect("the witness's log");
            }
        });
        let mut ctx = Ctx::new("t", 1, "lab", "", LabKw::default(), true, false);
        ctx.run = run.clone();
        let armed = ctx.arm_with_wait(&wait);
        if answers && same_file {
            armed.expect("armed");
            let logged = crate::util::scratch_text(&log);
            assert_eq!(
                crate::settings_witness::rereads(&logged, &settings),
                1,
                "the line was in the log when the arm returned: {logged:?}"
            );
            let writes = ctx.sumd.get("arm_writes").and_then(J::as_i64).unwrap_or(0);
            assert_eq!(
                writes,
                2,
                "the missed write was made again: {}",
                ctx.sumd.py()
            );
            assert!(wait.elapsed() < HANG);
            continue;
        }
        let made = HANG.as_millis() / REWRITE_EVERY.as_millis();
        match armed {
            Err(ScnErr::Failed(m)) => assert!(
                m.starts_with("the survey flag was not armed: ")
                    && m.contains("no re-read")
                    && m.contains(&format!("({made} writes)")),
                "{what}: one write every REWRITE_EVERY until HANG: {m}"
            ),
            other => panic!("{what}: {other:?}"),
        }
        assert_eq!(ctx.sumd.get("arm_writes"), None, "{what}");
        assert_eq!(
            crate::settings_witness::rereads(&crate::util::scratch_text(&log), &names),
            usize::from(answers),
            "{what}: the controlled watcher supplied exactly its intended evidence"
        );
        assert_eq!(
            wait.elapsed(),
            HANG,
            "{what}: the simulated hang detector ended the wait"
        );
    }
}

#[test]
fn the_journal_goes_before_whether_it_holds_a_dismissal() {
    let mut ctx = Ctx::new("t", 1, "lab", "", LabKw::default(), true, false);
    put_journal(
        &mut ctx,
        J::Arr(vec![J::Arr(vec![
            J::Float(0.4),
            J::s("dismissed"),
            J::s("DISMISSED survey seq=1"),
        ])]),
    );
    let J::Obj(m) = &ctx.sumd else {
        panic!("summary is an object")
    };
    let keys: Vec<&str> = m.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["journal", "dismissed_journal"]);
    assert_eq!(ctx.sumd.get("dismissed_journal"), Some(&J::Bool(true)));
}

#[test]
fn the_cycle_is_measured_from_frames_and_journal_and_judged() {
    let rule = "─".repeat(50);
    let card = format!("● How is Claude doing this session? (optional)\n{rule}\n❯\u{a0}\n{rule}");
    let zero = format!("● How is Claude doing this session? (optional)\n{rule}\n❯\u{a0}0\n{rule}");
    let after = format!("✻ Worked for 0s · done 12:16 PM\n{rule}\n❯\u{a0}\n{rule}");
    let frames = vec![
        frame(10.5, "✻ Worked for 0s · done 12:16 PM", 0),
        frame(12.5, &card, 1),
        frame(14.3, &zero, 2),
        frame(14.7, &after, 3),
    ];
    let journal = vec![(
        15.3,
        J::parse(r#"{"t":15200,"kind":"dismissed","line":"DISMISSED survey seq=1"}"#).unwrap(),
    )];
    let d = cycle_stats(&frames, &journal, 10.0);
    assert_eq!(d.get("idle_to_card"), Some(&J::Float(2.0)));
    assert_eq!(d.get("card_to_zero"), Some(&J::Float(1.8)));
    assert_eq!(d.get("zero_to_gone"), Some(&J::Float(0.4)));
    assert_eq!(d.get("gone_to_journal"), Some(&J::Float(0.5)));
    assert_eq!(
        d.get("composer_states").unwrap().dump(crate::pyjson::PY),
        r#"["", "0", ""]"#
    );
    assert!(judge_cycle(&d).is_empty());
    let no_card = cycle_stats(&frames[..1], &[], 10.0);
    assert_eq!(
        judge_cycle(&no_card),
        vec!["no survey card was drawn".to_string()]
    );
}

#[test]
fn a_frame_line_says_what_changed_in_python_repr() {
    let f = frame(1.0, "Feedback: Bad · Esc to undo", 7);
    assert_eq!(
        frame_line(&f),
        "FRAME seq=7 T=1 card=0 composer=None status='' rows=['Feedback: Bad · Esc to undo']"
    );
}

/// A RUN'S OWN LAB IS MADE WITH THE RUN'S KEYS AND WORDS: its `[harness]` lines in place of the
/// caller's, and its instance's words after the caller's `LAB_INSTANCE_ENV`, so the
/// occupancy scenario's lab reads `prompt` and the nudge's seam at its minute. NEGATIVE
/// CONTROL: a run with no words of its own leaves the caller's instance words as they were.
#[test]
fn a_runs_own_lab_gets_its_keys_and_its_instance_words() {
    use aterm_agent::supervise::occupancy::rules::NUDGE_SEAM;
    let caller = || UpOpts {
        harness_keys: "continue = false".into(),
        instance_env: "TMPDIR=/s".into(),
        ..UpOpts::default()
    };
    let listed = scenarios()
        .into_iter()
        .find(|s| s.name == crate::occupancy_break::NAME)
        .expect("the occupancy scenario is listed");
    let o = lab_opts(caller(), listed.harness_keys, listed.kw);
    assert!(o.trust && o.lifeline);
    assert_eq!(o.harness_keys, crate::occupancy_break::HARNESS_KEYS);
    assert_eq!(
        crate::lab::instance_env(&o.instance_env),
        Ok(vec![
            ("TMPDIR".to_string(), "/s".to_string()),
            (
                NUDGE_SEAM.to_string(),
                crate::occupancy_break::NUDGE_S.to_string()
            ),
        ])
    );
    let alone = lab_opts(UpOpts::default(), listed.harness_keys, listed.kw);
    assert_eq!(alone.instance_env, crate::occupancy_break::INSTANCE_ENV);
    let plain = lab_opts(caller(), "", LabKw::default());
    assert_eq!(plain.instance_env, "TMPDIR=/s");
    assert_eq!(plain.harness_keys, "");
}

/// C3-WALL WAITS FOR THE OUTAGE'S ONE RESUME AT ITS FIRST RUNG (v6 §14.17.5.2): its lab runs
/// the ladders at the forever lab's eighth, so the rung is 15 s and the wait a minute past it,
/// on `outage-resume@v1` rather than main's typed retry. NEGATIVE CONTROL: c1-approval's lab
/// adds no words of its own.
#[test]
fn c3_wall_waits_for_the_outages_resume_at_its_first_rung() {
    let listed = |name: &str| {
        scenarios()
            .into_iter()
            .find(|s| s.name == name)
            .expect("the scenario is listed")
    };
    let wall = listed("c3-wall");
    assert_eq!(wall.kw.instance_env, crate::autopilot_scenes::INSTANCE_ENV);
    assert_eq!(wall.kw.instance_env, crate::forever::ladder_env());
    assert_eq!(listed("c1-approval").kw.instance_env, "");
    assert_eq!(wall_resumed(), "rule=outage-resume@v1");
    assert!((wall_resume_s() - 75.0).abs() < f64::EPSILON);
}

/// A RUN THAT ASKS ITS LAB FOR KEYS OR WORDS REFUSES A LAB IT ATTACHES TO, which was made
/// without them. NEGATIVE CONTROL: a run that asks for none (`watch`, `catch`, the nudge
/// trials) attaches.
#[test]
fn a_run_that_asks_its_lab_for_keys_refuses_an_attached_one() {
    let listed = scenarios()
        .into_iter()
        .find(|s| s.name == crate::occupancy_break::NAME)
        .expect("the occupancy scenario is listed");
    assert!(unset_on_attach(listed.harness_keys, listed.kw).is_some());
    let words = LabKw {
        instance_env: crate::occupancy_break::INSTANCE_ENV,
        ..LabKw::default()
    };
    assert!(unset_on_attach("", words).is_some());
    assert!(unset_on_attach("continue = false", LabKw::default()).is_some());
    let standin = LabKw {
        standin: true,
        ..LabKw::default()
    };
    assert!(unset_on_attach("", standin).is_some());
    assert_eq!(unset_on_attach("", LabKw::default()), None);
}
