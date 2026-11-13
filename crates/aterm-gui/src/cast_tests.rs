// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Drives both shipping queues through the real entry ceiling, projecting every
/// one-byte append onto the derived model; a payload-only rule is the negative
/// control at the first count eviction.
#[test]
fn tiny_output_retention_conforms() {
    use aterm_models_gui::output_retention_model;
    use aterm_spec::interp;
    let projection = |budget: usize| {
        interp::with_consts(
            &output_retention_model(),
            &[
                ("Cap", MAX_RETAINED_EVENTS as i64),
                ("ByteBudget", budget as i64),
                ("MaxSeq", (MAX_RETAINED_EVENTS + 9) as i64),
            ],
        )
    };
    let fan_model = projection(DEFAULT_BUDGET_BYTES);
    let rec_model = projection(DEFAULT_BUDGET_BYTES / output_cost(1));
    let rec_payload_only = interp::with_buggy(&rec_model, 1);
    let mut fan_state = fan_model.init_state();
    let mut rec_state = rec_model.init_state();
    let mut payload_only_state = rec_payload_only.init_state();
    let mut rec = CastRecorder::new(80, 24);
    let fan = Arc::new(ByteFanout::new());
    let sub = fan.subscribe();
    let first: Arc<[u8]> = Arc::from(&b"x"[..]);
    let weak = Arc::downgrade(&first);
    for i in 0..MAX_RETAINED_EVENTS + 9 {
        let burst = if i == 0 {
            first.clone()
        } else {
            Arc::from(&b"x"[..])
        };
        rec.record_output_shared(Duration::from_millis(i as u64), burst.clone());
        fan.tee(&burst, i as u64);
        assert!(rec_model.fire("Push", &mut rec_state));
        let count = (rec_state["seq"] - rec_state["lo"] + 1) as usize;
        assert_eq!(rec.events.len(), count);
        assert_eq!(rec.evicted, (rec_state["lo"] - 1) as u64);
        assert_eq!(rec.used, count * output_cost(1));
        assert!(fan_model.fire("Push", &mut fan_state));
        let count = (fan_state["seq"] - fan_state["lo"] + 1) as usize;
        let q = sub.slot.queue.lock().unwrap();
        assert_eq!(q.bursts.len(), count);
        assert_eq!(q.dropped, (fan_state["lo"] - 1) as u64);
        assert_eq!(q.used, count);
        assert!(rec_payload_only.fire("Push", &mut payload_only_state));
        assert!(rec_model.check_invariant("EntriesBounded", &payload_only_state));
    }
    drop(first);
    assert!(
        weak.upgrade().is_none(),
        "evicted payload released by both queues"
    );
    assert!(rec.used <= DEFAULT_BUDGET_BYTES);
    assert!(
        rec.events.slots() <= rec.events.len() + 3 * CHUNK_EVENTS,
        "{} slots for {} events",
        rec.events.slots(),
        rec.events.len()
    );
    eprintln!(
        "tiny output retention: {} events in {} slot bytes, {} bytes charged of {}",
        rec.events.len(),
        rec.events.slots() * EVENT_SLOT_BYTES,
        rec.used,
        DEFAULT_BUDGET_BYTES
    );
    assert_eq!(
        sub.slot.queue.lock().unwrap().bursts.capacity(),
        MAX_RETAINED_EVENTS
    );
    assert!(
        rec.to_asciicast()
            .lines()
            .next()
            .unwrap()
            .contains("aterm_truncated")
    );
    let (bursts, dropped) = sub.drain();
    assert_eq!(bursts.len(), MAX_RETAINED_EVENTS);
    assert_eq!(dropped, 9);
    assert_eq!(sub.drain(), (Vec::new(), 0));

    let old = interp::with_buggy(&fan_model, 1);
    let mut before = fan_state.clone();
    before.insert("seq", MAX_RETAINED_EVENTS as i64);
    before.insert("lo", 1);
    assert!(old.fire("Push", &mut before));
    assert!(!fan_model.check_invariant("EntriesBounded", &before));
}

#[test]
fn empty_live_bursts_use_no_queue_space() {
    let fan = Arc::new(ByteFanout::new());
    let sub = fan.subscribe();
    for i in 0..MAX_RETAINED_EVENTS + 1 {
        fan.tee(&Arc::from(&b""[..]), i as u64);
    }
    let q = sub.slot.queue.lock().unwrap();
    assert_eq!(q.bursts.capacity(), 0);
    assert_eq!(q.used, 0);
    assert_eq!(q.dropped, 0);
}

/// The single forward fold reproduces, frame for frame, a per-frame re-fold (each
/// frame = the screen after every event with `t <= frame_target`).
#[test]
fn fold_frames_single_pass_matches_per_frame_refold() {
    let mut rec = CastRecorder::new(40, 6);
    assert!(
        rec.snapshot()
            .fold_frames(4, ResizePolicy::Native)
            .is_empty(),
        "empty recording ⇒ no frames"
    );
    rec.record_output(Duration::from_millis(0), b"one");
    rec.record_output(Duration::from_millis(100), b"\r\ntwo");
    rec.record_output(Duration::from_millis(200), b"\r\nthree");
    rec.record_output(Duration::from_millis(300), b"\r\nfour");
    let snap = rec.snapshot();
    let count = 5;
    let got = snap.fold_frames(count, ResizePolicy::Native);
    assert_eq!(got.len(), count);
    let span = Duration::from_millis(300);
    for (k, (_, rows)) in got.iter().enumerate() {
        let target = span * ((k + 1) as u32) / (count as u32);
        let mut term = Terminal::new(6, 40);
        for ev in snap.events.iter() {
            if ev.t() > target {
                break;
            }
            if let Event::Output { data, .. } = ev {
                term.process(data);
            }
        }
        let want: Vec<String> = (0..term.rows() as usize)
            .map(|r| crate::control::visible_row(&term, r))
            .collect();
        assert_eq!(rows, &want, "frame {k} diverged from per-frame re-fold");
    }
}

