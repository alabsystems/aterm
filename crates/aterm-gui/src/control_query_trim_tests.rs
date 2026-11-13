// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Arc, Mutex};

use aterm_core::terminal::Terminal;

use super::{
    ROWS_REVERSED, TEXT_USAGE, TextArgs, TextShape, cmd_blocktext_args, cmd_text_json_opt,
    frame_rows_reply, lines_reply, text_args,
};

fn cmd_text_opt(term: &Arc<Mutex<Terminal>>, args: TextArgs) -> String {
    super::cmd_text_opt(term, &crate::frame_memo::FrameSlot::default(), args)
}

#[test]
fn lines_reply_names_truncation_only_when_something_was_lost() {
    assert_eq!(lines_reply(0, 0), "OK 0\n");
    assert_eq!(lines_reply(28_171, 0), "OK 28171\n");
    assert_eq!(lines_reply(28_171, 91_357), "OK 28171 truncated=91357\n");
    assert_eq!(lines_reply(0, 1), "OK 0 truncated=1\n");
}

const TRIM: TextArgs = TextArgs {
    trim: true,
    shape: TextShape::All,
    display: false,
};

fn args(rest: &str) -> TextArgs {
    text_args(rest).unwrap_or_else(|e| panic!("{rest:?} parses: {e}"))
}

#[test]
fn text_order_display_reads_a_right_to_left_run_as_painted() {
    assert!(args("order=display").display);
    assert!(!args("order=logical trim").display);
    for bad in ["order=visual", "order=display order=display", "order="] {
        assert_eq!(text_args(bad), Err(TEXT_USAGE.to_string()), "{bad:?}");
    }
    let term = Arc::new(Mutex::new(Terminal::new(3, 10)));
    term.lock()
        .unwrap()
        .process("ab \u{05D0}\u{05D1}\r\nplain".as_bytes());
    let logical = cmd_text_opt(&term, args("rows=0-1"));
    assert!(logical.contains("ab \u{05D0}\u{05D1}\n"), "{logical:?}");
    let display = cmd_text_opt(&term, args("rows=0-1 order=display"));
    assert!(
        display.contains("ab \u{05D1}\u{05D0}\n"),
        "painted order: {display:?}"
    );
    assert!(
        display.contains("plain\n"),
        "an LTR row is unchanged: {display:?}"
    );
    let json = cmd_text_json_opt(&term, args("rows=0-1 order=display"));
    assert!(
        json.contains("\"rows\":[\"ab \u{05D1}\u{05D0}\",\"plain\"]"),
        "{json}"
    );
    assert!(
        json.contains(",\"order\":\"display\",\"display_map\":[[0,1,2,4,3,5,6,7,8,9],null]"),
        "{json}"
    );
    let plain = cmd_text_json_opt(&term, args("rows=0-1"));
    assert!(!plain.contains("display_map"), "{plain}");
}

#[test]
fn text_args_accepts_trim_and_one_shape_in_any_order() {
    let all = TextArgs::default();
    assert_eq!(text_args(""), Ok(all));
    assert_eq!(text_args("  "), Ok(all));
    assert_eq!(text_args("trim"), Ok(TRIM));
    assert_eq!(text_args(" trim "), Ok(TRIM));
    let tail5 = TextArgs {
        trim: false,
        shape: TextShape::Tail(5),
        display: false,
    };
    assert_eq!(text_args("tail=5"), Ok(tail5));
    assert_eq!(
        text_args("tail=5 trim"),
        Ok(TextArgs {
            trim: true,
            ..tail5
        })
    );
    assert_eq!(
        text_args("trim tail=5"),
        Ok(TextArgs {
            trim: true,
            ..tail5
        }),
        "any order"
    );
    assert_eq!(
        text_args("rows=20-23"),
        Ok(TextArgs {
            trim: false,
            shape: TextShape::Rows(20, 23),
            display: false,
        })
    );
    assert_eq!(
        text_args("rows=7-7 trim"),
        Ok(TextArgs {
            trim: true,
            shape: TextShape::Rows(7, 7),
            display: false,
        }),
        "a one-row span is a span"
    );
    for bad in [
        "foo",
        "20",
        "trim extra",
        "TRIM",
        "trim=1",
        "--trim",
        "trim trim",
        "tail=0",
        "tail=",
        "tail=x",
        "tail=-1",
        "tail",
        "tail=5 tail=6",
        "tail=5 rows=1-2",
        "rows=5",
        "rows=a-b",
        "rows=5-",
        "rows=-5",
        "rows=1-2-3",
        "rows",
    ] {
        assert_eq!(
            text_args(bad),
            Err(TEXT_USAGE.to_string()),
            "{bad:?} is rejected, never silently ignored"
        );
    }
    assert_eq!(
        text_args("rows=5-2"),
        Err(ROWS_REVERSED.to_string()),
        "well-formed, but names no row on any grid"
    );
}

