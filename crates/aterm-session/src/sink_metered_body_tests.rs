// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn bracketed(body: &[u8]) -> Vec<u8> {
    [PASTE_OPEN, body, PASTE_CLOSE].concat()
}

#[derive(Clone, Copy)]
enum StopAt {
    Never,
    After(usize),
    Inside(usize),
}

fn windows_body(frame: &[u8], stop: StopAt) -> (MeteredEnd, usize, Vec<u8>) {
    let shared = Shared::new();
    let meter = BulkMeter::new();
    let since = shared.discard_epoch();
    let mut conpty = Vec::new();
    let mut writes = 0usize;
    let mut conpty_write = |bytes: &[u8], flag: Option<&AtomicBool>| {
        writes += 1;
        if matches!(stop, StopAt::Inside(n) if n == writes) {
            meter.request_stop();
        }
        if flag.is_some_and(|f| f.load(Ordering::Acquire)) {
            return Ok(0);
        }
        conpty.extend_from_slice(bytes);
        if matches!(stop, StopAt::After(n) if n == writes) {
            meter.request_stop();
        }
        Ok(bytes.len())
    };
    let end = write_metered(frame, &meter, |rest| {
        shared.write_stamped_through(rest, since, Some(&meter), &mut conpty_write)
    })
    .expect("an in-memory write never fails");
    let owed = deliver_owed_through(&end.owed, |rest| {
        shared.write_stamped_through(rest, since, None, &mut conpty_write)
    });
    let accepted = end.accepted + owed;
    (end, accepted, conpty)
}

#[test]
fn a_stop_reaches_the_windows_write_and_the_paste_ends_closed() {
    let mut body = vec![b'p'; METERED_SLICE - PASTE_OPEN.len() - 1];
    body.extend_from_slice("\u{e9}".as_bytes());
    body.extend(vec![b'q'; 3 * METERED_SLICE]);
    let frame = bracketed(&body);
    let mut want = frame[..METERED_SLICE + 1].to_vec();
    want.extend_from_slice(PASTE_CLOSE);
    for stop in [StopAt::After(1), StopAt::Inside(2)] {
        let (end, accepted, conpty) = windows_body(&frame, stop);
        assert_eq!(
            end.owed,
            [&frame[METERED_SLICE..=METERED_SLICE], PASTE_CLOSE].concat(),
            "the write the stop reached wrote nothing; the cut's debt came back"
        );
        assert_eq!(conpty, want, "the head, the rest of `é`, the close");
        assert_eq!(accepted, conpty.len());
        assert!(std::str::from_utf8(&conpty).is_ok(), "no half character");
    }
    let (end, accepted, conpty) = windows_body(&frame, StopAt::Inside(1));
    assert!(end.owed.is_empty());
    assert_eq!((accepted, conpty.len()), (0, 0));
}

#[test]
fn the_windows_twin_writes_an_unstopped_frame_whole() {
    let frame = bracketed(&vec![b'p'; 3 * METERED_SLICE]);
    let (end, accepted, conpty) = windows_body(&frame, StopAt::Never);
    assert_eq!(conpty, frame);
    assert_eq!(accepted, frame.len());
    assert!(end.owed.is_empty());
}

#[test]
fn the_close_a_windows_stop_owes_waits_until_a_sever_releases_it() {
    let shared = Shared::new();
    let meter = BulkMeter::new();
    let since = shared.discard_epoch();
    let frame = bracketed(&vec![b'p'; 3 * METERED_SLICE]);
    let mut conpty = Vec::new();
    let mut writes = Vec::new();
    let mut conpty_write = |bytes: &[u8], flag: Option<&AtomicBool>| {
        writes.push(flag.is_some());
        match writes.len() {
            1 => {
                conpty.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            2 => {
                meter.request_stop();
                Ok(if flag.is_some_and(|f| f.load(Ordering::Acquire)) {
                    0
                } else {
                    bytes.len()
                })
            }
            _ => {
                meter.request_stop();
                if flag.is_some_and(|f| f.load(Ordering::Acquire)) {
                    return Ok(0);
                }
                shared.severed.store(true, Ordering::Release);
                Ok(0)
            }
        }
    };
    let end = write_metered(&frame, &meter, |rest| {
        shared.write_stamped_through(rest, since, Some(&meter), &mut conpty_write)
    })
    .expect("an in-memory write never fails");
    assert_eq!(end.owed, PASTE_CLOSE, "the cut owes the close");
    let owed = deliver_owed_through(&end.owed, |rest| {
        shared.write_stamped_through(rest, since, None, &mut conpty_write)
    });
    assert_eq!(
        writes,
        [true, true, false],
        "the frame's writes carry its stop; the owed close's carries none"
    );
    assert!(
        shared.is_severed(),
        "only the sever ended the close's write"
    );
    assert_eq!(owed, 0, "the severed close wrote nothing");
    assert_eq!(
        conpty,
        frame[..METERED_SLICE],
        "the program got the head alone"
    );
}

#[test]
fn a_give_up_hands_back_what_the_cut_owes() {
    let gave_up_at = |frame: &[u8], stop: bool, at: usize| {
        let meter = BulkMeter::new();
        let mut off = 0usize;
        let end = write_metered(frame, &meter, |rest| {
            if off >= at {
                if stop {
                    meter.request_stop();
                }
                return Ok(0);
            }
            let n = rest.len().min(at - off);
            off += n;
            Ok(n)
        })
        .expect("no error");
        assert_eq!(end.accepted, at.min(frame.len()));
        end.owed
    };
    let frame = bracketed("ab\u{e9}cd".as_bytes());
    assert_eq!(
        gave_up_at(&frame, true, 2),
        [&PASTE_OPEN[2..], PASTE_CLOSE].concat()
    );
    assert_eq!(
        gave_up_at(&frame, true, 9),
        [&frame[9..10], PASTE_CLOSE].concat()
    );
    assert_eq!(gave_up_at(&frame, true, 10), PASTE_CLOSE.to_vec());
    let into_close = frame.len() - 2;
    assert_eq!(
        gave_up_at(&frame, true, into_close),
        frame[into_close..].to_vec()
    );
    assert!(gave_up_at(&frame, true, 0).is_empty());
    assert!(gave_up_at(&frame, false, 10).is_empty());
    let plain = "ab\u{e9}cd".as_bytes();
    assert_eq!(gave_up_at(plain, true, 3), plain[3..4].to_vec());
    assert!(gave_up_at(plain, true, 4).is_empty());
}