/// `fold_frames` rebases to the first retained event, so survivors far past t0
/// spread frames over the surviving span rather than a blank leading gap.
#[test]
fn fold_frames_rebases_to_first_event() {
    let mut rec = CastRecorder::new(40, 6);
    rec.record_output(Duration::from_secs(300), b"AAA");
    rec.record_output(Duration::from_secs(310), b"\r\nZZZ");
    let frames = rec.snapshot().fold_frames(2, ResizePolicy::Native);
    assert!(
        frames[0].1[0].contains("AAA"),
        "frame 1 must not be blank after rebase"
    );
    assert_eq!(frames[0].0, Duration::from_secs(5));
    assert_eq!(frames[1].0, Duration::from_secs(10));
}

/// `cast frames` replays a session under its seam policy, as `cast drift` does
/// (`aterm_control::replay::ReplayEngine`).
#[test]
fn fold_frames_replays_under_the_sessions_seam_policy() {
    use aterm_control::cast_drift::{DriftArgs, LiveScreen, analyze};
    enum Step {
        Out(Vec<u8>),
        Grow(u16),
    }
    let policy = ResizePolicy::ConPty;
    let (cols, rows) = (40u16, 10u16);
    let mut steps = vec![
        Step::Out(b"\x1b[?9001h\x1b[?1004h".to_vec()),
        Step::Out(b"history 1\r\nhistory 2\r\nC:\\>less file\r\n\x1b[?1049h\x1b[H\x1b[2J".to_vec()),
    ];
    for i in 1..rows {
        steps.push(Step::Out(format!("pager {i}\r\n").into_bytes()));
    }
    steps.push(Step::Out(b"\x1b]133;D;-1\x07\x1b]133;A\x07C:\\>".to_vec()));
    steps.push(Step::Out(
        (0..12)
            .flat_map(|i| format!("\r\nL{i:02}").into_bytes())
            .collect(),
    ));
    steps.push(Step::Grow(rows + 4));
    let mut live = Terminal::new(rows, cols);
    live.set_console_host_posture(policy);
    let mut rec = CastRecorder::new(cols, rows);
    for (i, step) in steps.iter().enumerate() {
        let t = Duration::from_millis(10 * i as u64);
        match step {
            Step::Out(bytes) => {
                live.process(bytes);
                rec.record_output(t, bytes);
            }
            Step::Grow(to) => {
                live.resize_with_policy(*to, cols, policy);
                rec.record_resize(t, cols, *to);
            }
        }
    }
    let screen: Vec<String> = (0..live.rows() as usize)
        .map(|r| crate::control::visible_row(&live, r))
        .collect();
    let folded = |policy| {
        rec.snapshot()
            .fold_frames(1, policy)
            .pop()
            .map(|(_, rows)| rows)
            .expect("a frame")
    };
    assert_eq!(folded(policy), screen, "the live ConPTY screen");
    let (header, events) = rec.snapshot().into_cast();
    let c = live.cursor();
    let held = LiveScreen {
        rows: screen.clone(),
        cursor: Some((c.row, c.col)),
        alt: Some(live.is_alternate_screen()),
    };
    let report = analyze(&header, &events, Some(&held), &DriftArgs::default(), policy);
    assert_eq!(report.fidelity, Some(true), "{report:?}");
    assert_ne!(
        folded(ResizePolicy::Native),
        screen,
        "the recording must tell the policies apart"
    );
}

/// Each event line is a `[f64, "o"|"r", string]` JSON array, output-only for
/// `record_output`, with a correctly JSON-escaped payload.
#[test]
fn events_are_well_formed_arrays() {
    let mut rec = CastRecorder::new(80, 24);
    rec.record_output(Duration::from_millis(100), b"hello\n");
    rec.record_output(Duration::from_millis(250), b"a\tb\"c\\d\r");
    rec.record_resize(Duration::from_millis(300), 100, 30);

    let cast = rec.to_asciicast();
    let mut lines = cast.lines();
    let _header = lines.next().unwrap();

    let e0 = lines.next().unwrap();
    assert_eq!(
        parse_event(e0),
        (0.100, "o".to_string(), "hello\n".to_string())
    );

    let e1 = lines.next().unwrap();
    assert_eq!(
        parse_event(e1),
        (0.250, "o".to_string(), "a\tb\"c\\d\r".to_string())
    );
    assert!(e1.contains("\\t") && e1.contains("\\\"") && e1.contains("\\\\") && e1.contains("\\r"));
    assert!(!e1[1..e1.len() - 1].contains('\t'));

    let e2 = lines.next().unwrap();
    assert_eq!(
        parse_event(e2),
        (0.300, "r".to_string(), "100x30".to_string())
    );

    assert!(lines.next().is_none());
}

/// Timestamps stay non-decreasing when a caller's `t` goes backwards.
#[test]
fn timestamps_are_monotonic() {
    let mut rec = CastRecorder::new(80, 24);
    rec.record_output(Duration::from_millis(500), b"a");
    rec.record_output(Duration::from_millis(200), b"b");
    rec.record_output(Duration::from_millis(700), b"c");

    let cast = rec.to_asciicast();
    let ts: Vec<f64> = cast.lines().skip(1).map(|l| parse_event(l).0).collect();
    assert_eq!(ts.len(), 3);
    for w in ts.windows(2) {
        assert!(w[1] >= w[0], "ts not monotonic: {ts:?}");
    }
    assert_eq!(ts[0], 0.500);
    assert_eq!(ts[1], 0.500);
    assert_eq!(ts[2], 0.700);
}

/// Control bytes below 0x20 (other than \n\r\t) use the \u00XX form, so the
/// payload is a legal JSON string body.
#[test]
fn control_bytes_use_unicode_escape() {
    let mut rec = CastRecorder::new(80, 24);
    rec.record_output(Duration::ZERO, b"\x1b[0m\x07\x00");
    let cast = rec.to_asciicast();
    let line = cast.lines().nth(1).unwrap();
    assert!(line.contains("\\u001b") && line.contains("\\u0007") && line.contains("\\u0000"));
    assert!(!line.as_bytes().iter().any(|&b| b < 0x20));
    let payload = parse_event(line).2;
    assert_eq!(payload, "\u{1b}[0m\u{7}\u{0}");
}

