// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::terminal::Terminal;

struct Rec {
    header: CastHeader,
    events: Vec<CastEvent<Vec<u8>>>,
    live: Terminal,
    t: f64,
    /// The live engine stands in for an aterm before the resize undo: it
    /// drops the undo before every resize, so a quiet flap shifts it too.
    pre_undo: bool,
    policy: ResizePolicy,
}

impl Rec {
    fn new(cols: u16, rows: u16, adopted: bool) -> Self {
        let mut live = Terminal::new(rows, cols);
        if adopted {
            live.process(b"\x1b[?1049h");
        }
        Self {
            header: CastHeader {
                width: cols,
                height: rows,
                evicted: 0,
                dropped: Some(CastDrops::default()),
                resize_undo: None,
                resize_laws: CastHeader::laws_named(Some(ResizeLaw::default().header_value())),
                handoff: None,
            },
            events: Vec::new(),
            live,
            t: 0.0,
            pre_undo: false,
            policy: ResizePolicy::Native,
        }
    }

    fn set_policy(&mut self, policy: ResizePolicy) {
        self.policy = policy;
        self.live.set_console_host_posture(policy);
    }

    fn out(&mut self, dt: f64, bytes: &[u8]) -> f64 {
        self.t += dt;
        self.live.process(bytes);
        self.events.push(CastEvent::Output {
            t: self.t,
            data: bytes.to_vec(),
        });
        self.t
    }

    fn resize(&mut self, dt: f64, cols: u16, rows: u16) -> f64 {
        self.t += dt;
        if self.pre_undo {
            self.live.grid_mut().drop_resize_undo();
        }
        self.live.resize_with_policy(rows, cols, self.policy);
        self.events.push(CastEvent::Resize {
            t: self.t,
            cols,
            rows,
        });
        self.t
    }

    fn flap(&mut self, dt: f64) -> f64 {
        let (cols, rows) = (self.live.cols(), self.live.rows());
        let t = self.resize(dt, cols, rows - 1);
        self.out(0.0003, b"\x1b[?1000h");
        self.resize(0.0004, cols, rows);
        t
    }

    fn quiet_flap(&mut self, dt: f64) -> f64 {
        let (cols, rows) = (self.live.cols(), self.live.rows());
        let t = self.resize(dt, cols, rows - 1);
        self.resize(0.0007, cols, rows);
        t
    }

    fn screen(&self) -> LiveScreen {
        let c = self.live.cursor();
        LiveScreen {
            rows: (0..self.live.rows() as usize)
                .map(|r| visible_row(&self.live, r))
                .collect(),
            cursor: Some((c.row, c.col)),
            alt: Some(self.live.is_alternate_screen()),
        }
    }

    fn audit(&self, args: &DriftArgs) -> DriftReport {
        analyze(
            &self.header,
            &self.events,
            Some(&self.screen()),
            args,
            self.policy,
        )
    }
}

#[test]
fn an_audit_replays_the_console_host_posture_of_its_policy() {
    let (cols, rows) = (40u16, 10u16);
    let mut chunks: Vec<Vec<u8>> = vec![
        b"\x1b[?9001h\x1b[?1004h".to_vec(),
        b"history 1\r\nhistory 2\r\nC:\\>less file\r\n\x1b[?1049h\x1b[H\x1b[2J".to_vec(),
    ];
    for i in 1..rows {
        chunks.push(format!("pager {i}\r\n").into_bytes());
    }
    chunks.push(b"\x1b]133;D;-1\x07\x1b]133;A\x07C:\\>".to_vec());
    let mut cursors = Vec::new();
    for policy in [ResizePolicy::ConPty, ResizePolicy::Native] {
        let mut rec = Rec::new(cols, rows, false);
        rec.set_policy(policy);
        for chunk in &chunks {
            rec.out(0.01, chunk);
        }
        let screen = rec.screen();
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.fidelity, Some(true), "{policy:?}: {report:?}");
        assert_eq!(report.cursor_live, screen.cursor, "{policy:?}");
        cursors.push(screen.cursor);
    }
    assert_ne!(
        cursors[0], cursors[1],
        "the recording must reach the conhost layout"
    );

    let grow = |policy: ResizePolicy| {
        let mut rec = Rec::new(20, 6, false);
        rec.set_policy(policy);
        for i in 0..12 {
            rec.out(0.01, format!("L{i:02}\r\n").as_bytes());
        }
        rec.resize(0.1, 20, 10);
        rec
    };
    let conpty = grow(ResizePolicy::ConPty);
    let native = grow(ResizePolicy::Native);
    assert_ne!(conpty.screen().rows, native.screen().rows);
    assert_eq!(conpty.audit(&DriftArgs::default()).fidelity, Some(true));
    assert_eq!(native.audit(&DriftArgs::default()).fidelity, Some(true));
    let wrong_policy = analyze(
        &conpty.header,
        &conpty.events,
        Some(&conpty.screen()),
        &DriftArgs::default(),
        ResizePolicy::Native,
    );
    assert_eq!(wrong_policy.fidelity, Some(false));
}

