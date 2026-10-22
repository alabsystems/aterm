// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bus mirror's pure parts: the take's frame, the pin's file, the
//! landing line and its answer, the bucket on a stepped clock, the alias key.

use super::*;

fn slot(pseq: u64, sid: &str, ms: u64) -> PinSlot {
    PinSlot {
        pseq,
        sid: sid.to_string(),
        at: BusCursor::new(4, ms * 2, ms),
        body: format!(
            "v=2 t=1 ep=4 seq={} ms={ms} op=open node=t{ms} k=turn src=screen conf=strong",
            ms * 2
        ),
    }
}

#[test]
fn the_mode_reads_its_two_words_and_defaults_off() {
    assert_eq!(LogMode::default(), LogMode::Off);
    for m in [LogMode::Off, LogMode::Turns] {
        assert_eq!(LogMode::parse(m.word()), Some(m));
    }
    assert_eq!(LogMode::parse("on"), None);
    assert_eq!(LogMode::parse("TURNS"), None);
}

/// RULING Q11: the take's frame round-trips — the header's byte count is the
/// body's (what a Bytes reader counts), each line a sid, a cursor and a body
/// with spaces in it.
#[test]
fn a_take_frame_reads_back_its_slots() {
    let body = "s-a 4:2:1 v=2 t=1 ep=4 seq=2 ms=1 op=open node=- k=spawn src=aterm conf=exact\n\
                s-b 4:9:3 v=2 t=1 ep=4 seq=9 ms=3 op=gap node=- k=gap why=evicted n=2 src=aterm conf=exact\n";
    let header = format!("OK {} slots=2 more=1", body.len());
    let (slots, more) = parse_take(&header, body.as_bytes()).expect("reads");
    assert!(more);
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[1].sid, "s-b");
    assert_eq!(slots[1].at, BusCursor::new(4, 9, 3));
    assert!(slots[1].body.ends_with("n=2 src=aterm conf=exact"));
    let (none, more) = parse_take("OK 0 slots=0 stale=1", b"").expect("a ghost lane's");
    assert!(none.is_empty() && !more);
    for (h, b) in [
        ("OK 5 slots=2", "s-a 4:2:1 x\n"),
        ("OK 5", "s-a 4:2:1 x\n"),
        ("OK 5 slots=1", "s-a 4:2 x\n"),
        ("ERR busy adopting=1", ""),
    ] {
        assert!(parse_take(h, b.as_bytes()).is_err(), "{h} {b:?}");
    }
}

/// The pin is its batch exactly: rendered, read back, and refused whole when
/// any line of it does not read.
#[test]
fn a_pin_round_trips_and_a_damaged_one_reads_as_damage() {
    let pin = Pin {
        inc: 7,
        total: 12345,
        key: Some([0xab; 16]),
        slots: vec![
            slot(640, "s-a", 1),
            slot(641, "s-b", 1),
            slot(642, "s-a", 2),
        ],
    };
    let text = pin.render();
    assert!(
        text.starts_with(&format!(
            "logpin v=2 inc=7 n=3 total=12345 key={}\n",
            "ab".repeat(16)
        )),
        "{text}"
    );
    assert_eq!(Pin::parse(&text), Ok(pin.clone()));
    // An older build's pin (`v=1`, before the durable copies) reads the same;
    // it is told from this build's, whole or cut short, and a prefix too short
    // to name its version is neither's.
    let older = text.replacen("v=2", "v=1", 1);
    assert_eq!(Pin::parse(&older), Ok(pin.clone()));
    for (bytes, older_build, this_build) in [
        (older.as_bytes(), true, false),
        (&older.as_bytes()[..10], true, false),
        (text.as_bytes(), false, true),
        (&text.as_bytes()[..11], false, true),
        (&text.as_bytes()[..9], false, false),
        (&b""[..], false, false),
    ] {
        assert_eq!(
            (Pin::by_an_older_build(bytes), Pin::by_this_build(bytes)),
            (older_build, this_build),
            "{:?}",
            String::from_utf8_lossy(bytes)
        );
    }
    // A pin written before the key reads with none; a key that does not
    // read, or a word past it, is damage.
    let keyless = Pin {
        key: None,
        ..pin.clone()
    };
    assert!(keyless
        .render()
        .starts_with("logpin v=2 inc=7 n=3 total=12345\n"));
    assert_eq!(Pin::parse(&keyless.render()), Ok(keyless));
    assert!(Pin::parse(&text.replace(" key=abab", " key=zzab")).is_err());
    assert_eq!(
        Pin::parse(&text.replacen(" key=", " later=1 key=", 1)),
        Ok(pin.clone()),
        "a newer writer's word is read past"
    );
    let empty = Pin {
        slots: Vec::new(),
        ..pin.clone()
    };
    assert_eq!(Pin::parse(&empty.render()), Ok(empty));
    let short = text.lines().take(3).collect::<Vec<_>>().join("\n");
    assert!(Pin::parse(&short).is_err(), "a slot missing");
    assert!(Pin::parse(&text.replacen("v=2", "v=3", 1)).is_err());
    assert!(Pin::parse(&text.replace("\n641 ", "\nx ")).is_err());
    assert!(Pin::parse("").is_err());
}

