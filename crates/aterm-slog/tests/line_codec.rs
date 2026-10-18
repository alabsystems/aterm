// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The line encoding (§3.2) and the tier table (§3.6): round trips, the
//! 512-byte refusal, unknown tokens and words, the bus body, and the tier
//! leak test with its negative control.

use aterm_slog::vocab::{
    BOX_WORDS, CAUSE_WORDS, CLS_WORDS, DIR_WORDS, MAIL_KIND_WORDS, NEED_WORDS, OUT_WORDS,
    PHASE_WORDS, STATE_WORDS, VERB_WORDS, WALL_WORDS, WHY_WORDS, WORD_WORDS,
};
use aterm_slog::{
    Actor, Cap, Conf, DecodeError, EncodeError, FIELDS, Fields, Key, Kind, MAX_RECORD_BYTES, Node,
    Op, Rec, ShortId, Src, Tier, Val, decode, decode_bus, encode, encode_bus, encode_with_table,
};

fn rec(op: Op, node: Node, k: Kind, f: Fields) -> Rec {
    Rec {
        seq: 412,
        t: 1_760_545_012_345,
        op,
        node,
        par: None,
        k,
        src: Src::Screen,
        conf: Conf::Strong,
        by: Actor::Human,
        f,
    }
}

/// A record carrying one value of every key, each at its cap.
fn every_key() -> Vec<Rec> {
    FIELDS
        .iter()
        .map(|(key, _, cap)| {
            let val = match cap {
                Cap::Word(list) => Val::Word(list[list.len() - 1]),
                Cap::U => Val::U(u64::MAX),
                Cap::I => Val::I(-101),
                // A broker offset is a host value; a wire id is any other.
                Cap::Id if key.tier() >= Tier::Host => {
                    Val::Id(ShortId::new("@88231").expect("an id"))
                }
                Cap::Id => Val::Id(ShortId::new("88231.4:a_b-c").expect("an id")),
                Cap::Tok(..) => Val::Tok(
                    aterm_slog::BoundedToken::new(*key, "Cargo Build-x.9:1,2 é")
                        .expect("a token survives"),
                ),
                Cap::Text(_) => Val::Text(
                    aterm_slog::BoundedText::new(*key, "SECRET-TEXT ~/src 中 %")
                        .expect("text survives"),
                ),
            };
            let f = Fields::new().with(*key, val).expect("fits its own cap");
            rec(
                Op::Mark,
                Node::new(aterm_slog::Lvl::Turn, 17),
                Kind::Note,
                f,
            )
        })
        .collect()
}

#[test]
fn fields_table_is_in_key_order_and_names_are_unique() {
    let mut seen = std::collections::BTreeSet::new();
    for (i, (key, tier, cap)) in FIELDS.iter().enumerate() {
        assert_eq!(*key as usize, i, "{key:?} out of place");
        assert_eq!(key.tier(), *tier);
        assert_eq!(key.cap(), *cap);
        assert!(seen.insert(key.word()), "two keys spell {}", key.word());
        assert_eq!(Key::from_word(key.word()), Some(*key));
        if matches!(cap, Cap::Text(_)) {
            assert!(*tier >= Tier::Host, "{key:?}: text is H or C only");
        }
        for header in [
            "v", "t", "ep", "seq", "ms", "op", "node", "k", "par", "by", "src", "conf",
        ] {
            assert_ne!(key.word(), header, "a field key spells a header token");
            assert_eq!(Key::from_word(header), None, "{header} is no field key");
        }
    }
    for stranger in ["", "Out", "out ", "nope", "dur_m", "titlex"] {
        assert_eq!(Key::from_word(stranger), None, "{stranger:?}");
    }
}

