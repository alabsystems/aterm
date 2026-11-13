// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::Terminal;
use aterm_types::Appearance;

fn drain(term: &mut Terminal) -> Vec<u8> {
    term.take_response().unwrap_or_default()
}

#[test]
fn dsr_996_reports_current_scheme() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?996n");
    assert_eq!(drain(&mut term), b"\x1b[?997;1n");

    term.set_color_scheme(Appearance::Light);
    let _ = drain(&mut term); // discard any push from the change
    term.process(b"\x1b[?996n");
    assert_eq!(drain(&mut term), b"\x1b[?997;2n");
}

#[test]
fn response_append_alloc_fault_drops_response_fail_closed() {
    use crate::fault;
    let mut term = Terminal::new(24, 80);
    fault::with_armed("response.append_alloc", || {
        term.process(b"\x1b[?996n");
    });
    assert!(
        term.take_response().is_none(),
        "an armed response-append alloc fault must drop the response (fail closed)"
    );
    term.process(b"\x1b[?996n");
    assert_eq!(
        drain(&mut term),
        b"\x1b[?997;1n",
        "response path recovers after the fault clears"
    );
}

#[test]
fn mode_2031_pushes_on_change_only_when_set() {
    let mut term = Terminal::new(24, 80);
    term.set_color_scheme(Appearance::Light);
    assert!(term.take_response().is_none(), "no push without DEC 2031");

    term.process(b"\x1b[?2031h");
    term.set_color_scheme(Appearance::Dark);
    assert_eq!(drain(&mut term), b"\x1b[?997;1n");

    term.set_color_scheme(Appearance::Dark);
    assert!(term.take_response().is_none(), "unchanged scheme = no push");
}

/// A program's replies drain the response limit and its 2031 push latches the scheme only once the clocks its slices were parsed at refilled it, however slowly or fast the slices are folded, under the engine's own limit and the policy's.
#[test]
fn the_2031_push_is_admitted_by_the_slices_clocks() {
    use crate::terminal::ClockReading;
    use std::time::Duration;
    let at = |base: ClockReading, ms: u64| ClockReading {
        monotonic: base.monotonic + Duration::from_millis(ms),
        wall_ms: base.wall_ms.map(|w| w + ms),
    };
    let mut drain_bucket = b"\x1b[?2031h".to_vec();
    for _ in 0..20_000 {
        drain_bucket.extend_from_slice(b"\x1b[6n");
    }
    for _ in 0..8 {
        drain_bucket.extend_from_slice(b"\x1b[5n");
    }
    for policy in [false, true] {
        for (gap_ms, pause, admitted) in [
            (0, Duration::from_millis(150), false),
            (2_000, Duration::ZERO, true),
        ] {
            let mut term = Terminal::new(24, 80);
            if policy {
                term.apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
                    aterm_policy::profiles::standard(),
                ));
            }
            let base = ClockReading::now();
            term.process_at(&drain_bucket, base);
            let _ = drain(&mut term);
            std::thread::sleep(pause);
            term.process_at(b"\x1b]11;#ffffff\x07", at(base, gap_ms));
            assert_eq!(
                term.modes().color_scheme == Appearance::Light,
                admitted,
                "policy: {policy}, {gap_ms} ms later by the slices' clocks, {pause:?} later by the system's"
            );
        }
    }
}

/// The policy's palette bucket refills by the clocks the slices were parsed at, never the system's: a drained bucket answers an OSC 4 query only once the slice's clock has moved.
#[test]
fn the_palette_bucket_refills_by_the_slices_clocks() {
    use crate::terminal::ClockReading;
    use std::time::Duration;
    let at = |base: ClockReading, ms: u64| ClockReading {
        monotonic: base.monotonic + Duration::from_millis(ms),
        wall_ms: base.wall_ms.map(|w| w + ms),
    };
    // Twelve queries of seven pairs (the OSC parser keeps 15 parameters): 84 against the 64-token bucket.
    let mut drain_bucket = Vec::new();
    for _ in 0..12 {
        drain_bucket.extend_from_slice(b"\x1b]4");
        for i in 0..7 {
            drain_bucket.extend_from_slice(format!(";{i};?").as_bytes());
        }
        drain_bucket.push(0x07);
    }
    for (gap_ms, pause, admitted) in [
        (0, Duration::from_millis(150), false),
        (2_000, Duration::ZERO, true),
    ] {
        let mut term = Terminal::new(24, 80);
        term.apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
            aterm_policy::profiles::standard(),
        ));
        let base = ClockReading::now();
        term.process_at(&drain_bucket, base);
        let drained = drain(&mut term);
        assert_eq!(
            drained.windows(4).filter(|w| w == b"\x1b]4;").count(),
            64,
            "fixture: the bucket answered 64 pairs, then ran dry"
        );
        std::thread::sleep(pause);
        term.process_at(b"\x1b]4;0;?\x07", at(base, gap_ms));
        let answer = drain(&mut term);
        assert_eq!(
            answer.windows(6).any(|w| w == b"\x1b]4;0;"),
            admitted,
            "{gap_ms} ms later by the slices' clocks, {pause:?} later by the system's: {answer:?}"
        );
    }
}