#[test]
fn an_audit_replays_a_host_resync_under_the_sessions_posture() {
    let (cols, rows) = (40u16, 10u16);
    let mut rec = Rec::new(cols, rows, false);
    rec.live.set_console_host_posture(ResizePolicy::ConPty);
    rec.live.set_host_resyncs_alt_screen(true);
    rec.out(0.01, b"\x1b[?9001h\x1b[?1004h");
    rec.out(
        0.01,
        b"history 1\r\nC:\\>less file\r\n\x1b[?1049h\x1b[H\x1b[2J",
    );
    for i in 1..rows {
        rec.out(0.01, format!("pager {i}\r\n").as_bytes());
    }
    rec.out(0.01, b"\x1b]133;D;-1\x07\x1b]133;A\x07C:\\>");
    assert!(
        rec.live.take_alt_screen_resync_request(),
        "the host is asked"
    );
    rec.out(0.01, b"\x1b[?1049l");
    rec.out(0.01, b"echo x\r\nx\r\n\x1b]133;D;0\x07\x1b]133;A\x07C:\\>");
    assert!(!rec.live.is_alternate_screen());
    let screen = rec.screen();
    let args = DriftArgs::default();
    let posture = Posture {
        policy: ResizePolicy::ConPty,
        host_resyncs_alt_screen: true,
    };
    let report = analyze(&rec.header, &rec.events, Some(&screen), &args, posture);
    assert_eq!(report.fidelity, Some(true), "{report:?}");
    let local = analyze(
        &rec.header,
        &rec.events,
        Some(&screen),
        &args,
        ResizePolicy::ConPty,
    );
    assert_eq!(local.fidelity, Some(false), "the control: {local:?}");
}

fn frame(tag: &str, rows_drawn: usize) -> Vec<u8> {
    let mut s = String::from("\x1b[?2026h\x1b[2J\x1b[H");
    for r in 0..rows_drawn {
        s.push_str(&format!("\x1b[{};1H{tag}{r}", r + 1));
    }
    s.push_str("\x1b[6;3H\x1b[?2026l");
    s.into_bytes()
}

fn diff(row: usize, text: &str) -> Vec<u8> {
    format!(
        "\x1b[?2026h\x1b[{};1H\x1b[K\x1b[1m{text}\x1b[22m\x1b[6;3H\x1b[?2026l",
        row + 1
    )
    .into_bytes()
}

fn rows_of(report: &DriftReport) -> Vec<usize> {
    report.drift.iter().map(|d| d.row).collect()
}

#[test]
fn drift_finds_a_net_zero_flap_on_the_alt_screen() {
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 8));
    let flap = rec.flap(1.0);
    rec.out(0.005, &diff(2, "B2"));
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
    assert_eq!(report.fidelity, Some(true));
    assert_eq!(report.culprit_word(), fmt_t(flap));
    assert_eq!(report.candidates.len(), 1);
    assert!(report.candidates[0].net_zero());
    assert_eq!(rows_of(&report), vec![0, 1, 3, 4, 5, 6, 7]);
    let expected: Vec<String> = report.drift.iter().map(|d| d.other.clone()).collect();
    assert_eq!(expected, ["A0", "A1", "A3", "A4", "A5", "A6", "A7"]);
    assert_eq!(
        report.drift[6].live, "",
        "the grow appended a blank bottom row"
    );
    assert_eq!(
        report.drift[0].live, "A1",
        "the top row was demoted and dropped"
    );
    assert!(
        !report.seed_alt,
        "the recording enters the alt screen itself, so the replay starts on primary"
    );
    assert_eq!(report.folds, 2, "FAITHFUL + STABLE; ONLY is STABLE here");
    let wire = report.render(7, None);
    let mut lines = wire.lines();
    let head = lines.next().unwrap();
    let n: usize = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert_eq!(lines.clone().count(), n);
    assert!(head.starts_with(&format!(
        "OK {n} verdict=desync fidelity=pass against=screen drift_rows=7 runs=1 candidates=1 \
         capped=0 culprit={} anchor=",
        fmt_t(flap)
    )));
    assert!(head.ends_with(
        " seed=primary seed_basis=toggle geom=20x8 evicted=0 dropped=0 cut=- folds=2 \
         fold_events=10 ms=7"
    ));
    assert_eq!(
        lines.next().unwrap(),
        format!(
            "run t={} n=2 gap_ms=0.700 geom=20x8>20x7>20x8 net=zero first_out_ms=5.000 \
             clear_ms=- culprit=1",
            fmt_t(flap)
        )
    );
    assert_eq!(lines.next().unwrap(), "row 0 live=A1 expected=A0");
    assert_eq!(lines.last().unwrap(), "cursor live=5,2 expected=5,2");
}

#[test]
fn a_quiet_flap_is_undone_and_drift_is_clean() {
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 8));
    rec.quiet_flap(1.0);
    rec.out(0.005, &diff(2, "B2"));
    let live = rec.screen();
    assert_eq!(live.rows[0], "A0", "the demoted row came back");
    assert_eq!(live.rows[7], "A7", "no blank row appended");
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
    assert_eq!(report.fidelity, Some(true));
    assert_eq!(report.candidates.len(), 1, "still a net-zero candidate");
    assert!(report.candidates[0].net_zero());
    assert!(report.drift.is_empty());

    let split = rec.flap(1.0);
    rec.out(0.005, &diff(2, "B3"));
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
    assert_eq!(report.culprit_word(), fmt_t(split));
}