/// Every word of every closed vocabulary is one wire token, and no vocabulary
/// names a word twice (two changes that each add the same word merge cleanly).
#[test]
fn every_vocabulary_word_is_one_wire_token_named_once() {
    for list in [
        BOX_WORDS,
        WALL_WORDS,
        CLS_WORDS,
        OUT_WORDS,
        NEED_WORDS,
        WORD_WORDS,
        WHY_WORDS,
        VERB_WORDS,
        DIR_WORDS,
        MAIL_KIND_WORDS,
        PHASE_WORDS,
        STATE_WORDS,
        CAUSE_WORDS,
    ] {
        for (i, w) in list.iter().enumerate() {
            assert!(
                !w.is_empty() && w.bytes().all(|b| b.is_ascii_graphic() && b != b'%'),
                "{w:?} is not one token"
            );
            assert_eq!(aterm_codec::pct::encode(w), *w);
            assert!(!list[..i].contains(w), "{w:?} is listed twice");
        }
    }
    for k in Kind::ALL {
        assert_eq!(Kind::from_word(k.word()), *k);
    }
}

#[test]
fn encode_then_decode_is_lossless_for_every_key() {
    for r in every_key() {
        let line = encode(&r, Tier::Content).expect("fits");
        assert!(!line.contains("  ") && !line.contains('\n'), "{line}");
        assert_eq!(decode(&line), Ok(r.clone()), "{line}");
        let body = encode_bus(&r, Tier::WireMeta, 2, Some(97)).expect("fits");
        let back = decode_bus(&body).expect("decodes");
        assert_eq!((back.ep, back.ms), (2, Some(97)));
        // The bus keeps W and W* keys only.
        let key = r.f.iter().next().expect("one field").0;
        if key.tier() <= Tier::WireMeta {
            assert_eq!(back.rec, r, "{body}");
        } else {
            assert!(back.rec.f.is_empty(), "{body}");
        }
    }
}

#[test]
fn the_design_examples_round_trip() {
    for line in [
        "412 open t17 turn par=r3 by=human src=screen conf=strong t=1760545012345",
        "418 open p204 approval par=t17 box=bash:not-read-only src=screen conf=strong t=1760545012400",
        "420 close p204 approval out=approved dur_ms=812 by=harness src=screen conf=strong t=1760545013212",
        "456 amend t17 turn vdur_ms=249870 tok_in=1201344 tok_out=18022 src=transcript conf=exact t=1760545263000",
    ] {
        let r = decode(line).expect("decodes");
        assert_eq!(encode(&r, Tier::Content).as_deref(), Ok(line));
    }
    let body = "v=2 t=1760545012345 ep=2 seq=412 ms=97 op=open node=t17 k=turn par=r3 by=human src=screen conf=strong";
    let b = decode_bus(body).expect("decodes");
    assert_eq!(
        encode_bus(&b.rec, Tier::Wire, b.ep, b.ms).as_deref(),
        Ok(body)
    );
}

#[test]
fn unknown_tokens_are_skipped_and_unknown_words_read_unknown() {
    let r = decode(
        "9 mark t1 frobnicate zzz=1 box=teleport word=busy noeq src=fancy conf=wild t=5 by=s-7bd6",
    )
    .expect("decodes");
    assert_eq!(r.k, Kind::Unknown);
    assert_eq!(r.f.get_word(Key::Box), Some("unknown"));
    assert_eq!(r.f.get_word(Key::Word), Some("busy"));
    assert_eq!(r.f.len(), 2, "zzz= and noeq were skipped");
    assert_eq!((r.src, r.conf), (Src::Unknown, Conf::Unknown));
    assert_eq!(r.by.word(), "s-7bd6");
    for bad in [
        "",
        "x open t1 turn",
        "1 explode t1 turn",
        "1 open q9 turn",
        "1 open t1",
    ] {
        assert!(decode(bad).is_err(), "{bad:?}");
    }
    assert_eq!(decode_bus("v=1 t=5 ev=x"), Err(DecodeError::Opaque));
}