/// A 2031 the program enables after either reset reports only what changes later, however the
/// bytes are batched: the reset's own return to the default background is settled quietly.
/// NEGATIVE CONTROL: with `reset_common_fields` leaving the latch and the `2031` arm not
/// settling it, `` RIS then 2031 in one batch: no change `` fails (either alone holds it).
#[test]
fn a_2031_after_a_reset_pushes_only_later_changes() {
    for case in [
        "RIS then 2031 in one batch",
        "RIS, then 2031",
        "the host's reset, then 2031",
    ] {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]11;#ffffff\x1b\\");
        assert!(
            term.take_reports().is_none(),
            "PRECONDITION: no push without 2031"
        );
        match case {
            "RIS then 2031 in one batch" => term.process(b"\x1bc\x1b[?2031h"),
            "RIS, then 2031" => {
                term.process(b"\x1bc");
                term.process(b"\x1b[?2031h");
            }
            _ => {
                term.reset();
                term.process(b"\x1b[?2031h");
            }
        }
        assert_eq!(term.take_reports(), None, "{case}: no change");
        term.process(b"\x1b]11;#ffffff\x1b\\");
        assert_eq!(
            term.take_reports().unwrap_or_default(),
            b"\x1b[?997;2n",
            "{case}: a later change pushes"
        );
    }
}

/// A scheme change the program's output makes is settled under the 2031 it was made under,
/// wherever the reads end: one made before `?2031h` is never pushed, and one made under 2031 is
/// pushed though `?2031l` or a RIS follows it in the same read.
/// NEGATIVE CONTROL: with the `2031` arm not settling, `` before ?2031h, one read `` and
/// `` under 2031, then ?2031l, one read `` fail; with `reset_terminal_state` not settling,
/// `` under 2031, then RIS, one read `` fails.
#[test]
fn a_scheme_change_is_settled_under_the_2031_it_was_made_under() {
    const LIGHT: &[u8] = b"\x1b]11;#ffffff\x1b\\";
    for (case, enabled, then, pushed) in [
        ("before ?2031h", false, &b"\x1b[?2031h"[..], None),
        (
            "under 2031, then ?2031l",
            true,
            b"\x1b[?2031l",
            Some(&b"\x1b[?997;2n"[..]),
        ),
        (
            "under 2031, then RIS",
            true,
            b"\x1bc",
            Some(&b"\x1b[?997;2n"[..]),
        ),
    ] {
        for one_read in [true, false] {
            let mut term = Terminal::new(24, 80);
            if enabled {
                term.process(b"\x1b[?2031h");
            }
            assert_eq!(
                term.take_reports(),
                None,
                "PRECONDITION: {case}: nothing yet"
            );
            if one_read {
                term.process(&[LIGHT, then].concat());
            } else {
                term.process(LIGHT);
                term.process(then);
            }
            let reads = if one_read { "one read" } else { "two reads" };
            assert_eq!(term.take_reports().as_deref(), pushed, "{case}, {reads}");
        }
    }
}

/// The host's reset leaves the latch at the scheme it leaves, as a batch's end would: a rebase
/// to the baseline afterwards changes nothing.
/// NEGATIVE CONTROL: with `reset_common_fields` leaving the latch, `` the rebase changes no
/// scheme `` fails.
#[test]
fn the_hosts_reset_leaves_no_scheme_change_owed() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]11;#ffffff\x1b\\");
    assert_eq!(
        term.color_scheme(),
        Appearance::Light,
        "PRECONDITION: light"
    );
    term.reset();
    let baseline = term.color_baseline();
    assert!(
        !term.rebase_colors(&baseline).scheme_changed,
        "the rebase changes no scheme"
    );
}

/// DSR 996 answers after what the read did to the scheme is settled, as a read that ended
/// before it would have: a change made under 2031 is pushed, then answered.
/// NEGATIVE CONTROL: with the `996` arm not settling, `` one read `` gets the answer alone.
#[test]
fn a_996_after_a_change_in_its_read_sees_the_push_first() {
    const LIGHT: &[u8] = b"\x1b]11;#ffffff\x1b\\";
    for one_read in [true, false] {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2031h");
        let mut reports = Vec::new();
        if one_read {
            term.process(&[LIGHT, b"\x1b[?996n"].concat());
        } else {
            term.process(LIGHT);
            reports.extend(term.take_reports().unwrap_or_default());
            term.process(b"\x1b[?996n");
        }
        reports.extend(term.take_reports().unwrap_or_default());
        let reads = if one_read { "one read" } else { "two reads" };
        assert_eq!(reports, b"\x1b[?997;2n", "{reads}: the push");
        assert_eq!(drain(&mut term), b"\x1b[?997;2n", "{reads}: the answer");
    }
}

#[test]
fn the_2031_push_is_a_report_and_the_996_answer_a_reply() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?2031h");
    term.set_color_scheme(Appearance::Light);
    term.process(b"\x1b[?996n");
    assert_eq!(term.take_reports().unwrap_or_default(), b"\x1b[?997;2n");
    assert_eq!(drain(&mut term), b"\x1b[?997;2n", "the answer");
    assert!(term.take_reports().is_none());
}