/// A PIN A POWER CUT CUT SHORT NEVER READS: every pin ends at the newline of
/// its last line, so a prefix of one is damage — never a pin with a lower
/// total and no key (ruling Q13's count and Q6's key reset) nor a slot whose
/// body lost its tail (republished cut). The mirror then stops, keeping the
/// pin on disk, rather than mint a fresh key.
#[test]
fn a_pin_cut_anywhere_short_of_its_last_byte_does_not_read() {
    let pin = Pin {
        inc: 7,
        total: 12345,
        key: Some([0xab; 16]),
        slots: vec![slot(640, "s-a", 1), slot(641, "s-b", 1)],
    };
    let text = pin.render();
    let header = Pin {
        slots: Vec::new(),
        ..pin.clone()
    }
    .render();
    for whole in [&text, &header] {
        assert!(Pin::parse(whole).is_ok());
        for cut in 0..whole.len() {
            assert!(
                Pin::parse(&whole[..cut]).is_err(),
                "a pin cut at {cut} of {}: {:?}",
                whole.len(),
                &whole[..cut]
            );
        }
    }
}

/// RULING Q10: one landing names every session's last cursor once in each
/// seq space, in the order they first appear, and its answer is one word a
/// pair. A take holding an uncarried update's old-space `why=reset` cover
/// before the new space's records (ruling Q7) lands both spaces: naming only
/// the session's last cursor left the cover owed, taken again every take.
#[test]
fn a_landing_names_each_sessions_last_cursor_and_reads_a_word_a_pair() {
    let slots = [slot(1, "s-a", 1), slot(2, "s-b", 1), slot(3, "s-a", 2)];
    let pairs = landed_pairs(&slots);
    assert_eq!(
        pairs,
        vec![
            ("s-a".to_string(), BusCursor::new(4, 4, 2)),
            ("s-b".to_string(), BusCursor::new(4, 2, 1))
        ]
    );
    let reset = PinSlot {
        pseq: 9,
        sid: "s-c".to_string(),
        at: BusCursor::new(3, 120, 52),
        body: "v=2 t=1 ep=3 seq=120 ms=41 op=gap node=- k=gap why=reset n=12 src=aterm conf=exact"
            .to_string(),
    };
    let spaces = landed_pairs(&[reset, slot(10, "s-c", 1), slot(11, "s-c", 2)]);
    assert_eq!(
        spaces,
        vec![
            ("s-c".to_string(), BusCursor::new(3, 120, 52)),
            ("s-c".to_string(), BusCursor::new(4, 4, 2))
        ],
        "the old space's cover lands beside the new space's records"
    );
    assert_eq!(landed_line(&pairs), "log-landed s-a 4:4:2 s-b 4:2:1");
    assert_eq!(
        parse_landed("OK ok adopting", 2),
        Ok(Some(vec![LandedWord::Ok, LandedWord::Adopting]))
    );
    assert_eq!(parse_landed("OK stale=1", 2), Ok(None), "a ghost lane");
    assert!(parse_landed("OK ok", 2).is_err());
    assert!(parse_landed("OK ok sideways", 2).is_err());
    assert!(parse_landed("ERR usage: log-landed", 2).is_err());
    for w in [
        LandedWord::Ok,
        LandedWord::Stale,
        LandedWord::Done,
        LandedWord::Gone,
        LandedWord::Ahead,
    ] {
        assert!(w.settled());
    }
    assert!(!LandedWord::Fenced.settled() && !LandedWord::Adopting.settled());
}

/// The bucket on a stepped clock: a full burst, then the sustained rate.
#[test]
fn the_bucket_spends_its_burst_then_refills_at_its_rate() {
    let mut b = Bucket::full(0);
    assert_eq!(b.room(0), RATE_BURST);
    b.spend(RATE_BURST);
    assert_eq!(b.room(0), 0);
    assert_eq!(b.room(1000), RATE_PER_SEC, "one second's worth");
    assert_eq!(
        b.room(500),
        RATE_PER_SEC,
        "a clock read backwards takes nothing"
    );
    assert_eq!(b.room(10_000_000), RATE_BURST, "never past the burst");
}

/// RULING Q6: the alias key is minted, 16 random bytes, never a function of
/// the node's caps: two mints differ, and none is all zeros.
#[test]
fn the_alias_key_is_minted_never_derived() {
    let a = mint_alias_key().expect("entropy");
    let b = mint_alias_key().expect("entropy");
    assert_ne!(a, b);
    assert_ne!(a, [0u8; 16]);
}