/// A BUS BODY'S UNKNOWN WORDS ARE NOT LOSS (§3.4: a new word never changes
/// `v`): an operation or a node level this build does not know decodes, with
/// its `ep`, `seq` and `ms`, to a record no bus class holds — a mark of kind
/// `unknown` on the session, its fields dropped — while a body missing its
/// seq, operation, node or kind is still refused.
#[test]
fn a_bus_bodys_unknown_words_decode_to_a_record_no_bus_class_holds() {
    for body in [
        "v=2 t=7 ep=3 seq=9 ms=4 op=zzzop node=t1 k=need word=help src=mark conf=claim",
        "v=2 t=7 ep=3 seq=9 ms=4 op=zzzop src=mark conf=claim",
        "v=2 t=7 ep=3 seq=9 ms=4 op=open node=q1 par=- k=turn src=mark conf=claim",
        "v=2 t=7 ep=3 seq=9 ms=4 op=close node=t0 k=turn out=ok src=mark conf=claim",
    ] {
        let b = decode_bus(body).unwrap_or_else(|e| panic!("{e:?}: {body}"));
        assert_eq!(
            (b.ep, b.rec.seq, b.ms, b.rec.t),
            (3, 9, Some(4), 7),
            "{body}"
        );
        assert!(!b.rec.is_bus_class(), "{body}");
        assert_eq!(
            (b.rec.op, b.rec.node, b.rec.par, b.rec.k),
            (Op::Mark, Node::SESSION, None, Kind::Unknown),
            "{body}"
        );
        assert!(b.rec.f.is_empty(), "nothing of it is read: {body}");
        assert_eq!((b.rec.src, b.rec.conf), (Src::Mark, Conf::Claim));
    }
    for (body, err) in [
        (
            "v=2 t=7 ep=3 ms=4 op=zzzop node=t1 k=turn",
            DecodeError::Seq,
        ),
        ("v=2 t=7 ep=3 seq=9 ms=4 node=t1 k=turn", DecodeError::Op),
        ("v=2 t=7 ep=3 seq=9 ms=4 op=open k=turn", DecodeError::Node),
        ("v=2 t=7 ep=3 seq=9 ms=4 op=open node=t1", DecodeError::Kind),
    ] {
        assert_eq!(decode_bus(body), Err(err), "{body}");
    }
}

#[test]
fn oversized_records_are_refused_and_values_clamped() {
    // Text is clamped to its key's cap on the way in, escapes included.
    let cwd = aterm_slog::BoundedText::new(Key::Cwd, &"中".repeat(400)).expect("text");
    assert!(aterm_slog::field::encoded_len(cwd.as_str()) <= 256);
    assert_eq!(cwd.as_str(), "中".repeat(28), "never half a character");
    // Three capped texts together pass the record bound: refused.
    let f = Fields::new()
        .text(Key::Cwd, &"/x".repeat(200))
        .and_then(|f| f.text(Key::Title, &"t".repeat(300)))
        .and_then(|f| f.text(Key::Text, &"n".repeat(300)))
        .expect("each fits its own cap");
    let r = rec(Op::Mark, Node::SESSION, Kind::Note, f);
    match encode(&r, Tier::Content) {
        Err(EncodeError::TooLarge(n)) => assert!(n > MAX_RECORD_BYTES),
        other => panic!("not refused: {other:?}"),
    }
    // The same record at the wire tier drops the H and C keys and fits.
    let wire = encode(&r, Tier::Wire).expect("fits without its text");
    assert!(wire.len() <= MAX_RECORD_BYTES);
}

/// THE TIER LEAK TEST: no H or C key survives `encode(Tier::Wire)`, and no
/// C key survives `Tier::Host` (the journal's tier). The negative control
/// marks one H key as W in a copy of the table and watches it leak.
#[test]
fn no_host_or_content_key_survives_the_wire_tier() {
    let leaks = |table: &[(Key, Tier, Cap)], tier: Tier| -> Vec<Key> {
        every_key()
            .iter()
            .filter_map(|r| {
                let line = encode_with_table(r, tier, table).expect("fits");
                let (key, _) = r.f.iter().next().expect("one field");
                let held_above = key.tier() > tier;
                (held_above && line.contains(&format!(" {}=", key.word()))).then_some(*key)
            })
            .collect()
    };
    assert!(leaks(FIELDS, Tier::Wire).is_empty());
    assert!(leaks(FIELDS, Tier::WireMeta).is_empty());
    assert!(leaks(FIELDS, Tier::Host).is_empty());
    for r in every_key() {
        let line = encode(&r, Tier::Wire).expect("fits");
        assert!(!line.contains("SECRET-TEXT"), "{line}");
    }

    // Negative control: `cwd` marked W leaks at the wire tier.
    let mut doctored: Vec<(Key, Tier, Cap)> = FIELDS.to_vec();
    let row = doctored
        .iter_mut()
        .find(|row| row.0 == Key::Cwd)
        .expect("cwd is declared");
    row.1 = Tier::Wire;
    assert_eq!(leaks(&doctored, Tier::Wire), vec![Key::Cwd]);
}

