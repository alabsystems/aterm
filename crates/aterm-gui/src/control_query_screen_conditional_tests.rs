// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SCREEN_USAGE, screen_args, screen_etag};
use aterm_core::terminal::Terminal;
use std::sync::{Arc, Mutex};

fn cmd_screen_styled_json(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    super::cmd_screen_styled_json(term, &crate::frame_memo::FrameSlot::default(), rest)
}

fn frame(term: &Arc<Mutex<Terminal>>) -> String {
    let out = cmd_screen_styled_json(term, "");
    let at = out
        .rfind(",\"etag\":")
        .expect("a full frame carries its etag");
    out[..at].to_string()
}

fn etag(term: &Arc<Mutex<Terminal>>) -> u64 {
    screen_etag(&term.lock().expect("mutex"))
}

#[test]
fn an_alt_screen_re_entry_at_the_same_seq_is_not_unchanged() {
    let term = Arc::new(Mutex::new(Terminal::new(6, 30)));
    let box_a = b"\x1b[?1049h\x1b[H ls -la /tmp/work\r\n [y/n]\r\n";
    let box_b = b"\x1b[?1049l\x1b[?1049h\x1b[H rm -rf /tmp/work\r\n [y/n]\r\n";
    term.lock().expect("mutex").process(box_a);
    let (seq_a, frame_a, etag_a) = {
        let seq = term.lock().expect("mutex").content_seq();
        (seq, frame(&term), etag(&term))
    };
    term.lock().expect("mutex").process(box_b);
    let seq_b = term.lock().expect("mutex").content_seq();
    assert_eq!(seq_a, seq_b, "the premise: both boxes read the same seq");
    assert_ne!(frame(&term), frame_a, "the premise: the frames differ");
    assert_ne!(
        etag(&term),
        etag_a,
        "a re-entered alt screen is a new generation"
    );
    let reply = cmd_screen_styled_json(&term, &format!("since={etag_a:016x}"));
    assert!(
        reply.contains("rm -rf") || reply.contains("\"glyph\""),
        "{reply}"
    );
    assert!(
        !reply.contains("\"unchanged\""),
        "box B served as box A: {reply}"
    );
}

#[test]
fn a_kitty_animation_frame_step_is_not_unchanged() {
    let (cw, ch) = (10usize, 20usize);
    let t0 = aterm_time::Instant::now();
    let at = |ms: u64| aterm_core::terminal::ClockReading {
        monotonic: t0 + std::time::Duration::from_millis(ms),
        wall_ms: None,
    };
    let fill = |rgba: [u8; 4]| {
        let raw: Vec<u8> = rgba.iter().copied().cycle().take(cw * ch * 4).collect();
        aterm_codec::base64::encode(&raw).expect("encode")
    };
    let (red, blue) = (fill([230, 20, 20, 255]), fill([20, 20, 230, 255]));
    let mut t = Terminal::new(4, 8);
    t.set_cell_pixel_size(cw as u16, ch as u16);
    for command in [
        format!("\x1b_Ga=t,f=32,s={cw},v={ch},i=7,q=2;{red}\x1b\\"),
        format!("\x1b_Ga=f,f=32,s={cw},v={ch},i=7,z=30,q=2;{blue}\x1b\\"),
        "\x1b_Ga=a,i=7,r=1,z=30\x1b\\".to_string(),
        "\x1b[1;1H\x1b_Ga=p,i=7,C=1,q=2\x1b\\".to_string(),
        "\x1b_Ga=a,i=7,s=3\x1b\\".to_string(),
    ] {
        t.process_at(command.as_bytes(), at(0));
    }
    let term = Arc::new(Mutex::new(t));
    let (frame_0, etag_0) = (frame(&term), etag(&term));
    assert!(frame_0.contains("\"b64\":"), "the premise: an image shows");
    let step = term
        .lock()
        .expect("mutex")
        .advance_kitty_animations(at(30).monotonic);
    assert!(step.advanced, "the premise: the animation stepped");
    let (frame_1, etag_1) = (frame(&term), etag(&term));
    assert_ne!(frame_1, frame_0, "the premise: the served frame moved");
    assert_ne!(etag_1, etag_0, "an animation frame step is not unchanged");
    let reply = cmd_screen_styled_json(&term, &format!("since={etag_0:016x}"));
    assert!(
        !reply.contains("\"unchanged\""),
        "the verb serves the stepped frame: {reply}"
    );
}