fn term_with(rows: u16, seed: &[u8]) -> Arc<Mutex<Terminal>> {
    let mut t = Terminal::new(rows, 40);
    t.process(seed);
    Arc::new(Mutex::new(t))
}

/// `"title"` is the program's OSC 0/2 title, read in the rows' own hold, escaped, and `""` when none was set.
#[test]
fn text_json_carries_the_title_read_with_its_rows() {
    let decoded = |term: &Arc<Mutex<Terminal>>| {
        let reply = cmd_text_json_opt(term, TextArgs::default());
        let body = reply.strip_prefix("OK 1\n").expect("an OK 1 reply");
        let v: aterm_json::Value = aterm_json::from_str(body.trim()).expect("valid JSON");
        (reply, v)
    };
    let (bare, v) = decoded(&term_with(4, b"\xe2\x9c\xb3 idle\r\n"));
    assert_eq!(
        v.get("title").and_then(aterm_json::Value::as_str),
        Some(""),
        "NEGATIVE CONTROL: no OSC 2, an empty title: {bare}"
    );
    let running = term_with(
        4,
        "\x1b]2;\u{25d0} Claude Code\x07\u{23fa} streaming\r\n".as_bytes(),
    );
    let (reply, v) = decoded(&running);
    assert_eq!(
        v.get("title").and_then(aterm_json::Value::as_str),
        Some("\u{25d0} Claude Code"),
        "{reply}"
    );
    assert!(
        reply.contains(",\"gen\":") && reply.find("\"gen\":") < reply.find("\"title\":"),
        "after the generation it shares a hold with: {reply}"
    );
    assert!(
        reply.contains("\"title\":\"\u{25d0} Claude Code\",\"human_ms\":null"),
        "{reply}"
    );
    running
        .lock()
        .unwrap()
        .process(b"\x1b]0;say \"hi\" \\ there\x07");
    let (quoted, v) = decoded(&running);
    assert!(
        quoted.contains(r#""title":"say \"hi\" \\ there""#),
        "a quote and a backslash are escaped: {quoted}"
    );
    assert_eq!(
        v.get("title").and_then(aterm_json::Value::as_str),
        Some(r#"say "hi" \ there"#),
        "and decode back"
    );
    assert_eq!(
        v.get("rows"),
        decoded(&term_with(4, "\u{23fa} streaming\r\n".as_bytes()))
            .1
            .get("rows"),
        "the title is no row"
    );
}

#[test]
fn text_json_carries_the_generation_of_the_rows_it_sends() {
    const ALT_A: &[u8] = b"\x1b[?1049h\x1b[H Bash command\r\n   ls -la /tmp/work\r\n";
    const ALT_B: &[u8] = b"\x1b[H Bash command\r\n   rm -rf /tmp/work\r\n";
    let term = term_with(6, ALT_A);
    let field = |json: &str, key: &str| {
        let at = json.find(&format!("\"{key}\":")).expect(key) + key.len() + 3;
        json[at..]
            .split([',', '}'])
            .next()
            .unwrap()
            .trim_matches('"')
            .to_string()
    };
    let a = cmd_text_json_opt(&term, TextArgs::default());
    let gen_a = super::screen_gen(&term.lock().unwrap()).to_string();
    assert_eq!(field(&a, "gen"), gen_a, "{a}");
    term.lock()
        .unwrap()
        .process(&[b"\x1b[?1049l\x1b[?1049h".as_slice(), ALT_B].concat());
    let b = cmd_text_json_opt(&term, TextArgs::default());
    assert!(b.contains("rm -rf"), "PRECONDITION: box B is up: {b}");
    assert_eq!(
        field(&a, "seq"),
        field(&b, "seq"),
        "NEGATIVE CONTROL: seq repeats"
    );
    assert_ne!(field(&a, "gen"), field(&b, "gen"), "the generation moved");
    assert_eq!(
        field(&b, "gen"),
        super::screen_gen(&term.lock().unwrap()).to_string()
    );
    let tail = cmd_text_json_opt(&term, args("tail=2 trim"));
    assert_eq!(field(&tail, "gen"), field(&b, "gen"), "{tail}");
}

#[test]
fn text_json_carries_the_persons_stamp_after_gen() {
    let term = term_with(24, &numbered(22));
    let never = super::cmd_text_json_read(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        TextArgs::default(),
        || (None, None, 0),
        None,
        None,
    );
    assert!(
        never.contains(",\"human_ms\":null,\"human_seq\":0,\"launch\":null}"),
        "{never}"
    );
    let keyed = super::cmd_text_json_read(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        TextArgs::default(),
        || (Some(12_345), None, 3),
        None,
        None,
    );
    let gen_at = keyed.find("\"gen\":").expect("gen");
    let human_at = keyed
        .find(",\"human_ms\":12345,\"human_seq\":3,\"launch\":null}")
        .expect("human_ms, human_seq and launch, closing");
    assert!(gen_at < human_at, "{keyed}");
    let shaped = super::cmd_text_json_read(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        args("tail=5 trim"),
        || (Some(7), None, 9),
        None,
        None,
    );
    assert!(
        shaped.ends_with(
            ",\"human_ms\":7,\"human_seq\":9,\"launch\":null,\"trimmed\":2,\"first\":19}\n"
        ),
        "{shaped}"
    );
    assert_eq!(cmd_text_json_opt(&term, TextArgs::default()), never);
    let served = super::cmd_text_json_read(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        args("tail=5 trim"),
        || (None, Some(5_000), 0),
        None,
        None,
    );
    assert!(
        served.ends_with(
            ",\"human_ms\":null,\"human_seq\":0,\"launch\":null,\"trimmed\":2,\"first\":19,\
             \"unkeyed_ms\":5000}\n"
        ),
        "{served}"
    );
    let keyed = super::cmd_text_json_read(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        TextArgs::default(),
        || (Some(7), Some(5_000), 0),
        None,
        None,
    );
    assert!(!keyed.contains("unkeyed_ms"), "{keyed}");
    // The person's typing rides apart where it is not their last gesture (a scroll since),
    // and stays out where it is.
    let typed = |typed_ms| {
        super::cmd_text_json_read(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            TextArgs::default(),
            || super::PersonStamps {
                human_ms: Some(7),
                unkeyed_ms: None,
                human_seq: 9,
                typed_ms,
            },
            None,
            None,
        )
    };
    let scrolled = typed(Some(7_200_000));
    assert!(
        scrolled.ends_with(",\"human_typed_ms\":7200000}\n"),
        "{scrolled}"
    );
    assert!(
        typed(None).ends_with(",\"human_typed_ms\":null}\n"),
        "never typed"
    );
    assert!(!typed(Some(7)).contains("human_typed_ms"), "the same stamp");
}

#[test]
fn text_json_judges_the_launch_witness_by_its_own_count() {
    use crate::launch_witness::{Launch, LaunchWitness, Why};
    let term = term_with(24, &numbered(22));
    let w = LaunchWitness::default();
    w.mark_for_test(Launch {
        id: 4,
        seq: 9,
        word: Some("claude"),
        why: Why::Person,
    });
    let at = super::cmd_text_json_read(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        args("tail=5 trim"),
        || (Some(7), None, 9),
        Some(&w),
        None,
    );
    assert!(
        at.ends_with(
            ",\"human_seq\":9,\"launch\":{\"id\":4,\"seq\":9,\"word\":\"claude\",\
             \"why\":\"person\"},\"trimmed\":2,\"first\":19}\n"
        ),
        "{at}"
    );
    let moved = super::cmd_text_json_read(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        TextArgs::default(),
        || (Some(7), None, 10),
        Some(&w),
        None,
    );
    assert!(
        moved.ends_with(",\"human_seq\":10,\"launch\":{\"why\":\"since\"}}\n"),
        "{moved}"
    );
}

#[cfg(unix)]
#[test]
fn text_json_ends_with_the_sessions_submission_count() {
    use crate::input::{EgressMode, InputEvent};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    use std::io::Read as _;
    let term = term_with(24, &numbered(22));
    let tracker = std::sync::Arc::new(crate::app_input::OutputEchoTracker::default());
    let (mut peer, master) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    peer.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("hang detector");
    master.set_nonblocking(true).expect("nonblocking master");
    let sink = aterm_session::sink::SinkWriter::new_owned(master.into());
    sink.note_master_nonblocking(true);
    sink.install_spill_arranger(|| {});
    let read = |args: TextArgs| {
        super::cmd_text_json_read(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            args,
            || (None, Some(5_000), 0),
            None,
            Some((&*tracker, &sink)),
        )
    };
    let before = read(args("tail=5 trim"));
    assert!(
        before.ends_with(
            ",\"launch\":null,\"trimmed\":2,\"first\":19,\"unkeyed_ms\":5000,\"inputs\":[0,0],\
             \"submits\":0}\n"
        ),
        "{before}"
    );
    let modes = crate::mode_mirror_of(&term);
    let enter = InputEvent::Key {
        key: Key::Named(NamedKey::Enter),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let write = |ev: &InputEvent| {
        crate::app_input::tracked_egress(
            &term,
            &modes,
            &sink,
            &tracker,
            ev,
            EgressMode::Interactive,
        )
    };
    write(&enter);
    assert!(read(TextArgs::default()).ends_with(",\"submits\":0}\n"));
    write(&InputEvent::KeySequence(b"keep going".to_vec()));
    assert!(
        read(TextArgs::default()).ends_with(",\"submits\":0}\n"),
        "a draft is no submission"
    );
    write(&enter);
    let after = read(TextArgs::default());
    assert!(after.ends_with(",\"submits\":1}\n"), "{after}");
    assert!(after.contains(",\"inputs\":[3,3],"), "{after}");
    let flight = tracker.begin_event(&enter);
    let held = read(TextArgs::default());
    assert!(held.contains(",\"inputs\":[null,4],"), "{held}");
    drop(flight);
    let over = read(TextArgs::default());
    assert!(over.contains(",\"inputs\":[4,4],"), "{over}");
    let landing = std::sync::Arc::clone(&tracker);
    let key = enter.clone();
    super::WHILE_ROWS_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || drop(landing.begin_event(&key))));
    });
    let during = read(TextArgs::default());
    assert!(
        super::WHILE_ROWS_READ.with(|hook| hook.borrow().is_none()),
        "the write was made inside the read"
    );
    assert!(during.contains(",\"inputs\":[4,5],"), "{during}");
    let fence = sink.try_fence_egress().expect("egress initially settled");
    let held = read(TextArgs::default());
    assert!(held.contains(",\"inputs\":[null,5],"), "{held}");
    let receipt = write(&InputEvent::KeySequence(vec![0x1b]));
    assert!(receipt.fully_accepted_nonempty() && !receipt.is_direct());
    assert_eq!(sink.spilled_bytes(), 1, "the clear key is still buffered");
    let buffered = read(TextArgs::default());
    assert!(buffered.contains(",\"inputs\":[null,6],"), "{buffered}");
    assert!(buffered.ends_with(",\"submits\":1}\n"), "{buffered}");
    drop(fence);
    sink.arrange_pending_drainer();
    let expected = b"\rkeep going\r\x1b";
    let mut delivered = vec![0; expected.len()];
    peer.read_exact(&mut delivered)
        .expect("the owned peer receives the bytes");
    assert_eq!(delivered.as_slice(), expected);
    assert!(
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(2),
            || tracker.input_settled(&sink).filter(|&n| n == 6),
        )
        .is_some(),
        "the drainer releases its final frame hold"
    );
    let drained = read(TextArgs::default());
    assert!(drained.contains(",\"inputs\":[6,6],"), "{drained}");
    let bare = cmd_text_json_opt(&term, TextArgs::default());
    assert!(
        !bare.contains("submits") && !bare.contains("inputs"),
        "{bare}"
    );
}