/// After drop-oldest eviction the asciicast discloses the truncation in the header
/// and rebases survivor timestamps to the first retained event.
#[test]
fn eviction_is_disclosed_and_timestamps_rebased() {
    let mut rec = CastRecorder::with_budget(80, 24, 10);
    for i in 0..100u32 {
        rec.record_output(Duration::from_secs(u64::from(i)), b"abcd");
    }
    assert!(rec.evicted() > 0, "head was evicted");
    assert!(rec.event_count() < 10, "bounded: {}", rec.event_count());
    let cast = rec.to_asciicast();
    let header = cast.lines().next().unwrap();
    assert!(header.starts_with('{') && header.ends_with('}'));
    assert!(header.contains("\"version\": 2"));
    assert!(header.contains("aterm_truncated"), "disclosed: {header}");
    assert!(header.contains("\"evicted_events\""));
    let first_ts = parse_event(cast.lines().nth(1).unwrap()).0;
    assert_eq!(first_ts, 0.0, "first survivor rebased to 0");
    let ts: Vec<f64> = cast.lines().skip(1).map(|l| parse_event(l).0).collect();
    for w in ts.windows(2) {
        assert!(w[1] >= w[0]);
    }
}

/// An untruncated recording keeps absolute timestamps and no `aterm_truncated` field.
#[test]
fn no_eviction_keeps_plain_header_and_absolute_timestamps() {
    let mut rec = CastRecorder::new(80, 24);
    rec.record_output(Duration::from_millis(100), b"hi");
    rec.record_output(Duration::from_millis(400), b"there");
    assert_eq!(rec.evicted(), 0);
    let cast = rec.to_asciicast();
    assert!(!cast.contains("aterm_truncated"), "no false disclosure");
    assert_eq!(parse_event(cast.lines().nth(1).unwrap()).0, 0.100);
    assert_eq!(parse_event(cast.lines().nth(2).unwrap()).0, 0.400);
}

/// The header names the geometry in force at the first retained event.
#[test]
fn evicted_resize_moves_the_header_geometry() {
    let line: Vec<u8> = [&b"\r\n"[..], &[b'X'; 100]].concat();
    let mut rec = CastRecorder::with_budget(80, 24, 4 * output_cost(line.len()));
    rec.record_output(Duration::ZERO, &line);
    rec.record_resize(Duration::from_millis(1), 100, 30);
    let geometry = |rec: &CastRecorder| {
        let (header, _) = rec.snapshot().into_cast();
        (header.width, header.height)
    };
    assert_eq!(geometry(&rec), (80, 24), "untruncated: construction size");
    let mut truncated_with_resize = false;
    let mut ms = 2;
    while rec
        .events
        .iter()
        .any(|ev| matches!(ev, Event::Resize { .. }))
    {
        assert_eq!(geometry(&rec), (80, 24), "a retained resize still applies");
        truncated_with_resize |= rec.evicted() > 0;
        rec.record_output(Duration::from_millis(ms), &line);
        ms += 1;
    }
    assert!(
        truncated_with_resize,
        "the output before the resize was evicted while the resize stayed"
    );
    assert_eq!(geometry(&rec), (100, 30));
    let cast = rec.to_asciicast();
    assert!(
        cast.starts_with("{\"version\": 2, \"width\": 100, \"height\": 30,"),
        "{cast}"
    );
    let parsed = aterm_control::asciicast::parse(&cast).expect("parses");
    assert_eq!((parsed.header.width, parsed.header.height), (100, 30));
    let frames = rec.snapshot().fold_frames(1, ResizePolicy::Native);
    let screen = &frames.last().expect("one frame").1;
    assert_eq!(screen.len(), 30);
    assert!(
        screen.iter().any(|row| *row == "X".repeat(100)),
        "{screen:?}"
    );
}

/// A seeded recording: random bursts and pieces from every escape class (quotes,
/// backslashes, C0, DEL, C1, bidi controls, multibyte text, invalid bytes and a
/// lead completed by the next burst) and resizes, with timestamps that sometimes
/// go backwards, under `budget`.
fn seeded_recording(seed: u64, budget: usize, events: usize) -> CastRecorder {
    const PIECES: [&[u8]; 9] = [
        b"plain text ",
        b"\"q\\",
        b"\x1b[31m\x07\x00\x7f\r\n\t",
        "\u{200e}\u{2066}x\u{2069}\u{2028}".as_bytes(),
        "\u{85}\u{9b}".as_bytes(),
        "é漢🙂".as_bytes(),
        &[0x80, 0xff, b'a', 0xc3],
        &[0xa9],
        &[0xf0, 0x9f],
    ];
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut rec = CastRecorder::with_budget(80, 24, budget);
    let mut ms = 0u64;
    for _ in 0..events {
        ms = match next() % 8 {
            0 => ms.saturating_sub(next() % 50),
            _ => ms + next() % 900,
        };
        let t = Duration::from_millis(ms);
        match next() % 10 {
            0 => rec.record_resize(t, 1 + (next() % 300) as u16, 1 + (next() % 100) as u16),
            1..=4 => rec.record_output(t, PIECES[(next() % 9) as usize]),
            _ => {
                let bytes: Vec<u8> = (0..next() % 64).map(|_| next() as u8).collect();
                rec.record_output(t, &bytes);
            }
        }
    }
    rec
}