#[test]
fn another_terminal_at_the_same_address_is_not_unchanged() {
    let term = Arc::new(Mutex::new(Terminal::new(4, 12)));
    term.lock().expect("mutex").process(b"hello");
    let (frame_0, etag_0) = (frame(&term), etag(&term));
    let (seq_0, gen_0) = {
        let t = term.lock().expect("mutex");
        (t.content_seq(), super::screen_gen(&t).to_string())
    };
    let mut other = Terminal::new(4, 12);
    other.process(b"world");
    *term.lock().expect("mutex") = other;
    {
        let t = term.lock().expect("mutex");
        assert_eq!(
            (t.content_seq(), super::screen_gen(&t).to_string()),
            (seq_0, gen_0),
            "the premise: the counters agree"
        );
    }
    assert_ne!(frame(&term), frame_0, "the premise: other text shows");
    let reply = cmd_screen_styled_json(&term, &format!("since={etag_0:016x}"));
    assert!(
        !reply.contains("\"unchanged\""),
        "another terminal's screen is never the first one's: {reply}"
    );
}

#[test]
fn conditional_screen_answers_unchanged_only_when_unchanged() {
    let term = Arc::new(Mutex::new(Terminal::new(4, 12)));
    term.lock().expect("mutex").process(b"first");
    let full = cmd_screen_styled_json(&term, "");
    assert!(
        full.starts_with("OK 1\n") && full.contains("\"glyph\""),
        "{full}"
    );
    let tag = etag(&term);
    assert!(
        full.trim_end()
            .ends_with(&format!(",\"etag\":\"{tag:016x}\"}}")),
        "the full frame ends with the etag the next poll sends back: {full}"
    );
    for arg in [
        format!("since={tag:016x}"),
        format!("--json since={tag:016x}"),
    ] {
        let cond = cmd_screen_styled_json(&term, &arg);
        assert!(cond.starts_with("OK 1\n"), "{arg}: same framing: {cond}");
        assert!(cond.contains("\"unchanged\":true"), "{arg}: {cond}");
        assert!(
            cond.contains("\"gen\":\""),
            "{arg}: names the generation: {cond}"
        );
        assert!(
            !cond.contains("\"glyph\""),
            "{arg}: no grid on an unchanged reply"
        );
        assert!(
            cond.len() * 4 < full.len(),
            "{arg}: far smaller than the frame"
        );
    }
    term.lock().expect("mutex").process(b" second");
    let after = cmd_screen_styled_json(&term, &format!("since={tag:016x}"));
    assert!(
        after.contains("\"glyph\"") && !after.contains("\"unchanged\""),
        "{after}"
    );
}

#[test]
fn screen_takes_since_and_json_and_refuses_the_rest() {
    assert_eq!(screen_args(""), Ok(None));
    assert_eq!(screen_args("--json"), Ok(None));
    assert_eq!(screen_args("since=00000000000000ff"), Ok(Some(255)));
    assert_eq!(screen_args("--json since=00000000000000FF"), Ok(Some(255)));
    assert_eq!(screen_args("since=00000000000000ff --json"), Ok(Some(255)));
    for bad in [
        "since=",
        "since=ff",
        "since=zzzzzzzzzzzzzzzz",
        "since=00000000000000ff since=00000000000000ff",
        "since=000000000000000000ff",
        "trim",
        "since",
    ] {
        assert_eq!(screen_args(bad), Err(SCREEN_USAGE.to_string()), "{bad:?}");
    }
    let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
    assert_eq!(cmd_screen_styled_json(&term, "trim"), SCREEN_USAGE);
}

#[test]
fn text_json_says_open_only_inside_an_unfinished_frame() {
    let term = Arc::new(Mutex::new(Terminal::new(4, 40)));
    term.lock().unwrap().process(b"busy\r\n");
    let read = || {
        super::cmd_text_json_read(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            super::TextArgs::default(),
            || (None, None, 0),
            None,
            None,
        )
    };
    assert!(!read().contains("\"frame\""), "no bracket");
    term.lock().unwrap().process(b"\x1b[?2026h");
    assert!(!read().contains("\"frame\""), "nothing written in it");
    term.lock().unwrap().process(b"\x1b[2;1Hhalf");
    assert!(read().ends_with(",\"frame\":\"open\"}\n"), "{}", read());
    term.lock().unwrap().process(b"\x1b[?2026l");
    assert!(!read().contains("\"frame\""), "closed");
    let begun = std::time::Instant::now()
        .checked_sub(aterm_core::terminal::FRAME_HANG + std::time::Duration::from_secs(1))
        .expect("an instant a minute back");
    term.lock().unwrap().process_at(
        b"\x1b[?2026h\x1b[3;1Hhung",
        aterm_core::terminal::ClockReading {
            monotonic: begun,
            wall_ms: None,
        },
    );
    assert!(term.lock().unwrap().sync_frame_unfinished());
    assert!(!read().contains("\"frame\""), "past its hang detector");
}