#[test]
fn frame_rows_reply_places_the_verdict_then_trimmed() {
    let body = "a\n\nb\n\n\n";
    assert_eq!(
        frame_rows_reply(body, 5, "", false, 0),
        "OK 5\na\n\nb\n\n\n"
    );
    assert_eq!(
        frame_rows_reply(body, 5, "", true, 0),
        "OK 3 trimmed=2\na\n\nb\n"
    );
    let v = "turn submitted=1 status=settled seq=9 id=2 dur_ms=3 hash=00000000000000ab";
    assert_eq!(
        frame_rows_reply(body, 5, v, false, 0),
        format!("OK 5 {v}\na\n\nb\n\n\n")
    );
    assert_eq!(
        frame_rows_reply(body, 5, v, true, 0),
        format!("OK 3 {v} trimmed=2\na\n\nb\n")
    );
    assert_eq!(
        frame_rows_reply("\n\n", 2, v, true, 0),
        format!("OK 0 {v} trimmed=2\n")
    );
    assert_eq!(
        frame_rows_reply(body, 5, "", false, 19),
        "OK 5 first=19\na\n\nb\n\n\n"
    );
    assert_eq!(
        frame_rows_reply(body, 5, "", true, 19),
        "OK 3 trimmed=2 first=19\na\n\nb\n"
    );
    assert_eq!(
        frame_rows_reply(body, 5, v, true, 3),
        format!("OK 3 {v} trimmed=2 first=3\na\n\nb\n")
    );
}