#[test]
fn a_flap_time_split_is_a_return_and_is_named() {
    for gap in [1.6, 0.4] {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 8));
        let shrink = rec.resize(1.0, 20, 7);
        rec.out(0.0003, b"\x1b[?1004h");
        rec.resize(gap, 20, 8);
        rec.out(0.005, &diff(2, "B2"));
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Desync, "gap {gap}: {report:?}");
        assert_eq!(report.fidelity, Some(true));
        assert_eq!((report.runs_total, report.eligible), (2, 1), "{report:?}");
        assert_eq!(report.culprit_word(), fmt_t(shrink));
        let run = &report.candidates[0];
        assert!(run.net_zero() && run.culprit);
        assert_eq!(run.geoms, [(20, 8), (20, 7), (20, 8)]);
        assert!(run.gap_ms > RUN_GAP_SECS * 1000.0, "{}", run.gap_ms);
        assert_eq!(rows_of(&report), vec![0, 1, 3, 4, 5, 6, 7]);
        assert_eq!(report.drift[0].live, "A1", "shifted up one row");
        assert_eq!(report.drift[0].other, "A0");
        assert_eq!(report.folds, 2, "FAITHFUL + STABLE; the journal attributes");
        let wire = report.render(0, None);
        assert!(
            wire.contains(" geom=20x8>20x7>20x8 net=zero first_out_ms=5.000 "),
            "{wire}"
        );
        assert!(!wire.contains(" undo=off"), "{wire}");
    }
}

#[test]
fn a_return_the_app_repainted_through_is_clean_and_a_kept_size_is_no_candidate() {
    let repaint = |rows: usize| {
        let mut s = String::from("\x1b[?2026h");
        for r in 0..rows {
            s.push_str(&format!("\x1b[{};1H\x1b[KR{r}", r + 1));
        }
        s.push_str("\x1b[6;3H\x1b[?2026l");
        s.into_bytes()
    };
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 8));
    rec.resize(1.0, 20, 7);
    rec.out(0.02, &repaint(7));
    rec.resize(1.6, 20, 8);
    rec.out(0.02, &repaint(8));
    rec.out(0.5, &diff(2, "B2"));
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
    assert_eq!(report.eligible, 1, "the span is weighed: {report:?}");

    let mut quiet = Rec::new(20, 8, false);
    quiet.out(0.1, b"\x1b[?1049h");
    quiet.out(0.1, &frame("A", 8));
    quiet.resize(1.0, 20, 7);
    quiet.resize(1.6, 20, 8);
    quiet.out(0.005, &diff(2, "B2"));
    assert_eq!(quiet.screen().rows[0], "A0", "the undo handed the row back");
    let report = quiet.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
    assert_eq!((report.runs_total, report.eligible), (2, 0));

    let mut kept = Rec::new(20, 8, false);
    kept.out(0.1, b"\x1b[?1049h");
    kept.out(0.1, &frame("A", 8));
    kept.resize(1.0, 20, 7);
    kept.out(0.005, &diff(2, "B2"));
    let report = kept.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
    assert_eq!((report.runs_total, report.eligible), (1, 0));
}

#[test]
fn a_recording_from_before_the_undo_replays_without_it() {
    let mut old = Rec::new(20, 8, false);
    old.pre_undo = true;
    old.out(0.1, b"\x1b[?1049h");
    old.out(0.1, &frame("A", 8));
    let flap = old.quiet_flap(1.0);
    old.out(0.005, &diff(2, "B2"));
    assert_eq!(old.screen().rows[0], "A1", "the older engine shifted it");

    let kept = old.audit(&DriftArgs::default());
    assert_eq!(kept.verdict, Verdict::Unfaithful, "{kept:?}");
    assert!(!kept.undo_dropped);
    for undo in [Undo::Drop, Undo::Match] {
        let report = old.audit(&DriftArgs {
            undo,
            ..DriftArgs::default()
        });
        assert_eq!(report.verdict, Verdict::Desync, "{undo:?}: {report:?}");
        assert_eq!(report.fidelity, Some(true));
        assert_eq!(report.culprit_word(), fmt_t(flap));
        assert!(report.undo_dropped);
        assert_eq!(rows_of(&report), vec![0, 1, 3, 4, 5, 6, 7]);
        let wire = report.render(3, Some("client"));
        assert!(
            wire.lines()
                .next()
                .unwrap()
                .ends_with(" ms=3 undo=off computed=client"),
            "{wire}"
        );
        let folds = if undo == Undo::Match { 3 } else { 2 };
        assert_eq!(report.folds, folds, "{undo:?}");
    }

    let mut new = Rec::new(20, 8, false);
    new.header.resize_undo = Some(true);
    new.out(0.1, b"\x1b[?1049h");
    new.out(0.1, &frame("A", 8));
    new.quiet_flap(1.0);
    new.out(0.005, &diff(2, "B2"));
    let report = new.audit(&DriftArgs {
        undo: Undo::Match,
        ..DriftArgs::default()
    });
    assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
    assert!(!report.undo_dropped);
    assert_eq!(report.folds, 2, "one engine: its fidelity passed");
    assert!(!report.render(3, None).contains("undo="));
    let blind = analyze(
        &old.header,
        &old.events,
        None,
        &DriftArgs {
            undo: Undo::Match,
            ..DriftArgs::default()
        },
        ResizePolicy::Native,
    );
    assert!(!blind.undo_dropped);
    assert_eq!(blind.verdict, Verdict::Clean, "{blind:?}");
}

