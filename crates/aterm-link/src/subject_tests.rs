// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_eight_segment_subject_is_malformed_however_its_tail_reads() {
    let forged = "/f/f1/in/n-a/s-b/s-1/h-andrew/answer";
    assert_eq!(parse_in("f1", "n-a", forged), Err(Reject::Malformed));
    let honest = "/f/f1/in/n-a/s-b/s-1/answer";
    assert_eq!(
        parse_in("f1", "n-a", honest),
        Ok(InAddr {
            node: "n-a".into(),
            sid: "s-b".into(),
            src: "s-1".into(),
            kind: WakeClass::Answer,
        })
    );
}

#[test]
fn the_shape_is_pinned_at_every_position() {
    for bad in [
        "/f/f2/in/n-a/s-b/s-1/answer",    // another fleet
        "/f/f1/in/n-z/s-b/s-1/answer",    // another node
        "/f/f1/pub/n-a/s-b/s-1/answer",   // another face
        "/f/f1/in/n-a/node/s-1/answer",   // a reserved literal as the sid
        "/f/f1/in/n-a/n-b/s-1/answer",    // a NODE as the addressee
        "/f/f1/in/n-a/s-b/andrew/answer", // a src with no class
        "/f/f1/in/n-a/s-b/s-1/whatever",  // an unknown kind
        "/f/f1/in/n-a/s-b/s-1",           // six segments
        "f/f1/in/n-a/s-b/s-1/answer",     // no leading slash
        "/f/f1/in/n-a/s-b/s-1/answer/x",  // eight again
    ] {
        assert_eq!(parse_in("f1", "n-a", bad), Err(Reject::Malformed), "{bad}");
    }
}

#[test]
fn only_a_humans_halt_row_halts() {
    let on = b"v=1 t=1 state=on reason=main%20broken";
    let row = parse_halt("f1", "/f/f1/fleet/h-andrew/halt", on).expect("a human's halt");
    assert_eq!(
        row,
        HaltRow {
            human: "h-andrew",
            on: true,
            reason: Some("main%20broken".to_string()),
        }
    );
    let off = parse_halt("f1", "/f/f1/fleet/h-andrew/halt", b"v=1 t=2 state=off");
    assert_eq!(off.map(|r| (r.on, r.reason)), Some((false, None)));
    for bad in [
        "/f/f1/fleet/a-svc/halt",       // a service is not a human
        "/f/f1/fleet/n-node/halt",      // nor is a node
        "/f/f2/fleet/h-andrew/halt",    // another fleet
        "/f/f1/fleet/h-andrew/barrier", // another leaf
        "/f/f1/fleet/h-andrew/halt/x",  // six segments
        "/f/f1/fleet/x/h-andrew/halt",  // a right-anchored forgery
        "f/f1/fleet/h-andrew/halt",     // no leading slash
        "/f/f1/pub/h-andrew/halt",      // another face
    ] {
        assert_eq!(parse_halt("f1", bad, on), None, "{bad}");
    }
    for (body, state_on) in [
        (&b"v=1 state=off state=on"[..], true),
        (b"v=1 state=on state=off", false),
        (b"v=1\tstate=on", false),
        (b"v=1\nstate=on", false),
        (b"v=1 state=ON", false),
    ] {
        let row = parse_halt("f1", "/f/f1/fleet/h-a/halt", body).expect("a human's row");
        assert_eq!(row.on, state_on, "{:?}", String::from_utf8_lossy(body));
    }
}