fn numbered(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| format!("r{i}"))
        .collect::<Vec<_>>()
        .join("\r\n")
        .into_bytes()
}

#[test]
fn text_tail_and_rows_send_a_slice_and_say_where_it_starts() {
    let term = term_with(24, &numbered(24));
    let whole = cmd_text_opt(&term, TextArgs::default());
    assert!(whole.starts_with("OK 24\nr0\nr1\n"), "{whole}");
    assert_eq!(
        cmd_text_opt(&term, args("tail=5")),
        "OK 5 first=19\nr19\nr20\nr21\nr22\nr23\n"
    );
    assert_eq!(
        cmd_text_opt(&term, args("rows=20-23")),
        "OK 4 first=20\nr20\nr21\nr22\nr23\n"
    );
    assert_eq!(
        cmd_text_opt(&term, args("rows=23-99")),
        "OK 1 first=23\nr23\n",
        "the span's end is clamped to the grid"
    );
    assert_eq!(
        cmd_text_opt(&term, args("rows=0-0")),
        "OK 1\nr0\n",
        "row 0 carries no first="
    );
    assert_eq!(
        cmd_text_opt(&term, args("rows=99-100")),
        super::rows_off_grid(24),
        "no row of the span is on the grid"
    );
    assert_eq!(cmd_text_opt(&term, args("tail=99")), whole);
    assert_eq!(cmd_text_opt(&term, args("tail=24")), whole);
    assert_eq!(cmd_text_opt(&term, args("rows=0-99")), whole);
    assert_eq!(
        cmd_text_opt(&term, args(&format!("rows=0-{}", usize::MAX))),
        whole,
        "a span ending at usize::MAX clamps, never overflows"
    );
    assert!(!whole.contains("first="));
}