#[test]
fn a_keyed_recording_is_never_replayed_without_the_undo() {
    let mut broken = Rec::new(20, 8, false);
    broken.header.resize_undo = Some(true);
    broken.pre_undo = true;
    broken.out(0.1, b"\x1b[?1049h");
    broken.out(0.1, &frame("A", 8));
    broken.quiet_flap(1.0);
    broken.out(0.005, &diff(2, "B2"));
    for undo in [Undo::Keep, Undo::Match] {
        let report = broken.audit(&DriftArgs {
            undo,
            ..DriftArgs::default()
        });
        assert_eq!(report.verdict, Verdict::Unfaithful, "{undo:?}: {report:?}");
        assert!(!report.undo_dropped, "{undo:?}");
        assert_eq!(
            report.folds, 1,
            "{undo:?}: no second law was tried, and no variant after the gate failed"
        );
        let wire = report.render(3, Some("client"));
        assert!(!wire.contains("undo=off"), "{wire}");
        assert!(!report.candidates.iter().any(|c| c.culprit), "{report:?}");
    }
    broken.header.resize_undo = None;
    let report = broken.audit(&DriftArgs {
        undo: Undo::Match,
        ..DriftArgs::default()
    });
    assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
    assert!(report.undo_dropped);
}

#[test]
fn a_primary_trim_flap_replays_under_the_law_its_recording_names() {
    let record = |older: bool| {
        let mut rec = Rec::new(20, 8, false);
        rec.header.resize_undo = Some(true);
        if older {
            rec.header.resize_laws = &[ResizeLaw::Undo];
            rec.live.set_resize_law(ResizeLaw::Undo);
        }
        for i in 0..20 {
            rec.out(0.01, format!("h{i}\r\n").as_bytes());
        }
        rec.out(0.1, b"\x1b[H\x1b[2Jtop\r\n");
        let mut flap = 0.0;
        for (dt, rows) in [(1.0, 7), (0.0007, 8)] {
            let t = rec.resize(dt, 20, rows);
            if rows == 7 {
                flap = t;
            }
        }
        (rec, flap)
    };
    let (new, _) = record(false);
    assert_eq!(new.screen().rows[0], "top", "the blank came back");
    let report = new.audit(&DriftArgs::default());
    assert_eq!(report.fidelity, Some(true), "{report:?}");
    assert_eq!(report.verdict, Verdict::Clean, "{report:?}");

    let (old, flap) = record(true);
    assert_eq!(old.screen().rows[1], "top", "the older law moved every row");
    let report = old.audit(&DriftArgs::default());
    assert_eq!(report.fidelity, Some(true), "{report:?}");
    assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
    assert_eq!(report.culprit_word(), fmt_t(flap));
}

#[test]
fn a_recording_replays_under_the_resize_law_its_header_names() {
    type Corner = (&'static str, &'static [u8], &'static [(u16, u16)]);
    let corners: [Corner; 2] = [
        (
            "bottom push",
            b"\x1b[H\x1b[2JP0\r\nP1\r\nP2\r\nP3\r\nP4\r\nP5\r\nP6\r\nP7\x1b[H",
            &[(20, 7), (20, 8)],
        ),
        (
            "rewrap",
            b"\x1b[H\x1b[2Jtop\r\nLLLLLLLLLLLLLLLLLLL1\r\nend\r\n",
            &[(19, 7), (20, 8)],
        ),
    ];
    for (corner, paint, path) in corners {
        for law in [
            ResizeLaw::HandBack,
            ResizeLaw::PushedRows,
            ResizeLaw::Rewrap,
        ] {
            let mut rec = Rec::new(20, 8, false);
            rec.live.set_resize_law(law);
            rec.header.resize_undo = Some(true);
            rec.header.resize_laws = CastHeader::laws_named(Some(law.header_value()));
            for i in 0..20 {
                rec.out(0.01, format!("h{i}\r\n").as_bytes());
            }
            rec.out(0.1, paint);
            let before = rec.screen();
            let mut flap = None;
            let mut dt = 1.0;
            for &(cols, rows) in path {
                flap.get_or_insert(rec.resize(dt, cols, rows));
                dt = 0.0007;
            }
            let moved = rec.screen() != before;
            let older = match law {
                ResizeLaw::HandBack => true,
                ResizeLaw::PushedRows => corner == "rewrap",
                _ => false,
            };
            assert_eq!(moved, older, "{corner}, {law:?}");
            let report = rec.audit(&DriftArgs::default());
            assert_eq!(report.fidelity, Some(true), "{corner}, {law:?}: {report:?}");
            if moved {
                assert_eq!(report.verdict, Verdict::Desync, "{corner}, {law:?}");
                assert_eq!(report.culprit_word(), fmt_t(flap.unwrap_or_default()));
            } else {
                assert_eq!(report.verdict, Verdict::Clean, "{corner}, {law:?}");
            }
            rec.header.resize_laws = &[ResizeLaw::Rewrap];
            let current = rec.audit(&DriftArgs::default());
            if older {
                assert_eq!(current.fidelity, Some(false), "{corner}, {law:?}");
                assert_eq!(current.verdict, Verdict::Unfaithful, "{corner}, {law:?}");
            }
        }
    }
}

#[test]
fn with_no_screen_two_laws_that_disagree_answer_unknown() {
    let primary = DriftArgs {
        seed: Seed::Primary,
        ..DriftArgs::default()
    };
    let record = |law: ResizeLaw| {
        let mut rec = Rec::new(20, 8, false);
        rec.live.set_resize_law(law);
        rec.header.resize_undo = Some(true);
        rec.header.resize_laws = CastHeader::laws_named(Some(law.header_value()));
        for i in 0..20 {
            rec.out(0.01, format!("h{i}\r\n").as_bytes());
        }
        rec.out(
            0.1,
            b"\x1b[H\x1b[2JP0\r\nP1\r\nP2\r\nP3\r\nP4\r\nP5\r\nP6\r\nP7\x1b[H",
        );
        let flap = rec.resize(1.0, 20, 7);
        rec.resize(0.0007, 20, 8);
        (rec, flap)
    };
    let offline = |header: &CastHeader, rec: &Rec| {
        analyze(header, &rec.events, None, &primary, ResizePolicy::Native)
    };
    let (rec, flap) = record(ResizeLaw::HandBack);
    let blind = offline(&rec.header, &rec);
    assert_eq!(blind.verdict, Verdict::Unknown, "{blind:?}");
    assert!(blind.law_ambiguous && blind.drift.is_empty(), "{blind:?}");
    assert_eq!(blind.culprit_word(), "-");
    assert!(blind.render(0, None).contains(" ms=0 law=ambiguous\n"));
    for (law, verdict) in [
        (ResizeLaw::PushedRows, Verdict::Clean),
        (ResizeLaw::HandBack, Verdict::Desync),
    ] {
        let mut alone = rec.header.clone();
        alone.resize_laws = std::slice::from_ref(match law {
            ResizeLaw::PushedRows => &ResizeLaw::PushedRows,
            _ => &ResizeLaw::HandBack,
        });
        let report = offline(&alone, &rec);
        assert_eq!(report.verdict, verdict, "{law:?}: {report:?}");
        assert!(!report.law_ambiguous);
    }
    let seen = rec.audit(&primary);
    assert_eq!(seen.verdict, Verdict::Desync, "{seen:?}");
    assert_eq!(seen.culprit_word(), fmt_t(flap));
    assert!(!seen.law_ambiguous);
    let (rec, _) = record(ResizeLaw::Rewrap);
    let blind = offline(&rec.header, &rec);
    assert_eq!(blind.verdict, Verdict::Clean, "{blind:?}");
    assert!(!blind.law_ambiguous);
}

#[test]
fn a_primary_flap_that_grows_first_replays_clean() {
    for heights in [&[9u16, 8][..], &[7, 9, 8]] {
        let mut rec = Rec::new(20, 8, false);
        rec.header.resize_undo = Some(true);
        for i in 0..20 {
            rec.out(0.01, format!("h{i}\r\n").as_bytes());
        }
        rec.out(0.1, b"\x1b[H\x1b[2Jtop\r\n");
        let mut dt = 1.0;
        for &rows in heights {
            rec.resize(dt, 20, rows);
            dt = 0.0007;
        }
        assert_eq!(rec.screen().rows[0], "top", "{heights:?}: nothing moved");
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.fidelity, Some(true), "{heights:?}: {report:?}");
        assert_eq!(report.verdict, Verdict::Clean, "{heights:?}: {report:?}");
    }
}