/// The asciicast text as the recorder serialized it under its own lock, before
/// `cast` moved serialization onto the snapshot, with each output scrubbed as a
/// shared cast is (`cast_scrub`): the reference the off-lock writer must match
/// byte for byte.
fn locked_reference(rec: &CastRecorder) -> String {
    fn fmt_t(t: Duration) -> String {
        format!("{:.6}", t.as_secs_f64())
    }
    fn json_escape_bytes(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() + 2);
        let mut i = 0;
        while i < bytes.len() {
            match std::str::from_utf8(&bytes[i..]) {
                Ok(s) => {
                    out.push_str(&crate::control::json_escape(s));
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        let s = std::str::from_utf8(&bytes[i..i + valid]).unwrap();
                        out.push_str(&crate::control::json_escape(s));
                    }
                    out.push('\u{fffd}');
                    match e.error_len() {
                        Some(len) => i += valid + len,
                        None => break,
                    }
                }
            }
        }
        out
    }
    let truncated = rec.evicted > 0;
    let rebase = if truncated {
        rec.events.iter().next().map_or(Duration::ZERO, Event::t)
    } else {
        Duration::ZERO
    };
    let mut out = format!(
        "{{\"version\": 2, \"width\": {}, \"height\": {}, \"aterm_resize_undo\": {}",
        rec.width,
        rec.height,
        ResizeLaw::default().header_value()
    );
    if truncated {
        out.push_str(&format!(
            ", \"aterm_truncated\": {{\"evicted_events\": {}, \"note\": \
             \"drop-oldest evicted the head; leading ANSI state incomplete; \
             timestamps rebased to the first retained event\"}}",
            rec.evicted
        ));
    }
    if let Some(gap) = rec.handoff {
        let from = gap
            .from_build
            .map_or_else(|| "null".to_string(), |build| build.to_string());
        out.push_str(&format!(
            ", \"aterm_handoff\": {{\"carried\": 0, \"from_build\": {from}, \"note\": \
             \"recording restarted at a seamless update; the output and screen before it \
             are not in this recording\"}}"
        ));
    }
    out.push_str("}\n");
    let mut scrub = crate::cast_scrub::CastScrub::starting(!truncated && rec.handoff.is_none());
    let mut last_t = Duration::ZERO;
    for ev in rec.events.iter() {
        match ev {
            Event::Output { t, data, .. } => {
                let mut kept = Vec::new();
                scrub.feed(data, &mut kept);
                last_t = t.saturating_sub(rebase);
                if !kept.is_empty() {
                    out.push_str(&format!(
                        "[{}, \"o\", \"{}\"]\n",
                        fmt_t(last_t),
                        json_escape_bytes(&kept)
                    ));
                }
            }
            Event::Resize { t, cols, rows, .. } => {
                out.push_str(&format!(
                    "[{}, \"r\", \"{cols}x{rows}\"]\n",
                    fmt_t(t.saturating_sub(rebase))
                ));
            }
        }
    }
    let mut rest = Vec::new();
    scrub.finish(&mut rest);
    if !rest.is_empty() {
        out.push_str(&format!(
            "[{}, \"o\", \"{}\"]\n",
            fmt_t(last_t),
            json_escape_bytes(&rest)
        ));
    }
    out
}

/// The `cast` reply, serialized off the recorder lock into one buffer, is byte for
/// byte `OK <n>\n` and the text the recorder serialized under its lock: for an
/// empty recording, an untruncated one, truncated ones with each handoff form, and
/// one whose body length needs six digits.
#[test]
fn cast_reply_is_byte_identical_to_the_locked_serializer() {
    use crate::session_timeline::HandoffGap;
    let untruncated = seeded_recording(1, DEFAULT_BUDGET_BYTES, 400);
    let mut built = seeded_recording(2, 2048 + 96 * output_cost(0), 400);
    built.mark_handoff(HandoffGap {
        from_build: Some(42),
    });
    let mut unknown_build = seeded_recording(3, 512 + 24 * output_cost(0), 400);
    unknown_build.mark_handoff(HandoffGap { from_build: None });
    let large = seeded_recording(4, DEFAULT_BUDGET_BYTES, 20_000);
    let cases = [
        ("empty", CastRecorder::new(80, 24)),
        ("untruncated", untruncated),
        ("built", built),
        ("unknown build", unknown_build),
        ("large", large),
    ];
    for (name, rec) in cases {
        let want = locked_reference(&rec);
        match name {
            "untruncated" => {
                assert!(!want.contains("aterm_truncated") && want.contains('\u{fffd}'))
            }
            "built" => assert!(
                want.contains("aterm_truncated")
                    && want.contains("\"from_build\": 42")
                    && want.contains("\\u200e")
            ),
            "unknown build" => assert!(want.contains("\"from_build\": null")),
            "large" => assert!(want.len() >= 100_000, "{}", want.len()),
            _ => {}
        }
        let reply = crate::control::cmd_cast(&Mutex::new(rec));
        assert!(
            reply == format!("OK {}\n{want}", want.len()),
            "{name}: the reply diverged from the locked serializer"
        );
    }
}

/// `cast` lifts the snapshot under the recorder lock and serializes after
/// releasing it: a probe from the off-lock path it serializes on takes the lock.
#[test]
fn cmd_cast_releases_the_recorder_before_serializing() {
    use std::cell::Cell;
    use std::rc::Rc;
    let cast = Arc::new(Mutex::new(seeded_recording(5, DEFAULT_BUDGET_BYTES, 64)));
    let held = Rc::new(Cell::new(None));
    let _hook = serialize_hook::install({
        let (cast, held) = (cast.clone(), held.clone());
        move || held.set(Some(cast.try_lock().is_err()))
    });
    assert!(crate::control::cmd_cast(&cast).starts_with("OK "));
    assert_eq!(
        held.take(),
        Some(false),
        "serialized with the recorder lock held"
    );
    let rec = cast.lock().unwrap();
    let _ = rec.snapshot().into_cast();
    drop(rec);
    assert_eq!(held.take(), Some(true), "the probe sees a held recorder");
}

/// A lone continuation byte mid-stream renders as one U+FFFD in place.
#[test]
fn genuinely_invalid_byte_still_fffd() {
    let mut rec = CastRecorder::new(80, 24);
    rec.record_output(Duration::ZERO, &[b'A', 0x80, b'B']);
    let cast = rec.to_asciicast();
    let payload = parse_event(cast.lines().nth(1).unwrap()).2;
    assert_eq!(payload, "A\u{fffd}B");
}