#[test]
fn text_trim_composes_with_the_shape_and_trims_the_slice() {
    let term = term_with(24, &numbered(22));
    assert_eq!(
        cmd_text_opt(&term, TRIM),
        {
            let mut want = String::from("OK 22 trimmed=2\n");
            for i in 0..22 {
                want.push_str(&format!("r{i}\n"));
            }
            want
        },
        "whole-grid trim is unchanged"
    );
    let want = "OK 3 trimmed=2 first=19\nr19\nr20\nr21\n";
    assert_eq!(cmd_text_opt(&term, args("tail=5 trim")), want);
    assert_eq!(cmd_text_opt(&term, args("trim tail=5")), want, "any order");
    assert_eq!(
        cmd_text_opt(&term, args("rows=19-23 trim")),
        want,
        "the same slice by span"
    );
    assert_eq!(
        cmd_text_opt(&term, args("rows=22-23 trim")),
        "OK 0 trimmed=2 first=22\n",
        "an all-blank slice trims to nothing and still says where it was"
    );
    assert_eq!(
        cmd_text_opt(&term, args("rows=0-4 trim")),
        "OK 5 trimmed=0\nr0\nr1\nr2\nr3\nr4\n",
        "a slice with no blank tail trims nothing; row 0 carries no first="
    );
}