#[test]
fn a_primary_zoom_flap_replays_clean() {
    let paths: [&[(u16, u16)]; 3] = [
        &[(19, 7), (20, 8)],
        &[(21, 9), (20, 8)],
        &[(20, 7), (19, 7), (19, 8), (20, 8)],
    ];
    for path in paths {
        let mut rec = Rec::new(20, 8, false);
        rec.header.resize_undo = Some(true);
        for i in 0..20 {
            rec.out(0.01, format!("h{i}\r\n").as_bytes());
        }
        rec.out(0.1, b"\x1b[H\x1b[2Jtop\r\n");
        let mut dt = 1.0;
        for &(cols, rows) in path {
            rec.resize(dt, cols, rows);
            dt = 0.0007;
        }
        assert_eq!(rec.screen().rows[0], "top", "{path:?}: nothing moved");
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.fidelity, Some(true), "{path:?}: {report:?}");
        assert_eq!(report.verdict, Verdict::Clean, "{path:?}: {report:?}");
    }
}

#[test]
fn drift_is_clean_when_ed2_follows_the_flap() {
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 8));
    rec.flap(1.0);
    rec.out(0.005, &frame("C", 8));
    rec.out(0.5, &diff(2, "B2"));
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
    assert_eq!(report.fidelity, Some(true));
    assert_eq!(report.candidates.len(), 0);
    assert_eq!(report.runs_total, 1);
    assert_eq!(report.folds, 1);
    assert!(report.render(0, None).starts_with("OK 0 verdict=clean"));
}

#[test]
fn drift_is_clean_when_the_tail_is_blank() {
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 6));
    rec.flap(1.0);
    rec.out(0.005, &diff(2, "B2"));
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
    assert_eq!(report.candidates.len(), 1);
    assert!(!report.candidates[0].culprit);
    assert_eq!(report.culprit_word(), "-");
}

#[test]
fn drift_reports_unfaithful_not_desync_when_a_burst_is_missing() {
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 8));
    rec.flap(1.0);
    rec.out(0.005, &diff(2, "B2"));
    rec.events.pop();
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Unfaithful, "{report:?}");
    assert_eq!(report.fidelity, Some(false));
    assert_eq!(report.culprit_word(), "-");
    assert_eq!(rows_of(&report), vec![2]);
    let wire = report.render(0, None);
    assert!(wire.contains("row 2 live=B2 replayed=A3\n"), "{wire}");
    assert!(wire.contains("cursor live=5,2 replayed="), "{wire}");
    let mut racy = report.clone();
    racy.settle_cut(Cut::Racy);
    assert_eq!(racy.verdict, Verdict::Unknown);
}