/// A trailing incomplete lead is carried, not replaced: the burst that completes it
/// emits the whole character.
#[test]
fn trailing_incomplete_lead_is_carried_not_replaced() {
    let mut rec = CastRecorder::new(80, 24);
    rec.record_output(Duration::ZERO, &[b'X', 0xC3]);
    let payload = parse_event(rec.to_asciicast().lines().nth(1).unwrap()).2;
    assert_eq!(payload, "X");
    assert!(!payload.contains('\u{fffd}'));
    rec.record_output(Duration::from_millis(1), &[0xA9]);
    let last = parse_event(rec.to_asciicast().lines().last().unwrap()).2;
    assert_eq!(last, "é");
}

/// A complete shared burst is retained zero-copy: the stored event holds the
/// caller's allocation.
#[test]
fn shared_complete_burst_is_stored_without_copy() {
    let mut rec = CastRecorder::new(80, 24);
    let burst: Arc<[u8]> = Arc::from(&b"hello \xe2\x82\xac world\r\n"[..]);
    rec.record_output_shared(Duration::from_millis(1), burst.clone());
    match rec.events.iter().next().expect("one event") {
        Event::Output { data, .. } => {
            assert!(Arc::ptr_eq(data, &burst), "fast path must not copy");
        }
        Event::Resize { .. } => panic!("expected an output event"),
    }
    assert_eq!(rec.used, output_cost(burst.len()));
}

/// The zero-copy path is not taken when a carry is pending or the tail is
/// incomplete; the shared-Arc entry point reassembles like the borrowing one.
#[test]
fn shared_burst_with_split_multibyte_still_reassembles() {
    let mut rec = CastRecorder::new(80, 24);
    rec.record_output_shared(Duration::from_millis(1), Arc::from(&[0xE2u8, 0x82][..]));
    assert_eq!(rec.event_count(), 0, "incomplete lead must be carried");
    rec.record_output_shared(Duration::from_millis(2), Arc::from(&[0xACu8, 0xC3][..]));
    assert_eq!(rec.event_count(), 1);
    let payload = parse_event(rec.to_asciicast().lines().nth(1).unwrap()).2;
    assert_eq!(payload, "€");
    rec.record_output_shared(Duration::from_millis(3), Arc::from(&[0xA9u8][..]));
    let last = parse_event(rec.to_asciicast().lines().last().unwrap()).2;
    assert_eq!(last, "é");
}

/// An empty shared burst records nothing.
#[test]
fn shared_empty_burst_records_nothing() {
    let mut rec = CastRecorder::new(80, 24);
    rec.record_output_shared(Duration::from_millis(1), Arc::from(&b""[..]));
    assert_eq!(rec.event_count(), 0);
}

/// `n` complete one-burst payloads, `0;`, `1;`, …: each is retained as is.
fn numbered(n: usize) -> Vec<Arc<[u8]>> {
    (0..n)
        .map(|i| Arc::from(format!("{i};").as_bytes()))
        .collect()
}

/// A snapshot shares the sealed chunks and clones only the open events: after
/// eight chunks of bursts, lifting one moves the refcounts of fewer than two
/// chunks of payloads, and it reads the recording's events in order.
#[test]
fn a_snapshot_shares_the_sealed_chunks() {
    let mut rec = CastRecorder::new(80, 24);
    let bursts = numbered(8 * CHUNK_EVENTS);
    for (i, burst) in bursts.iter().enumerate() {
        rec.record_output_shared(Duration::from_millis(i as u64), burst.clone());
    }
    let counts = || bursts.iter().map(Arc::strong_count).collect::<Vec<_>>();
    let moved = |before: &[usize]| counts().iter().zip(before).filter(|(a, b)| a != b).count();
    let before = counts();
    let snap = rec.snapshot();
    assert!(
        moved(&before) < 2 * CHUNK_EVENTS,
        "{} cloned",
        moved(&before)
    );
    assert_eq!(snap.events.sealed.len(), rec.events.sealed.len());
    assert!(
        snap.events
            .sealed
            .iter()
            .zip(&rec.events.sealed)
            .all(|(a, b)| Arc::ptr_eq(a, b))
    );
    assert_eq!(listed(snap.events.iter()), kinds(&rec));
    drop(snap);
    let before = counts();
    let cloned: Vec<Event> = rec.events.iter().cloned().collect();
    assert_eq!(
        moved(&before),
        bursts.len(),
        "a full clone moves every count"
    );
    drop(cloned);
}

/// A resize recorded after the bursts the engine processed after it goes before
/// them even where that is inside sealed chunks: the chunks are unsealed, a snapshot
/// lifted before keeps the list it lifted, the resealed list gives back the slots
/// the unsealing took, and eviction then yields the events in the recorded order.
#[test]
fn a_late_resize_lands_inside_a_sealed_chunk() {
    let mut rec = CastRecorder::new(20, 8);
    let mut term = Terminal::new(8, 20);
    term.resize(7, 20);
    let late = ResizeStamp::take(&term);
    let n = 9 * CHUNK_EVENTS + 5;
    for burst_bytes in numbered(n) {
        let mut cuts = CastCuts::default();
        cuts.observe(0, &term, &CastWatermark::default());
        rec.record_burst(burst_bytes, cuts, rec.now());
    }
    assert!(rec.events.sealed.len() >= 8, "the bursts sealed chunks");
    let before = rec.snapshot();
    rec.record_resize_stamped(late, 20, 7);
    let now = kinds(&rec);
    assert_eq!(now.len(), n + 1);
    assert_eq!(now[0], "r:20x7", "before every burst it preceded");
    assert_eq!(
        listed(before.events.iter()),
        now[1..],
        "the snapshot is unmoved"
    );
    assert!(rec.events.sealed.len() >= 8, "sealed again");
    assert!(
        rec.events.slots() <= rec.events.len() + 3 * CHUNK_EVENTS,
        "{} slots for {} events",
        rec.events.slots(),
        rec.events.len()
    );
    assert_eq!(rec.used, rec.events.iter().map(Event::cost).sum::<usize>());
    let ts: Vec<Duration> = rec.events.iter().map(Event::t).collect();
    assert!(ts.windows(2).all(|w| w[0] <= w[1]));
    let evicted: Vec<Event> = std::iter::from_fn(|| rec.events.pop_front()).collect();
    assert_eq!(listed(evicted.iter()), now);
    assert!(rec.events.is_empty() && rec.events.sealed.is_empty());
}

