// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_rewind_verb_parses_its_forms_and_refuses_the_rest() {
    for (rest, want) in [
        ("", Asked::Status),
        (" status ", Asked::Status),
        ("open", Asked::Open(Target::Latest)),
        ("open ago=42", Asked::Open(Target::Ago(42))),
        ("open at=1200", Asked::Open(Target::At(Ticks(1200)))),
        ("step -1", Asked::Step(StepBy::Slice(-1))),
        ("step +3", Asked::Step(StepBy::Slice(3))),
        ("step 2", Asked::Step(StepBy::Slice(2))),
        ("step -5 by=sec", Asked::Step(StepBy::Secs(-5))),
        ("step +1 by=cmd", Asked::Step(StepBy::Cmd(1))),
        ("changed 17,41", Asked::Changed(17, 41)),
        ("text", Asked::Text { trim: false }),
        ("text trim", Asked::Text { trim: true }),
        ("close", Asked::Close),
        ("forget", Asked::Forget { all: false }),
        ("forget all", Asked::Forget { all: true }),
    ] {
        assert_eq!(parse(rest), Ok(want), "{rest:?}");
    }
    for rest in [
        "open ago=",
        "open ago=-1",
        "open when=3",
        "open ago=1 at=2",
        "step",
        "step x",
        "step 1 by=min",
        "step by=sec",
        "changed",
        "changed 3",
        "changed -1,2",
        "changed 1,2,3",
        "text all",
        "close now",
        "forget now",
        "forget all now",
        "export now",
        "rewind",
        "status extra",
    ] {
        assert_eq!(parse(rest), Err(USAGE.to_string()), "{rest:?}");
    }
}

/// `rewind check` first judges every pending witness of every kept chain ([`crate::temporal::judge_all`]), so none is left pending, and its digest check answers the line the chains checked before any was judged.
#[test]
fn rewind_check_judges_before_its_digest_check() {
    use crate::rewind::tests::{slice, start_recording, witness};
    use crate::temporal::keyframe_of;
    let app = crate::scripted_redraw_fixture::scripted_app();
    start_recording(&app);
    let s = app.pool.get(0).expect("session 0");
    let _ = slice(&app, b"one\r\n");
    witness(&app, &crate::term_lock(s.live_term()));
    {
        let term = crate::term_lock(s.live_term());
        let mut rec = s.ctx.temporal.lock().expect("recorder");
        rec.record_keyframe(keyframe_of(&term, usize::MAX).expect("keyable"));
    }
    let _ = slice(&app, b"two\r\n");
    witness(&app, &crate::term_lock(s.live_term()));
    let (pending, before) = {
        let rec = s.ctx.temporal.lock().expect("recorder");
        let build = aterm_stamp::build_id();
        let checks: Vec<PaneCheck> = rec
            .pane_chains()
            .expect("chains")
            .iter()
            .map(|chain| chain.check(PaneCode::ThisBuild, &build))
            .collect();
        (rec.pending(), check_line(&checks))
    };
    assert_eq!(pending, 2, "fixture: a pending witness in each chain");
    assert!(
        before.starts_with("chains=2 ") && before.contains(" verdict=pass "),
        "fixture: {before}"
    );
    assert_eq!(check_verb(&s.ctx, false), format!("OK {before}\n"));
    let rec = s.ctx.temporal.lock().expect("recorder");
    assert_eq!(
        (rec.pending(), rec.witnessed(), rec.diverged()),
        (0, 2, 0),
        "every pending witness judged"
    );
}
