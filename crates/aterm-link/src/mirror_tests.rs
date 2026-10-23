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
fn the_mode_reads_its_three_words_and_defaults_to_turns_on_a_retaining_broker() {
    assert_eq!(LogMode::default(), LogMode::Auto);
    for m in [LogMode::Auto, LogMode::Off, LogMode::Turns] {
        assert_eq!(LogMode::parse(m.word()), Some(m));
    }
    assert_eq!(LogMode::parse("on"), None);
    assert_eq!(LogMode::parse("TURNS"), None);
}

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

#[test]
fn a_pin_round_trips_and_a_damaged_one_reads_as_damage() {
    let pin = Pin {
        inc: 7,
        total: 12345,
        key: Some([0xab; 16]),
        win: Vec::new(),
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

#[test]
fn a_pin_cut_anywhere_short_of_its_last_byte_does_not_read() {
    let pin = Pin {
        inc: 7,
        total: 12345,
        key: Some([0xab; 16]),
        win: vec![(9, 300)],
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

#[test]
fn the_window_ledger_rides_the_pin_and_counts_what_the_broker_keeps() {
    let mut pin = Pin {
        inc: 7,
        total: 900,
        key: Some([0xab; 16]),
        win: Vec::new(),
        slots: vec![slot(640, "s-a", 1)],
    };
    pin.note_window(10, 100);
    pin.note_window(20, 200);
    pin.note_window(30, 300);
    let text = pin.render();
    assert!(
        text.starts_with(&format!(
            "logpin v=2 inc=7 n=1 total=900 key={} win=10:100,20:200,30:300\n",
            "ab".repeat(16)
        )),
        "{text}"
    );
    assert_eq!(Pin::parse(&text), Ok(pin.clone()));
    let keyless = Pin {
        key: None,
        ..pin.clone()
    };
    assert_eq!(Pin::parse(&keyless.render()), Ok(keyless));
    assert!(Pin::parse(&text.replace("win=10:100", "win=10:x")).is_err());
    assert!(Pin::parse(&text.replace("win=10:100,", "win=10,")).is_err());
    assert_eq!(
        [20, 21, 0, 31].map(|oldest| pin.window_bytes(oldest)),
        [500, 300, 600, 0],
        "a batch counts while its highest offset is at or above oldest="
    );
    pin.prune_window(20);
    assert_eq!(pin.win, vec![(20, 200), (30, 300)]);

    let mut full = Pin::default();
    let n = WINDOW_LEDGER_MAX as u64 + 1;
    for i in 0..n {
        full.note_window(i * 10, 1);
    }
    assert_eq!(full.win.len(), WINDOW_LEDGER_MAX);
    assert_eq!(full.win[0], (10, 2), "the two oldest, merged");
    assert_eq!(full.window_bytes(0), n, "no byte lost");
    assert_eq!(full.window_bytes(5), n, "and none counted shorter");
    assert_eq!(Pin::parse(&full.render()), Ok(full));
}

#[test]
fn a_full_ledger_gives_its_bytes_up_as_the_cut_passes_them() {
    const N: usize = 1024;
    let small = NODE_WINDOW_BYTES / N as u64;
    let even = vec![small; N];
    let mut lcg = 0x5eed_u64;
    let uneven: Vec<u64> = (0..N)
        .map(|_| {
            lcg = lcg
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            small / 8 + (lcg >> 33) % (small * 8)
        })
        .collect();
    let his: Vec<u64> = (1..=N as u64).map(|i| i * 10).collect();
    for (case, sizes) in [("even", even), ("uneven", uneven)] {
        let mut pin = Pin::default();
        for (hi, bytes) in his.iter().zip(&sizes) {
            pin.note_window(*hi, *bytes);
        }
        assert_eq!(pin.win.len(), WINDOW_LEDGER_MAX, "{case}");
        let held: u64 = sizes.iter().sum();
        assert_eq!(pin.window_bytes(0), held, "{case}: no byte lost");
        let slack = held / (WINDOW_LEDGER_MAX as u64 / 2);
        for (k, oldest) in his.iter().enumerate() {
            let exact: u64 = sizes[k..].iter().sum();
            let counted = pin.window_bytes(*oldest);
            assert!(
                counted >= exact,
                "{case} @{oldest}: counted {counted} < {exact}, shorter than the window"
            );
            assert!(
                counted - exact <= slack,
                "{case} @{oldest}: counted {counted}, {exact} truly there, more than one \
                 entry's {slack} over: {:?}",
                pin.win
            );
        }
        if case == "even" {
            assert!(
                pin.window_bytes(0) >= NODE_WINDOW_BYTES,
                "PRECONDITION: full"
            );
            let half = his[N / 2];
            assert!(
                pin.window_bytes(half) < NODE_WINDOW_BYTES,
                "a cut past half the window opens it: {} counted from @{half}",
                pin.window_bytes(half)
            );
        }
    }
}

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

#[test]
fn the_alias_key_is_minted_never_derived() {
    let a = mint_alias_key().expect("entropy");
    let b = mint_alias_key().expect("entropy");
    assert_ne!(a, b);
    assert_ne!(a, [0u8; 16]);
}
