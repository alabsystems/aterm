// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn v(s: &str) -> Vec<String> {
    s.lines().map(str::to_string).collect()
}

#[test]
fn screens_differ_by_the_rows_whose_text_differs() {
    assert_eq!(differing(&v("a\nb\nc"), &v("a\nB\nc\nd")), vec![1, 3]);
    assert_eq!(differing(&v("a  \nb"), &v("a\nb")), Vec::<usize>::new());
    assert_eq!(spans(&[0, 1, 2, 45, 46, 50]), "0-2,45-46,50");
}

#[test]
fn resizes_reads_its_header_and_epochs() {
    let (h, e) = parse_resizes(
        "epoch 1 shift=-1 pushed=0 class=agent verdict=healed by=nudge tried=1\n",
        "aterm-ctl: OK 1 total=1 runs=1 flaps=1 risks=0 render=ok unledgered=0 lost=0 nudge=idle nudged=1\n",
    );
    assert_eq!(hget(&h, "nudged").as_deref(), Some("1"));
    assert_eq!(e.len(), 1);
    assert_eq!(
        (e[0].id, e[0].shift, e[0].get("by")),
        (1, -1, Some("nudge"))
    );
}

#[test]
fn a_failed_reply_is_no_reading() {
    let reply = |code, stdout: &str, stderr: &str| Reply {
        code,
        stdout: stdout.into(),
        stderr: stderr.into(),
    };
    let head = "aterm-ctl: OK 0 total=0 render=ok nudge=idle nudged=2\n";
    let read = read_resizes(&reply(0, "", head));
    let (h, e) = read.expect("a header and no epochs is a reading");
    assert_eq!((hget(&h, "nudged").as_deref(), e.len()), (Some("2"), 0));
    for (code, err) in [(1, "aterm-ctl: ERR busy\n"), (0, ""), (1, head)] {
        let read = read_resizes(&reply(code, "", err));
        assert!(read.is_err(), "{code} {err:?}");
    }
    assert_eq!(screen_rows(&reply(0, "a\nb\n", "")).ok(), Some(v("a\nb")));
    for (code, out) in [(1, "a\n"), (0, "ERR busy\n"), (0, "\n\n")] {
        let rows = screen_rows(&reply(code, out, ""));
        assert!(rows.is_err(), "{code} {out:?}");
    }
}

#[test]
fn the_configured_nudge_is_the_render_tables_word() {
    assert_eq!(
        configured_nudge("[render]\nrepaint_nudge = \"Off\"\n").as_deref(),
        Some("off")
    );
    assert_eq!(
        configured_nudge(
            "[other]\nrepaint_nudge = \"always\"\n[render]\nrepaint_nudge = \"Off\"\n"
        )
        .as_deref(),
        Some("off")
    );
    assert_eq!(
        configured_nudge("[other]\nrepaint_nudge = \"Off\"\n"),
        None,
        "repaint_nudge outside [render] is ignored"
    );
    assert_eq!(configured_nudge("[update]\nenabled = false\n"), None);
}

fn healed_trial() -> Trial {
    let before = v("a\nb\nc");
    Trial {
        k: 1,
        via: "mode".into(),
        hold: true,
        rows: 3,
        cols: 10,
        shot: v("x\ny\nc"),
        after: before.clone(),
        fresh: before.clone(),
        before,
        head: vec![("render".into(), "ok".into())],
        status_render: "ok".into(),
        nudged: 1,
        epoch: Some(
            parse_resizes(
                "epoch 2 shift=-1 pushed=0 class=agent verdict=healed by=nudge tried=1",
                "",
            )
            .1
            .remove(0),
        ),
        log: vec![
            "repaint nudge hold rows=2 cols=10".into(),
            "healed by=nudge answer_ms=50".into(),
        ],
        told: vec!["2x10".into(), "3x10".into()],
        ..Trial::default()
    }
}

#[test]
fn a_healed_trial_passes_on_and_fails_off() {
    let mut m = healed_trial();
    assert_eq!(judge("on", &mut m), ("PASS".to_string(), Vec::new()));
    let (v, bad) = judge("off", &mut healed_trial());
    assert_eq!(v, "FAIL");
    assert!(bad.iter().any(|b| b.starts_with("the nudge acted")));
}

#[test]
fn a_trial_that_never_displaced_is_inconclusive() {
    let mut m = healed_trial();
    m.shot = m.before.clone();
    assert_eq!(judge("on", &mut m).0, "INCONCLUSIVE");
    let mut m = healed_trial();
    m.epoch = None;
    assert_eq!(judge("on", &mut m).0, "INCONCLUSIVE");
}

#[test]
fn a_nudge_log_line_is_shown_without_its_prefix_and_ids() {
    assert_eq!(
        nudge_log_line(
            "2026-10-15 INFO aterm_gui::app_nudge: repaint nudge hold rows=49 cols=144 session=3 epoch=1"
        ),
        "hold rows=49 cols=144"
    );
}