/// An evicted event's payload is freed at its eviction when no snapshot shares its
/// chunk.
#[test]
fn eviction_releases_what_a_snapshot_pinned() {
    let mut log = EventLog::default();
    let bursts = numbered(3 * CHUNK_EVENTS);
    let weak: Vec<std::sync::Weak<[u8]>> = bursts.iter().map(Arc::downgrade).collect();
    for (i, data) in bursts.into_iter().enumerate() {
        let ev = Event::Output {
            t: Duration::from_millis(i as u64),
            data,
            key: None,
        };
        log.insert(log.len(), ev);
    }
    let freed = |range: std::ops::Range<usize>| weak[range].iter().all(|w| w.upgrade().is_none());
    let snap = log.share();
    for _ in 0..10 {
        drop(log.pop_front());
    }
    assert!(
        weak[..10].iter().all(|w| w.upgrade().is_some()),
        "the snapshot still reads them"
    );
    drop(snap);
    drop(log.pop_front());
    assert!(freed(0..11), "released before the chunk is spent");
    assert!(weak[11..].iter().all(|w| w.upgrade().is_some()), "live");
    drop(log.pop_front());
    assert!(freed(11..12), "unshared: freed at its eviction");
}

/// `tee` delivers every burst (non-UTF-8 included) to every subscriber, byte-exact
/// and uncoalesced.
#[test]
fn byte_fanout_tees_every_burst_to_all_subscribers() {
    let fan = Arc::new(ByteFanout::new());
    let a = fan.subscribe();
    let b = fan.subscribe();
    assert_eq!(fan.subscriber_count(), 2);
    fan.tee(&Arc::from(&b"\x1b[31m"[..]), 5);
    fan.tee(&Arc::from(&[0x80u8, 0xff, 0x00][..]), 9);
    for sub in [&a, &b] {
        let (bursts, dropped) = sub.drain();
        assert_eq!(dropped, 0);
        assert_eq!(bursts.len(), 2, "every burst delivered, no coalesce");
        assert_eq!(&bursts[0].bytes[..], b"\x1b[31m");
        assert_eq!(&bursts[1].bytes[..], &[0x80, 0xff, 0x00]);
        assert_eq!(
            (bursts[0].at_us, bursts[1].at_us),
            (5, 9),
            "each burst carries the arrival it was teed with"
        );
        assert_eq!(sub.drain().0.len(), 0);
    }
}

/// A full queue drops oldest and counts the dropped bytes; the newest burst
/// survives.
#[test]
fn byte_fanout_full_queue_drops_oldest_and_counts() {
    let fan = Arc::new(ByteFanout::with_budget(10));
    let sub = fan.subscribe();
    for i in 0..100 {
        fan.tee(&Arc::from(&b"abcd"[..]), i);
    }
    let (bursts, dropped) = sub.drain();
    assert!(bursts.len() <= 3, "queue bounded: {}", bursts.len());
    assert!(
        dropped >= 4 * (100 - bursts.len() as u64),
        "dropped counted: {dropped}"
    );
    assert_eq!(&bursts.last().unwrap().bytes[..], b"abcd");
}

/// Dropping a subscription deregisters it; `tee` then pays nothing for it.
#[test]
fn byte_subscription_drop_deregisters() {
    let fan = Arc::new(ByteFanout::new());
    {
        let _s = fan.subscribe();
        assert_eq!(fan.subscriber_count(), 1);
    }
    assert_eq!(fan.subscriber_count(), 0, "deregistered on drop");
    fan.tee(&Arc::from(&b"x"[..]), 0);
}

/// The zero-subscriber early-out re-arms at each subscribe/drop edge: a subscriber
/// gets every burst teed after `subscribe` returns and none before.
#[test]
fn byte_fanout_zero_subscriber_fast_path_rearms_across_lifecycle() {
    let fan = Arc::new(ByteFanout::new());
    fan.tee(&Arc::from(&b"before"[..]), 0);
    let sub = fan.subscribe();
    fan.tee(&Arc::from(&b"during"[..]), 0);
    let (bursts, dropped) = sub.drain();
    assert_eq!(dropped, 0);
    assert_eq!(bursts.len(), 1, "only the post-subscribe burst is owed");
    assert_eq!(&bursts[0].bytes[..], b"during");
    drop(sub);
    fan.tee(&Arc::from(&b"between"[..]), 0);
    let sub2 = fan.subscribe();
    fan.tee(&Arc::from(&b"again"[..]), 0);
    let (bursts, _) = sub2.drain();
    assert_eq!(
        bursts.len(),
        1,
        "second lifecycle delivers post-subscribe only"
    );
    assert_eq!(&bursts[0].bytes[..], b"again");
}

/// The event kinds and payloads as written, oldest first: `o:<text>` and
/// `r:<cols>x<rows>`.
fn kinds(rec: &CastRecorder) -> Vec<String> {
    listed(rec.events.iter())
}

/// [`kinds`] of any run of events.
fn listed<'a>(events: impl Iterator<Item = &'a Event>) -> Vec<String> {
    events
        .map(|ev| match ev {
            Event::Output { data, .. } => format!("o:{}", String::from_utf8_lossy(data)),
            Event::Resize { cols, rows, .. } => format!("r:{cols}x{rows}"),
        })
        .collect()
}

/// One reader hold: stamp the burst the way the reader does (inside the
/// hold, before its bytes), then process them.
fn hold(term: &mut Terminal, cuts: &mut CastCuts, at: usize, bytes: &[u8]) {
    cuts.observe(at, term, &CastWatermark::default());
    term.process(bytes);
}

/// One reader burst's record, as the reader makes it after its holds.
fn burst(rec: &mut CastRecorder, bytes: &[u8], cuts: CastCuts, t: Duration) {
    rec.record_burst(Arc::from(bytes), cuts, t);
}