#[test]
fn text_json_carries_first_for_a_shaped_read() {
    let term = term_with(24, &numbered(22));
    let json = cmd_text_json_opt(&term, args("tail=5"));
    assert!(
        json.starts_with("OK 1\n{\"rows\":[\"r19\",\"r20\",\"r21\",\"\",\"\"],"),
        "{json}"
    );
    assert!(
        json.contains("\"dims\":{\"rows\":24,\"cols\":40}"),
        "dims stay the grid: {json}"
    );
    assert!(json.ends_with(",\"first\":19}\n"), "{json}");
    assert!(!json.contains("trimmed"), "{json}");
    let trimmed = cmd_text_json_opt(&term, args("tail=5 trim"));
    assert!(
        trimmed.starts_with("OK 1\n{\"rows\":[\"r19\",\"r20\",\"r21\"],"),
        "{trimmed}"
    );
    assert!(
        trimmed.ends_with(",\"trimmed\":2,\"first\":19}\n"),
        "{trimmed}"
    );
    let span = cmd_text_json_opt(&term, args("rows=20-21"));
    assert!(
        span.starts_with("OK 1\n{\"rows\":[\"r20\",\"r21\"],"),
        "{span}"
    );
    assert!(span.ends_with(",\"first\":20}\n"), "{span}");
    assert_eq!(
        cmd_text_json_opt(&term, args("rows=99-100")),
        super::rows_off_grid(24)
    );
    let whole = cmd_text_json_opt(&term, args("tail=99"));
    assert_eq!(
        whole,
        cmd_text_json_opt(&term, TextArgs::default()),
        "a tail larger than the grid is the bare JSON"
    );
    assert!(!whole.contains("\"first\""), "{whole}");
}

#[test]
fn blocktext_args_validate_grammar_and_pass_trim_to_the_emitter() {
    let emit = |id: u64, trim: bool| format!("OK {id} trim={trim}\n");
    assert_eq!(cmd_blocktext_args("7", emit), "OK 7 trim=false\n");
    assert_eq!(cmd_blocktext_args(" 7  trim ", emit), "OK 7 trim=true\n");
    for bad in ["", "trim", "x", "7 foo", "7 trim foo", "7trim", "-7"] {
        assert_eq!(
            cmd_blocktext_args(bad, |_, _| unreachable!("usage errors never emit")),
            "ERR usage: blocktext <id> [trim]\n",
            "{bad:?}"
        );
    }
    assert_eq!(
        cmd_blocktext_args("7 trim", |_, _| "ERR no such block\n".to_string()),
        "ERR no such block\n",
        "an emitter error is the reply"
    );
}