#[test]
fn fields_refuse_wrong_shapes_and_a_thirteenth_key() {
    use aterm_slog::FieldError;
    assert_eq!(
        Fields::new().with(Key::Box, Val::Word("teleport")),
        Err(FieldError::Shape(Key::Box))
    );
    assert_eq!(
        Fields::new().with(Key::Blk, Val::I(1)),
        Err(FieldError::Shape(Key::Blk))
    );
    assert!(
        Fields::new().text(Key::Detail, "x").is_err(),
        "W* holds no text"
    );
    let mut f = Fields::new();
    for key in FIELDS
        .iter()
        .filter(|r| r.2 == Cap::U)
        .map(|r| r.0)
        .take(12)
    {
        f.set(key, Val::U(1)).expect("room");
    }
    assert_eq!(f.set(Key::Code, Val::I(1)), Err(FieldError::Full));
    f.set(Key::Blk, Val::U(2))
        .expect("replacing a key needs no room");
}

/// A mail offset never crosses the bus (§3.6, §5.5): `cause=@<off>` and
/// `re=@<off>` are host keys, the bus carries only the class (`cause_k=mail`),
/// and no wire id can hold an `@<off>` at all.
#[test]
fn no_mail_offset_survives_the_bus() {
    let f = Fields::new().cause("@88231").expect("a cause");
    assert_eq!(f.get_word(Key::CauseKind), Some("mail"));
    let mut r = rec(
        Op::Open,
        Node::new(aterm_slog::Lvl::Turn, 17),
        Kind::Turn,
        f,
    );
    r.par = Some(Node::new(aterm_slog::Lvl::Run, 3));
    assert!(r.is_turn_class());
    let body = encode_bus(&r, Tier::WireMeta, 2, Some(1)).expect("fits");
    assert!(!body.contains('@'), "{body}");
    assert!(body.contains(" cause_k=mail"), "{body}");
    assert!(
        encode(&r, Tier::Host)
            .expect("fits")
            .contains(" cause=@88231")
    );
    // Every key that takes an id: an offset is held only below the bus.
    let off = ShortId::new("@88231").expect("an id");
    for (key, tier, cap) in FIELDS {
        if *cap != Cap::Id {
            continue;
        }
        let held = Fields::new().with(*key, Val::Id(off)).is_ok();
        assert_eq!(held, *tier >= Tier::Host, "{key:?} and an offset");
    }
    assert_eq!(
        Fields::new()
            .cause("412")
            .expect("a seq")
            .get_word(Key::CauseKind),
        Some("rec")
    );
}

/// A token is held to its key, not only its cap: one sanitized for a W*
/// key (`detail`, any visible character) never rides under a W key
/// (`tools`, `[a-z0-9.:,-]`).
#[test]
fn a_token_is_held_to_its_own_key() {
    use aterm_slog::FieldError;
    let detail =
        aterm_slog::BoundedToken::new(Key::Detail, "Secret Cmd /Users//x").expect("a token");
    assert_eq!(
        Fields::new().with(Key::Tools, Val::Tok(detail.clone())),
        Err(FieldError::Shape(Key::Tools))
    );
    assert!(Fields::new().with(Key::Detail, Val::Tok(detail)).is_ok());
    let tools = aterm_slog::BoundedToken::new(Key::Tools, "shell:9,edit:8").expect("a token");
    assert!(Fields::new().with(Key::Tools, Val::Tok(tools)).is_ok());
}