#[test]
fn drift_attributes_the_one_culprit_among_many_healed_flaps() {
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 8));
    for k in 0..82 {
        rec.flap(60.0);
        rec.out(0.005, &frame(if k % 2 == 0 { "C" } else { "A" }, 8));
    }
    let last = rec.flap(60.0);
    rec.out(0.005, &diff(2, "B2"));
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
    assert_eq!(report.runs_total, 83);
    assert_eq!(report.candidates.len(), 1, "the anchor bounds the work");
    assert_eq!(report.culprit_word(), fmt_t(last));
    assert_eq!(report.folds, 2);
    let capped = rec.audit(&DriftArgs {
        max_runs: 0,
        ..DriftArgs::default()
    });
    assert_eq!(capped.verdict, Verdict::Desync);
    assert_eq!((capped.eligible, capped.capped), (1, 0));
    assert_eq!(capped.culprit_word(), fmt_t(last));
}

#[test]
fn drift_seeds_alt_for_an_adopted_cast() {
    let mut rec = Rec::new(20, 8, true);
    rec.out(0.1, &frame("A", 8));
    let flap = rec.flap(1.0);
    rec.out(0.005, &diff(2, "B2"));
    let report = rec.audit(&DriftArgs::default());
    assert!(report.seed_alt);
    assert_eq!(report.fidelity, Some(true), "{report:?}");
    assert_eq!(report.verdict, Verdict::Desync);
    assert_eq!(report.culprit_word(), fmt_t(flap));
    assert_eq!(report.seed_basis, SeedBasis::Live);
    let forced = rec.audit(&DriftArgs {
        seed: Seed::Primary,
        ..DriftArgs::default()
    });
    assert!(!forced.seed_alt);
    assert_eq!(forced.seed_basis, SeedBasis::Arg);
    assert_eq!(forced.verdict, Verdict::Unfaithful);
    let mut dump = rec.screen();
    dump.alt = None;
    dump.cursor = None;
    let chosen = analyze(
        &rec.header,
        &rec.events,
        Some(&dump),
        &DriftArgs::default(),
        ResizePolicy::Native,
    );
    assert_eq!(chosen.seed_basis, SeedBasis::Fidelity, "{chosen:?}");
    assert!(chosen.seed_alt);
    assert_eq!(chosen.verdict, Verdict::Desync);
    assert_eq!(chosen.folds, 3, "primary FAITHFUL, alt FAITHFUL, STABLE");
    let blind = analyze(
        &rec.header,
        &rec.events,
        None,
        &DriftArgs::default(),
        ResizePolicy::Native,
    );
    assert_eq!(blind.seed_basis, SeedBasis::Default);
    assert_eq!(blind.verdict, Verdict::Unknown, "{blind:?}");
    assert!(blind.seed_ambiguous && blind.drift.is_empty());
    assert_eq!(blind.culprit_word(), "-");
    assert_eq!(blind.folds, 4, "FAITHFUL + STABLE on each seed");
    assert!(
        blind
            .render(0, None)
            .contains(" verdict=unknown fidelity=- against=replay drift_rows=0 ")
    );
    assert!(
        blind
            .render(0, None)
            .contains(" seed=ambiguous seed_basis=default ")
    );
    let told = analyze(
        &rec.header,
        &rec.events,
        None,
        &DriftArgs {
            seed: Seed::Alt,
            ..DriftArgs::default()
        },
        ResizePolicy::Native,
    );
    assert_eq!(told.verdict, Verdict::Desync);
    assert_eq!(told.culprit_word(), fmt_t(flap));
}

#[test]
fn drift_attributes_between_two_candidates() {
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 6));
    rec.flap(1.0);
    rec.out(0.005, b"\x1b[7;1HF6\x1b[8;1HF7\x1b[6;3H");
    let bad = rec.flap(1.0);
    rec.out(0.005, &diff(2, "B2"));
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.culprit_word(), fmt_t(bad));
    assert!(!report.candidates[0].culprit && report.candidates[1].culprit);
    assert_eq!(report.folds, 2, "FAITHFUL + STABLE: the journal attributes");
}

#[test]
fn a_draw_between_the_halves_does_not_hide_the_flap() {
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 8));
    let (cols, rows) = (rec.live.cols(), rec.live.rows());
    let t = rec.resize(1.0, cols, rows - 1);
    rec.out(0.004, b"\x1b[8;1H\x1b[Kspin\x1b[6;3H");
    rec.resize(0.002, cols, rows);
    rec.out(0.005, &diff(2, "B2"));
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
    assert_eq!(report.fidelity, Some(true));
    assert_eq!((report.runs_total, report.eligible), (1, 1));
    assert!(report.candidates[0].net_zero());
    assert_eq!(report.culprit_word(), fmt_t(t));
    assert_eq!(report.drift[0].live, "A1", "shifted up one row");
}