/// Burst A, a resize, then burst B in the engine: in every order the two threads
/// can reach the recorder's lock, it writes `A, r, B` with non-decreasing times.
#[test]
fn a_resize_processed_between_two_bursts_serializes_between_them() {
    let arrivals: [[u8; 3]; 3] = [[0, 1, 2], [1, 0, 2], [0, 2, 1]];
    for order in arrivals {
        let mut rec = CastRecorder::new(20, 8);
        let mut term = Terminal::new(8, 20);
        let mut a = CastCuts::default();
        let ta = rec.now();
        hold(&mut term, &mut a, 0, b"A");
        term.resize(7, 20);
        let r = ResizeStamp::take(&term);
        let mut b = CastCuts::default();
        let tb = rec.now();
        hold(&mut term, &mut b, 0, b"B");
        let (mut a, mut b) = (Some(a), Some(b));
        for which in order {
            match which {
                0 => burst(&mut rec, b"A", a.take().expect("once"), ta),
                1 => rec.record_resize_stamped(r, 20, 7),
                _ => burst(&mut rec, b"B", b.take().expect("once"), tb),
            }
        }
        assert_eq!(kinds(&rec), ["o:A", "r:20x7", "o:B"], "arrival {order:?}");
        let ts: Vec<Duration> = rec.events.iter().map(Event::t).collect();
        assert!(ts.windows(2).all(|w| w[0] <= w[1]), "{order:?}: {ts:?}");
    }

    let mut old = CastRecorder::new(20, 8);
    old.record_output(old.now(), b"A");
    old.record_output(old.now(), b"B");
    old.record_resize(old.now(), 20, 7);
    assert_eq!(kinds(&old), ["o:A", "o:B", "r:20x7"]);
}

/// A burst split by a resize between the reader's slices is cut there: head
/// before the resize, tail after, in either arrival order.
#[test]
fn a_burst_straddling_a_resize_is_cut_at_the_slice() {
    for resize_first in [false, true] {
        let mut rec = CastRecorder::new(20, 8);
        let mut term = Terminal::new(8, 20);
        let mut cuts = CastCuts::default();
        let arrived = rec.now();
        hold(&mut term, &mut cuts, 0, b"head");
        hold(&mut term, &mut cuts, 4, b"+more");
        assert_eq!(cuts.pieces(), 1, "no resize yet: one piece");
        assert_eq!(cuts.rest.capacity(), 0, "an uncut burst allocates nothing");
        term.resize(7, 20);
        let r = ResizeStamp::take(&term);
        hold(&mut term, &mut cuts, 9, b"tail");
        assert_eq!(cuts.pieces(), 2, "cut where the resize took the lock");
        if resize_first {
            rec.record_resize_stamped(r, 20, 7);
        }
        burst(&mut rec, b"head+moretail", cuts, arrived);
        if !resize_first {
            rec.record_resize_stamped(r, 20, 7);
        }
        assert_eq!(
            kinds(&rec),
            ["o:head+more", "r:20x7", "o:tail"],
            "resize first: {resize_first}"
        );
        let ts: Vec<Duration> = rec.events.iter().map(Event::t).collect();
        assert!(
            ts.windows(2).all(|w| w[0] <= w[1]),
            "{resize_first}: {ts:?}"
        );
    }

    let mut term = Terminal::new(8, 20);
    let mut cuts = CastCuts::default();
    let watermark = CastWatermark::default();
    cuts.observe(0, &term, &watermark);
    term.resize(7, 20);
    cuts.observe(0, &term, &watermark);
    assert_eq!(cuts.pieces(), 1);
    assert_eq!(cuts.first.map(|s| s.ordinal), Some(1));
    assert_eq!(
        cuts.rest.capacity(),
        0,
        "superseding the first stamp allocates nothing"
    );
}

/// A one-piece burst keeps the reader's allocation.
#[test]
fn a_one_piece_burst_keeps_the_readers_allocation() {
    let term = Terminal::new(8, 20);
    let mut rec = CastRecorder::new(20, 8);
    let mut cuts = CastCuts::default();
    cuts.observe(0, &term, &CastWatermark::default());
    let bytes: Arc<[u8]> = Arc::from(&b"shared"[..]);
    rec.record_burst(bytes.clone(), cuts, rec.now());
    let Some(Event::Output { data, .. }) = rec.events.iter().next_back() else {
        panic!("an output event");
    };
    assert!(Arc::ptr_eq(data, &bytes), "no copy of a one-piece burst");
}

/// On the alternate screen a shrink demotes the top row, so order matters: the
/// engine-ordered recording replays to the live screen, while writing the resize
/// after a burst the engine processed first replays to a screen never shown.
#[test]
fn the_engine_ordered_recording_replays_to_the_live_screen() {
    const ROWS: u16 = 6;
    let frame: Vec<u8> = {
        let mut f = String::from("\x1b[?1049h\x1b[2J");
        for r in 0..ROWS {
            f.push_str(&format!("\x1b[{};1HL{r}", r + 1));
        }
        f.push_str("\x1b[4;3H");
        f.into_bytes()
    };
    let diff = b"\x1b[1;1HT1\x1b[K\x1b[4;3H".to_vec();
    let mut live = Terminal::new(ROWS, 20);
    let mut rec = CastRecorder::new(20, ROWS);
    let mut old = CastRecorder::new(20, ROWS);
    let mut c0 = CastCuts::default();
    let t0 = rec.now();
    hold(&mut live, &mut c0, 0, &frame);
    live.resize(ROWS - 1, 20);
    let shrink = ResizeStamp::take(&live);
    let mut c1 = CastCuts::default();
    let t1 = rec.now();
    hold(&mut live, &mut c1, 0, &diff);
    live.resize(ROWS, 20);
    let grow = ResizeStamp::take(&live);
    burst(&mut rec, &frame, c0, t0);
    burst(&mut rec, &diff, c1, t1);
    rec.record_resize_stamped(shrink, 20, ROWS - 1);
    rec.record_resize_stamped(grow, 20, ROWS);
    old.record_output(Duration::from_millis(1), &frame);
    old.record_output(Duration::from_millis(2), &diff);
    old.record_resize(Duration::from_millis(3), 20, ROWS - 1);
    old.record_resize(Duration::from_millis(4), 20, ROWS);

    let screen = |t: &Terminal| -> Vec<String> {
        (0..t.rows() as usize)
            .map(|r| crate::control::visible_row(t, r))
            .collect()
    };
    let last = |rec: &CastRecorder| {
        rec.snapshot()
            .fold_frames(1, ResizePolicy::Native)
            .pop()
            .map(|(_, rows)| rows)
            .expect("a frame")
    };
    assert_eq!(
        last(&rec),
        screen(&live),
        "engine order replays the live screen"
    );
    assert_ne!(last(&old), screen(&live), "arrival order does not");
    let parsed = aterm_control::asciicast::parse(&rec.to_asciicast()).expect("parses");
    let codes: Vec<&str> = parsed
        .events
        .iter()
        .map(|ev| match ev {
            CastEvent::Output { .. } => "o",
            CastEvent::Resize { .. } => "r",
        })
        .collect();
    assert_eq!(codes, ["o", "r", "o", "r"]);
}