#[test]
fn every_candidate_is_weighed_and_the_journal_names_the_culprit() {
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 8));
    let first = rec.flap(1.0);
    for _ in 0..8 {
        rec.flap(1.0);
    }
    rec.out(0.005, &diff(2, "B2"));
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
    assert_eq!((report.eligible, report.capped), (9, 0));
    assert_eq!(report.culprit_word(), fmt_t(first));
    assert_eq!(report.folds, 2, "no attribution fold");
    let narrow = rec.audit(&DriftArgs {
        max_runs: 2,
        ..DriftArgs::default()
    });
    assert_eq!(narrow.verdict, Verdict::Desync);
    assert_eq!((narrow.candidates.len(), narrow.capped), (3, 6));
    assert!(narrow.candidates[0].culprit, "the culprit is listed first");
    let none = rec.audit(&DriftArgs {
        max_runs: 0,
        ..DriftArgs::default()
    });
    assert_eq!(none.verdict, Verdict::Desync);
    assert_eq!((none.candidates.len(), none.capped), (1, 8));
    assert!(
        none.render(0, None)
            .contains(" runs=9 candidates=9 capped=8 culprit=")
    );
}

#[test]
fn a_primary_rotation_is_attributed_by_the_only_folds() {
    let mut rec = Rec::new(20, 8, false);
    rec.header.resize_laws = &[ResizeLaw::Undo];
    rec.live.set_resize_law(ResizeLaw::Undo);
    let mut paint = String::new();
    for r in 0..8 {
        paint.push_str(&format!("\x1b[{};1HP{r}", r + 1));
    }
    paint.push_str("\x1b[H");
    rec.out(0.1, paint.as_bytes());
    rec.flap(1.0);
    rec.out(0.005, b"\x1b[H");
    rec.flap(1.0);
    rec.out(0.005, b"\x1b[3;1H\x1b[KQ2\x1b[H");
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
    assert!(!report.seed_alt);
    assert_eq!(report.eligible, 2);
    assert_eq!(report.folds, 4, "FAITHFUL + STABLE + ONLY x2: {report:?}");
    assert!(
        report.candidates.iter().all(|run| run.culprit),
        "each rotation contributes: {report:?}"
    );
    let unattributed = rec.audit(&DriftArgs {
        max_runs: 0,
        ..DriftArgs::default()
    });
    assert_eq!(unattributed.verdict, Verdict::Desync);
    assert_eq!(
        (unattributed.folds, unattributed.culprit_word()),
        (2, "-".to_string())
    );
}

#[test]
fn forked_variants_equal_full_folds() {
    let record = |clear: &str| {
        let mut rec = Rec::new(20, 8, false);
        rec.header.resize_laws = &[ResizeLaw::Undo];
        rec.live.set_resize_law(ResizeLaw::Undo);
        let mut paint = String::from(clear);
        for r in 0..8 {
            paint.push_str(&format!("\x1b[{};1HP{r}", r + 1));
        }
        paint.push_str("\x1b[H");
        rec.out(0.1, paint.as_bytes());
        rec.flap(1.0);
        rec.out(0.005, b"\x1b[H");
        rec.flap(1.0);
        rec.out(0.005, b"\x1b[3;1H\x1b[KQ2\x1b[H");
        rec
    };
    let anchored = record("\x1b[2J");
    let n = anchored.events.len();
    let forked = anchored.audit(&DriftArgs::default());
    assert_eq!(forked.anchor, Some(anchored.events[0].t()), "{forked:?}");
    assert_eq!(forked.folds, 4, "FAITHFUL + STABLE + ONLY x2: {forked:?}");
    assert_eq!(
        forked.fold_events,
        n + 3 * (n - 1),
        "each variant folds the events after the anchor"
    );
    let full = record("").audit(&DriftArgs::default());
    assert_eq!(full.anchor, None);
    assert_eq!((full.folds, full.fold_events), (4, 4 * n));
    assert_eq!(
        (forked.verdict, forked.culprit_word(), &forked.drift),
        (full.verdict, full.culprit_word(), &full.drift)
    );

    let (header, events) = (&anchored.header, &anchored.events);
    let engine = Engine {
        policy: ResizePolicy::Native,
        host_resyncs: false,
        keep_undo: true,
        law: ResizeLaw::Undo,
    };
    let runs = group_runs(header, events, &scan_marks(events).clears);
    let mask = apply_mask(events, &runs.iter().collect::<Vec<_>>());
    let (stable, _) = fold(header, events, false, Some(&mask), engine, false, None);
    let first = runs[0].first;
    let at = |k: usize| {
        let (_, fork) = fold(header, events, false, None, engine, true, Some(k));
        fold_rest(fork.expect("forks"), events, k + 1, Some(&mask), engine)
    };
    assert_eq!(at(first - 1), stable, "forked at the anchor");
    // Negative control: a fork past the first masked resize carries it (and its hand-back) into a variant that skips it.
    assert_ne!(at(first + 1), stable, "forked past a masked resize");
}

#[test]
fn a_racy_cut_is_answered_by_the_replays() {
    let mut rec = Rec::new(20, 8, true);
    rec.out(0.1, &frame("A", 8));
    let flap = rec.flap(1.0);
    rec.out(0.005, &diff(2, "B2"));
    let mut report = analyze_replay(
        &rec.header,
        &rec.events,
        Some(true),
        &DriftArgs::default(),
        ResizePolicy::Native,
    );
    report.settle_cut(Cut::Racy);
    assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
    assert_eq!(
        (report.fidelity, report.seed_basis),
        (None, SeedBasis::Live)
    );
    assert_eq!(report.culprit_word(), fmt_t(flap));
    let head = report.render(0, None);
    assert!(
        head.starts_with("OK 9 verdict=desync fidelity=- against=replay "),
        "{head}"
    );
    assert!(head.lines().next().unwrap().contains(" cut=racy "));
}