/// Two engines in one session (a restored engine, new lifetime token): the newer
/// engine's events sort after the older's, and each engine's ordinals order its own.
#[test]
fn a_newer_engine_sorts_after_the_older_one() {
    let mut rec = CastRecorder::new(20, 8);
    let mut old = Terminal::new(8, 20);
    old.resize(7, 20);
    let old_resize = ResizeStamp::take(&old);
    let mut fresh = Terminal::new(8, 20);
    assert!(fresh.resize_lifetime() > old.resize_lifetime());
    let mut cuts = CastCuts::default();
    hold(&mut fresh, &mut cuts, 0, b"new");
    let now = rec.now();
    burst(&mut rec, b"new", cuts, now);
    rec.record_resize_stamped(old_resize, 20, 7);
    assert_eq!(kinds(&rec), ["r:20x7", "o:new"]);
}

/// A recording stands against a cut as the engine and the recorder say: a hold or
/// a resize the screen shows that is not recorded leaves it behind, its record
/// makes it exact, and anything recorded after the cut puts it past.
#[test]
fn a_recording_holds_a_cut_behind_exactly_or_past() {
    let term = Mutex::new(Terminal::new(8, 20));
    let cast = Mutex::new(CastRecorder::new(20, 8));
    let watermark = cast.lock().unwrap().watermark();
    let cut = || CastCut::take(&crate::term_lock(&term), &watermark);
    let holding = |cut: &CastCut| cast.lock().unwrap().holding(cut);
    assert_eq!(holding(&cut()), Holding::Exact, "a fresh session");

    let cuts = rig::hold(&term, &cast, b"a");
    let shows_a = cut();
    assert_eq!(
        holding(&shows_a),
        Holding::Behind,
        "the burst is not recorded yet"
    );
    cast.lock()
        .unwrap()
        .record_burst(Arc::from(&b"a"[..]), cuts, Duration::ZERO);
    assert_eq!(holding(&shows_a), Holding::Exact);
    rig::burst(&term, &cast, Duration::from_millis(1), b"b");
    assert_eq!(holding(&shows_a), Holding::Past, "a later burst is in");

    let first = rig::resize_hold(&term, &cast, Duration::from_millis(2), 7, 20);
    let second = rig::resize_hold(&term, &cast, Duration::from_millis(3), 8, 20);
    let shows_both = cut();
    assert_eq!(holding(&shows_both), Holding::Behind);
    cast.lock().unwrap().record_resize_stamped(second, 20, 8);
    assert_eq!(
        holding(&shows_both),
        Holding::Behind,
        "the first resize is still missing"
    );
    cast.lock().unwrap().record_resize_stamped(first, 20, 7);
    assert_eq!(holding(&shows_both), Holding::Exact);

    let fresh = Mutex::new(Terminal::new(8, 20));
    let fresh_cut = CastCut::take(&crate::term_lock(&fresh), &watermark);
    assert_eq!(
        holding(&fresh_cut),
        Holding::Exact,
        "no resize of the newer engine"
    );
    rig::resize(&fresh, &cast, Duration::from_millis(4), 6, 20);
    assert_eq!(holding(&fresh_cut), Holding::Past);
    assert_eq!(
        holding(&shows_both),
        Holding::Past,
        "a newer engine's resize is in"
    );
}

/// Parse one event line `[<f64>, "<code>", "<json-string>"]` into
/// `(t, code, unescaped_payload)`, as strictly as an asciinema-style reader.
fn parse_event(line: &str) -> (f64, String, String) {
    let inner = line
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or_else(|| panic!("not a JSON array: {line}"));
    let (t_str, after_t) = inner.split_once(',').expect("missing comma after t");
    let t: f64 = t_str.trim().parse().expect("t is not an f64");
    let after_t = after_t.trim_start();
    let (code, after_code) = parse_json_string(after_t);
    let after_code = after_code.trim_start();
    let after_code = after_code
        .strip_prefix(',')
        .expect("missing comma after code");
    let (data, tail) = parse_json_string(after_code.trim_start());
    assert!(tail.trim().is_empty(), "trailing junk after data: {tail:?}");
    (t, code, data)
}

/// Parse a leading JSON string literal, returning (unescaped, remainder).
fn parse_json_string(s: &str) -> (String, &str) {
    let chars: Vec<char> = s.chars().collect();
    assert_eq!(
        chars.first().copied(),
        Some('"'),
        "expected a JSON string at {s:?}"
    );
    let mut out = String::new();
    let mut ci = 1;
    while ci < chars.len() {
        match chars[ci] {
            '"' => {
                let consumed: usize = chars[..=ci].iter().map(|c| c.len_utf8()).sum();
                return (out, &s[consumed..]);
            }
            '\\' => {
                ci += 1;
                match chars[ci] {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let hex: String = chars[ci + 1..ci + 5].iter().collect();
                        let cp = u32::from_str_radix(&hex, 16).expect("bad \\u");
                        out.push(char::from_u32(cp).expect("bad codepoint"));
                        ci += 4;
                    }
                    other => panic!("unknown escape \\{other}"),
                }
            }
            other => out.push(other),
        }
        ci += 1;
    }
    panic!("unterminated JSON string: {s:?}");
}