#[test]
fn drift_without_a_screen_compares_the_replays() {
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, b"\x1b[?1049h");
    rec.out(0.1, &frame("A", 8));
    rec.flap(1.0);
    rec.out(0.005, &diff(2, "B2"));
    let mut report = analyze(
        &rec.header,
        &rec.events,
        None,
        &DriftArgs::default(),
        ResizePolicy::Native,
    );
    report.settle_cut(Cut::File);
    assert_eq!(report.verdict, Verdict::Desync);
    assert_eq!(report.fidelity, None);
    let wire = report.render(0, Some("client"));
    assert!(
        wire.starts_with("OK 9 verdict=desync fidelity=- against=replay drift_rows=7"),
        "{wire}"
    );
    assert!(
        wire.lines()
            .next()
            .unwrap()
            .ends_with(" cut=file folds=2 fold_events=10 ms=0 computed=client")
    );
    let capped = DriftReport {
        list_rows: Some(2),
        ..report
    };
    let wire = capped.render(0, None);
    assert!(wire.starts_with("OK 4 verdict=desync"), "{wire}");
    assert!(wire.contains(" drift_rows=7 "));
}

#[test]
fn drift_of_an_empty_recording_is_empty() {
    let header = CastHeader {
        width: 80,
        height: 24,
        evicted: 0,
        dropped: None,
        resize_undo: None,
        resize_laws: &[ResizeLaw::Undo],
        handoff: None,
    };
    let events: Vec<CastEvent<Vec<u8>>> = Vec::new();
    let report = analyze(
        &header,
        &events,
        None,
        &DriftArgs::default(),
        ResizePolicy::Native,
    );
    assert_eq!(report.verdict, Verdict::Empty);
    assert_eq!(report.folds, 0);
    assert!(
        report
            .render(0, None)
            .starts_with("OK 0 verdict=empty fidelity=- against=replay drift_rows=0 runs=0")
    );
    assert!(report.render(0, None).contains(" dropped=- "));
}

#[test]
fn an_alt_exit_after_the_last_clear_drops_the_anchor() {
    let mut rec = Rec::new(20, 8, true);
    rec.out(0.1, &frame("A", 8));
    rec.out(0.1, b"\x1b[?1049l");
    let report = rec.audit(&DriftArgs::default());
    assert_eq!(report.anchor, None);
    assert!(report.seed_alt);
    assert_eq!(report.verdict, Verdict::Clean, "{report:?}");

    let mut split = Rec::new(20, 8, false);
    split.out(0.1, b"\x1b[?10");
    split.out(0.1, b"49h\x1b[1;1HA0");
    split.flap(1.0);
    split.out(0.1, b"\x1b[");
    let clear_t = split.out(0.1, b"2J\x1b[1;1HB0");
    let anchored = split.audit(&DriftArgs::default());
    assert_eq!(anchored.seed_basis, SeedBasis::Toggle);
    assert!(!anchored.seed_alt);
    assert_eq!(anchored.anchor, Some(clear_t));
    assert_eq!(anchored.eligible, 0);
    assert_eq!(anchored.verdict, Verdict::Clean);
    assert_eq!(anchored.fidelity, Some(true));

    split.out(0.1, b"\x1b[?104");
    split.out(0.1, b"9l");
    let dropped = split.audit(&DriftArgs::default());
    assert_eq!(dropped.anchor, None);
}

#[test]
fn args_parse_the_grammar_and_refuse_the_rest() {
    assert_eq!(DriftArgs::parse(""), Ok(DriftArgs::default()));
    assert_eq!(
        DriftArgs::parse("rows=3 max_runs=40 seed=alt"),
        Ok(DriftArgs {
            max_runs: MAX_RUNS,
            list_rows: Some(3),
            seed: Seed::Alt,
            undo: Undo::Keep,
        })
    );
    for bad in [
        "max_runs",
        "rows=-1",
        "seed=main",
        "frames",
        "count=2",
        "undo=off",
    ] {
        assert_eq!(DriftArgs::parse(bad), Err(USAGE), "{bad}");
    }
}

#[test]
fn offline_drift_sees_the_handoff() {
    use crate::asciicast::{parse, write_into};
    let mut rec = Rec::new(20, 8, false);
    rec.out(0.1, &frame("A", 8));
    rec.flap(1.0);
    let offline = |handoff: Option<HandoffHeader>| {
        let header = CastHeader {
            resize_undo: Some(true),
            handoff,
            ..rec.header.clone()
        };
        let mut text = String::new();
        write_into(&mut text, &header, &rec.events);
        let cast = parse(&text).expect("parses");
        let mut report = analyze(
            &cast.header,
            &cast.events,
            None,
            &DriftArgs::default(),
            ResizePolicy::Native,
        );
        report.settle_cut(Cut::File);
        let wire = report.render(0, Some("client"));
        wire.lines().next().unwrap().to_string()
    };
    let built = offline(Some(HandoffHeader {
        from_build: Some(42),
    }));
    assert!(
        built.ends_with(" ms=0 carried=0 from_build=42 computed=client"),
        "{built}"
    );
    let unknown = offline(Some(HandoffHeader { from_build: None }));
    assert!(
        unknown.ends_with(" ms=0 carried=0 from_build=- computed=client"),
        "{unknown}"
    );
    let plain = offline(None);
    assert!(
        plain.ends_with(" ms=0 computed=client") && !plain.contains("carried="),
        "{plain}"
    );
    assert!(DriftArgs::parse("max_runs=+5").is_err());
    assert!(DriftArgs::parse("rows=+5").is_err());
}
