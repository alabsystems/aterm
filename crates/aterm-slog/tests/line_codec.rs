// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The line encoding (§3.2) and the tier table (§3.6).

use aterm_slog::vocab::{
    ACT_WORDS, ASK_WORDS, BOX_WORDS, CAUSE_WORDS, CHANGE_WORDS, CLASS_WORDS, CLS_WORDS, DIR_WORDS,
    END_WORDS, FACT_RULE_WORDS, HEALED_BY_WORDS, HELD_WORDS, MAIL_KIND_WORDS, NEED_WORDS,
    NET_WORDS, ORIGIN_WORDS, OUT_WORDS, PHASE_WORDS, POOL_WORDS, SEV_WORDS, SOURCE_WORDS,
    STATE_WORDS, VERB_WORDS, WALL_WORDS, WHY_WORDS, WORD_WORDS,
};
use aterm_slog::{
    Actor, Cap, Conf, DecodeError, EncodeError, FIELDS, Fields, Key, Kind, Lvl, MAX_RECORD_BYTES,
    Node, Op, Rec, ShortId, Src, Tier, Val, decode, decode_bus, encode, encode_bus,
    encode_with_table,
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

fn every_key() -> Vec<Rec> {
    FIELDS
        .iter()
        .map(|(key, _, cap)| {
            let val = match cap {
                Cap::Word(list) => Val::Word(list[list.len() - 1]),
                Cap::U => Val::U(u64::MAX),
                Cap::I => Val::I(-101),
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
        ACT_WORDS,
        END_WORDS,
        POOL_WORDS,
        ORIGIN_WORDS,
        ASK_WORDS,
        CHANGE_WORDS,
        NET_WORDS,
        CLASS_WORDS,
        HEALED_BY_WORDS,
        SOURCE_WORDS,
        SEV_WORDS,
        HELD_WORDS,
        FACT_RULE_WORDS,
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
fn node_ids_of_every_width_round_trip() {
    for n in [1, 255, 256, 65_535, 65_536, u32::MAX] {
        for (lvl, k) in [
            (Lvl::Run, Kind::Cmd),
            (Lvl::Turn, Kind::Turn),
            (Lvl::Step, Kind::Approval),
        ] {
            let mut r = rec(Op::Open, Node::new(lvl, n), k, Fields::new());
            r.par = Some(Node::new(Lvl::Run, n));
            let line = encode(&r, Tier::Content).expect("fits");
            assert_eq!(decode(&line), Ok(r), "{line}");
        }
    }
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
    let cwd = aterm_slog::BoundedText::new(Key::Cwd, &"中".repeat(400)).expect("text");
    assert!(aterm_slog::field::encoded_len(cwd.as_str()) <= 256);
    assert_eq!(cwd.as_str(), "中".repeat(28), "never half a character");
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
    let wire = encode(&r, Tier::Wire).expect("fits without its text");
    assert!(wire.len() <= MAX_RECORD_BYTES);
}

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

    let mut doctored: Vec<(Key, Tier, Cap)> = FIELDS.to_vec();
    let row = doctored
        .iter_mut()
        .find(|row| row.0 == Key::Cwd)
        .expect("cwd is declared");
    row.1 = Tier::Wire;
    assert_eq!(leaks(&doctored, Tier::Wire), vec![Key::Cwd]);
    let mut doctored: Vec<(Key, Tier, Cap)> = FIELDS.to_vec();
    let row = doctored
        .iter_mut()
        .find(|row| row.0 == Key::AttnText)
        .expect("attn_text is declared");
    row.1 = Tier::Host;
    assert_eq!(leaks(&doctored, Tier::Host), vec![Key::AttnText]);
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

#[test]
fn a_rule_id_crosses_the_wire_as_name_dot_version() {
    let f = Fields::new()
        .id(Key::Rule, "continue.v1")
        .expect("a rule id");
    let mut r = rec(Op::Mark, Node::new(aterm_slog::Lvl::Turn, 17), Kind::Act, f);
    r.f = r.f.word(Key::Word, "continue").expect("an act word");
    let line = encode(&r, Tier::Wire).expect("fits");
    assert!(line.contains(" rule=continue.v1"), "{line}");
    assert_eq!(
        decode(&line).expect("decodes").f.get_str(Key::Rule),
        Some("continue.v1")
    );
    let body = encode_bus(&r, Tier::Wire, 1, Some(1)).expect("fits");
    assert!(body.contains(" rule=continue.v1"), "{body}");
    assert!(Fields::new().id(Key::Rule, "continue@v1").is_err());
    let at = decode(&line.replace("continue.v1", "continue@v1")).expect("the record decodes");
    assert_eq!(
        at.f.get(Key::Rule),
        None,
        "an `@` id never rides a wire key"
    );
}

#[test]
fn phase_3b_keys_and_kinds_are_appended_wire_numbers() {
    let n = FIELDS
        .iter()
        .position(|r| r.0 == Key::Attention)
        .expect("phase 5's first key");
    let tail: Vec<Key> = FIELDS[n - 4..n].iter().map(|r| r.0).collect();
    assert_eq!(tail, [Key::H5, Key::D7, Key::Skipped, Key::Dropped]);
    for k in tail {
        assert_eq!((k.tier(), k.cap()), (Tier::Wire, Cap::U), "{k:?}");
    }
    assert_eq!(
        FIELDS[n - 5].0,
        Key::Title,
        "the keys before 3B kept their places"
    );
    let at = Kind::ALL
        .iter()
        .position(|k| *k == Kind::Source)
        .expect("3B's first kind");
    assert_eq!(
        &Kind::ALL[at..at + 3],
        &[Kind::Source, Kind::MailLanded, Kind::Task]
    );
    for k in [Kind::Source, Kind::MailLanded] {
        assert!(k.is_mark() && k.node_level().is_none(), "{k:?}");
    }
}

#[test]
fn phase_5_task_key_and_kind_are_appended_and_local() {
    let n = 1 + FIELDS
        .iter()
        .position(|r| r.0 == Key::Attention)
        .expect("the task key");
    assert_eq!(FIELDS[n - 2].0, Key::Dropped, "3B's keys kept their places");
    assert_eq!(
        (Key::Attention.tier(), Key::Attention.cap()),
        (Tier::Wire, Cap::U)
    );
    let kind_at = |k: Kind| Kind::ALL.iter().position(|x| *x == k);
    assert_eq!(
        kind_at(Kind::Task),
        kind_at(Kind::MailLanded).map(|i| i + 1),
        "appended after 3B's kinds, which kept their places"
    );
    assert!(Kind::Task.is_mark() && Kind::Task.node_level().is_none());
    let f = Fields::new()
        .u(Key::Rev, 3)
        .and_then(|f| f.word(Key::State, "stalled"))
        .and_then(|f| f.u(Key::Attention, 1))
        .and_then(|f| f.word(Key::Why, "done-check"))
        .and_then(|f| f.i(Key::Code, -2))
        .expect("the task words are closed words");
    let r = Rec {
        seq: 4,
        t: 9,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k: Kind::Task,
        f,
        src: Src::Aterm,
        conf: Conf::Exact,
        by: Actor::Aterm,
    };
    assert!(!r.is_bus_class(), "a task mark stays local");
    let line = encode(&r, Tier::Content).expect("fits");
    assert_eq!(
        line,
        "4 mark - task rev=3 state=stalled attention=1 why=done-check code=-2 by=aterm src=aterm conf=exact t=9"
    );
    assert_eq!(decode(&line), Ok(r));
}

#[test]
fn phase_5_act_key_is_appended_and_closed() {
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    assert_eq!(
        at(Key::Act),
        at(Key::Attention).map(|i| i + 1),
        "appended after 5C-1's key, which kept its place"
    );
    assert_eq!(Key::Act.tier(), Tier::Wire);
    let words: Vec<&str> = aterm_slog::ActWord::ALL.iter().map(|a| a.word()).collect();
    assert_eq!(words, ACT_WORDS);
    for w in ACT_WORDS {
        assert_eq!(
            aterm_slog::ActWord::from_word(w).map(aterm_slog::ActWord::word),
            Some(*w)
        );
    }
    assert!(Fields::new().word(Key::Act, "approve").is_ok());
    assert!(
        Fields::new().word(Key::Act, "maybe").is_err(),
        "a closed word"
    );
}

#[test]
fn phase_5_reached_key_is_appended_and_host_only() {
    let n = 1 + FIELDS
        .iter()
        .position(|r| r.0 == Key::Reached)
        .expect("5D-4's key");
    assert_eq!(FIELDS[n - 2].0, Key::Act, "5D-2's key kept its place");
    assert_eq!(Key::Reached.tier(), Tier::Host);
    let r = Rec {
        seq: 5,
        t: 9,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k: Kind::Human,
        f: Fields::new().u(Key::Reached, 0).expect("a count"),
        src: Src::Input,
        conf: Conf::Strong,
        by: Actor::Human,
    };
    let line = encode(&r, Tier::Host).expect("fits");
    assert_eq!(
        line,
        "5 mark - human reached=0 by=human src=input conf=strong t=9"
    );
    assert_eq!(decode(&line), Ok(r.clone()));
    let wire = encode(&r, Tier::Wire).expect("fits");
    assert!(!wire.contains("reached="), "{wire}");
}

#[test]
fn phase_5_node_log_keys_are_appended_wire_words() {
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    assert_eq!(at(Key::PrevEnd), at(Key::Reached).map(|i| i + 1));
    assert_eq!(at(Key::CfgKey), at(Key::PrevEnd).map(|i| i + 1));
    assert_eq!(
        at(Key::OpBy),
        at(Key::CfgKey).map(|i| i + 1),
        "6C-2's op_by= comes after them"
    );
    assert_eq!(
        (Key::PrevEnd.tier(), Key::PrevEnd.cap()),
        (Tier::Wire, Cap::Word(END_WORDS))
    );
    assert_eq!(
        (Key::CfgKey.tier(), Key::CfgKey.cap()),
        (Tier::Wire, Cap::Id)
    );
    assert_eq!(Key::CfgKey.word(), "key");
    for w in [
        "clean", "handoff", "killed", "signal", "panic", "running", "unknown", "none", "-",
    ] {
        assert!(Fields::new().word(Key::PrevEnd, w).is_ok(), "{w}");
    }
    assert!(Fields::new().word(Key::PrevEnd, "signal-11").is_err());
    for w in ["connected", "disconnected", "stalled", "absent"] {
        assert!(Fields::new().word(Key::State, w).is_ok(), "{w}");
    }
    assert!(
        Fields::new()
            .id(Key::CfgKey, "a value with spaces")
            .is_err(),
        "an id, never text"
    );
    let r = Rec {
        seq: 1,
        t: 9,
        op: Op::Open,
        node: Node::INSTANCE,
        par: None,
        k: Kind::Start,
        f: Fields::new().word(Key::PrevEnd, "-").expect("a word"),
        src: Src::Aterm,
        conf: Conf::Exact,
        by: Actor::Aterm,
    };
    let line = encode(&r, Tier::Wire).expect("fits");
    assert_eq!(
        line,
        "1 open n start prev_end=- by=aterm src=aterm conf=exact t=9"
    );
    assert_eq!(decode(&line), Ok(r));
}

#[test]
fn phase_6_op_by_is_appended_host_only_and_an_older_reader_skips_it() {
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    assert_eq!(at(Key::OpBy), at(Key::CfgKey).map(|i| i + 1));
    assert_eq!(
        at(Key::Pool),
        at(Key::OpBy).map(|i| i + 1),
        "6C-6's pool= comes after it"
    );
    assert_eq!((Key::OpBy.tier(), Key::OpBy.cap()), (Tier::Host, Cap::Id));
    assert_eq!(Key::OpBy.word(), "op_by");
    let sid = ShortId::new("s-edge42").expect("a sid");
    let r = Rec {
        seq: 30,
        t: 9,
        op: Op::Close,
        node: Node::new(Lvl::Turn, 3),
        par: None,
        k: Kind::Turn,
        f: Fields::new()
            .word(Key::Out, "done")
            .and_then(|f| f.u(Key::DurMs, 40))
            .and_then(|f| f.u(Key::Ledger, 7))
            .and_then(|f| f.with(Key::OpBy, Val::Id(sid)))
            .expect("a close's fields"),
        src: Src::Screen,
        conf: Conf::Strong,
        by: Actor::Ctl,
    };
    let line = encode(&r, Tier::Host).expect("fits");
    assert_eq!(
        line,
        "30 close t3 turn out=done dur_ms=40 ledger=7 op_by=s-edge42 by=ctl src=screen conf=strong t=9"
    );
    assert_eq!(decode(&line), Ok(r.clone()));
    for tier in [Tier::Wire, Tier::WireMeta] {
        let wire = encode(&r, tier).expect("fits");
        assert!(
            !wire.contains("op_by=") && !wire.contains("s-edge42"),
            "{wire}"
        );
        let bus = encode_bus(&r, tier, 1, Some(4)).expect("fits");
        assert!(
            !bus.contains("op_by=") && !bus.contains("s-edge42"),
            "{bus}"
        );
    }
    let older = line.replace(" op_by=", " zz_opener=");
    let mut without = r.clone();
    without.f.remove(Key::OpBy);
    assert_eq!(decode(&older), Ok(without), "{older}");
}

#[test]
fn phase_6_pool_is_appended_host_only_and_closed() {
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    assert_eq!(at(Key::Pool), at(Key::OpBy).map(|i| i + 1));
    assert_eq!(
        at(Key::Build),
        at(Key::Pool).map(|i| i + 1),
        "6F-2's keys come after it"
    );
    assert_eq!(
        (Key::Pool.tier(), Key::Pool.cap()),
        (Tier::Host, Cap::Word(POOL_WORDS))
    );
    assert_eq!(Key::Pool.word(), "pool");
    assert_eq!(POOL_WORDS, ["shared"]);
    assert!(Fields::new().word(Key::Pool, "shared").is_ok());
    for stranger in ["own", "maybe", "Shared", "shared ", ""] {
        assert!(
            Fields::new().word(Key::Pool, stranger).is_err(),
            "{stranger:?} is no pool word"
        );
    }
    let r = Rec {
        seq: 31,
        t: 9,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k: Kind::Input,
        f: Fields::new()
            .word(Key::Verb, "key")
            .and_then(|f| f.word(Key::Act, "deny"))
            .and_then(|f| f.u(Key::Dropped, 7))
            .and_then(|f| f.word(Key::Pool, "shared"))
            .expect("an input mark's fields"),
        src: Src::Ctl,
        conf: Conf::Strong,
        by: Actor::Sid(ShortId::new("s-edge42").expect("a sid")),
    };
    let line = encode(&r, Tier::Host).expect("fits");
    assert_eq!(
        line,
        "31 mark - input verb=key act=deny dropped=7 pool=shared by=s-edge42 src=ctl conf=strong t=9"
    );
    assert_eq!(decode(&line), Ok(r.clone()));
    let wire = encode(&r, Tier::Wire).expect("fits");
    assert!(!wire.contains("pool="), "{wire}");
    let stranger = line.replace(" pool=shared ", " pool=maybe ");
    let read = decode(&stranger).expect("an unknown word reads");
    assert_eq!(read.f.get_word(Key::Pool), Some("unknown"), "{stranger}");
}

#[test]
fn phase_6_ring_vocabulary_is_appended_at_its_tiers() {
    let kind_at = |k: Kind| Kind::ALL.iter().position(|x| *x == k);
    let kinds = [
        Kind::State,
        Kind::Policy,
        Kind::Topic,
        Kind::Advisory,
        Kind::Program,
        Kind::MailFetch,
        Kind::Shown,
    ];
    let first = kind_at(Kind::Task).map(|i| i + 1).expect("5C-1's kind");
    assert_eq!(
        &Kind::ALL[first..first + kinds.len()],
        &kinds,
        "appended after `task`"
    );
    let words: Vec<&str> = kinds.iter().map(|k| k.word()).collect();
    assert_eq!(
        words,
        [
            "state",
            "policy",
            "topic",
            "advisory",
            "program",
            "mail-fetch",
            "shown"
        ]
    );
    for k in kinds {
        assert!(k.is_mark() && k.node_level().is_none(), "{k:?} is a mark");
        let r = Rec {
            seq: 1,
            t: 9,
            op: Op::Mark,
            node: Node::SESSION,
            par: None,
            k,
            src: Src::Aterm,
            conf: Conf::Exact,
            by: Actor::Aterm,
            f: Fields::new(),
        };
        assert!(!r.is_bus_class() && !r.is_turn_class(), "{k:?} stays local");
    }

    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    let rows: [(Key, &str, Tier, Cap); 17] = [
        (Key::Build, "build", Tier::Wire, Cap::U),
        (Key::Run, "run", Tier::Wire, Cap::U),
        (Key::Displaced, "displaced", Tier::Wire, Cap::I),
        (Key::At, "at", Tier::Wire, Cap::U),
        (Key::Bytes, "bytes", Tier::Wire, Cap::U),
        (Key::Producer, "producer", Tier::Wire, Cap::U),
        (
            Key::Reverted,
            "reverted",
            Tier::Wire,
            Cap::Tok(aterm_slog::Charset::Rollup, 300),
        ),
        (Key::Origin, "origin", Tier::Wire, Cap::Word(ORIGIN_WORDS)),
        (Key::Ask, "ask", Tier::Wire, Cap::Word(ASK_WORDS)),
        (Key::Coalesced, "coalesced", Tier::Wire, Cap::U),
        (Key::Holder, "holder", Tier::Host, Cap::Id),
        (Key::Via, "via", Tier::Host, Cap::Text(256)),
        (Key::Idem, "idem", Tier::Host, Cap::Text(64)),
        (Key::Reason, "reason", Tier::Content, Cap::Text(212)),
        (Key::AttnText, "attn_text", Tier::Content, Cap::Text(256)),
        (Key::TopicName, "topic_name", Tier::Content, Cap::Text(64)),
        (Key::Closer, "closer", Tier::Content, Cap::Text(64)),
    ];
    let first = at(Key::Pool).map(|i| i + 1).expect("6C-6's key");
    for (i, (key, word, tier, cap)) in rows.iter().enumerate() {
        assert_eq!(at(*key), Some(first + i), "{key:?} in its place");
        assert_eq!(
            (key.word(), key.tier(), key.cap()),
            (*word, *tier, *cap),
            "{key:?}"
        );
    }
    assert_eq!(
        FIELDS.get(first + rows.len()).map(|r| r.0),
        Some(Key::Change),
        "6F-5's `change=` follows `closer=`"
    );
    assert_eq!(
        &WHY_WORDS[WHY_WORDS.len() - 6..WHY_WORDS.len() - 4],
        ["mail", "burst"],
        "6F-6's two follow"
    );
    for w in ["spawning", "alive", "exited", "closed"] {
        assert!(Fields::new().word(Key::State, w).is_ok(), "session {w}");
    }
    for w in ["idle", "working", "blocked", "done", "error"] {
        assert!(Fields::new().word(Key::State, w).is_ok(), "program {w}");
    }
    assert!(Fields::new().word(Key::Ask, "permission").is_ok());
    assert!(Fields::new().word(Key::Ask, "teleport").is_err(), "closed");
    assert!(Fields::new().word(Key::Origin, "fleet").is_ok());
    assert!(Fields::new().word(Key::Origin, "anyone").is_err(), "closed");

    let wire = Fields::new()
        .u(Key::Build, 7)
        .and_then(|f| f.u(Key::Run, 3))
        .and_then(|f| f.i(Key::Displaced, -2))
        .and_then(|f| f.u(Key::At, 500))
        .and_then(|f| f.u(Key::Bytes, 42))
        .and_then(|f| f.u(Key::Producer, 9))
        .and_then(|f| f.tok(Key::Reverted, "alt,kitty"))
        .and_then(|f| f.word(Key::Origin, "fleet"))
        .and_then(|f| f.word(Key::Ask, "auth"))
        .and_then(|f| f.u(Key::Coalesced, 29))
        .expect("the wire keys");
    let rest = Fields::new()
        .id(Key::Holder, "codex-alab@41")
        .and_then(|f| f.text(Key::Via, "h-relay1,h-relay2"))
        .and_then(|f| f.text(Key::Idem, "QZP6KEY1"))
        .and_then(|f| f.text(Key::Reason, "QZP6REASON1"))
        .and_then(|f| f.text(Key::AttnText, "QZP6ATTN1 needs you"))
        .and_then(|f| f.text(Key::TopicName, "QZP6TOPIC1"))
        .and_then(|f| f.text(Key::Closer, "s-closer9"))
        .expect("the host and content keys");
    for f in [wire, rest] {
        let r = Rec {
            seq: 40,
            t: 9,
            op: Op::Mark,
            node: Node::SESSION,
            par: None,
            k: Kind::Program,
            src: Src::Engine,
            conf: Conf::Claim,
            by: Actor::Nobody,
            f,
        };
        let content = encode(&r, Tier::Content).expect("fits");
        assert_eq!(decode(&content), Ok(r.clone()), "{content}");
        let host = encode(&r, Tier::Host).expect("fits");
        let wire = encode(&r, Tier::Wire).expect("fits");
        for (key, _, tier, _) in &rows {
            let token = format!(" {}=", key.word());
            assert_eq!(
                host.contains(&token),
                r.f.get(*key).is_some() && *tier <= Tier::Host,
                "{key:?} at the host tier: {host}"
            );
            assert_eq!(
                wire.contains(&token),
                r.f.get(*key).is_some() && *tier == Tier::Wire,
                "{key:?} at the wire tier: {wire}"
            );
        }
        for planted in ["QZP6REASON1", "QZP6ATTN1", "QZP6TOPIC1", "s-closer9"] {
            assert!(!host.contains(planted), "{planted} below content: {host}");
        }
        let mut older = content.replace(" program ", " zz-kind-of-phase-9 ");
        for (key, ..) in &rows {
            older = older.replace(
                &format!(" {}=", key.word()),
                &format!(" zz_{}=", key.word()),
            );
        }
        let read = decode(&older).expect("an older reader still reads it");
        assert_eq!(read.k, Kind::Unknown, "{older}");
        assert_eq!(read.f.iter().count(), 0, "every new key skipped: {older}");
    }
}

#[test]
fn phase_6_ring_row_keys_are_appended_at_their_tiers() {
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    let rows: [(Key, &str, Tier, Cap); 7] = [
        (Key::Change, "change", Tier::Wire, Cap::Word(CHANGE_WORDS)),
        (Key::Since, "since", Tier::Content, Cap::Text(64)),
        (Key::Dl, "dl", Tier::Wire, Cap::U),
        (
            Key::HoldOrigin,
            "hold_origin",
            Tier::Wire,
            Cap::Tok(aterm_slog::Charset::Rollup, 64),
        ),
        (Key::Floor, "floor", Tier::Host, Cap::U),
        (Key::Seen, "seen", Tier::Wire, Cap::U),
        (Key::Moved, "moved", Tier::Wire, Cap::U),
    ];
    let first = at(Key::Closer).map(|i| i + 1).expect("6F-2's last key");
    for (i, (key, word, tier, cap)) in rows.iter().enumerate() {
        assert_eq!(at(*key), Some(first + i), "{key:?} in its place");
        assert_eq!(
            (key.word(), key.tier(), key.cap()),
            (*word, *tier, *cap),
            "{key:?}"
        );
    }
    assert_eq!(at(Key::Net), Some(first + rows.len()), "6F-7's keys follow");
    assert_eq!(CHANGE_WORDS, ["add", "drop"]);
    assert!(Fields::new().word(Key::Change, "rename").is_err(), "closed");
    assert_eq!(
        &WHY_WORDS[WHY_WORDS.len() - 4..WHY_WORDS.len() - 2],
        ["not-carried", "log-replaced"]
    );
    assert_eq!(
        &WHY_WORDS[WHY_WORDS.len() - 2..],
        ["signin", "install"],
        "a worker launcher's two refusals (task `why=`) follow"
    );

    let mark = |k: Kind, f: Fields| Rec {
        seq: 41,
        t: 9,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k,
        src: Src::Fabric,
        conf: Conf::Exact,
        by: Actor::Nobody,
        f,
    };
    let marks = [
        (
            mark(
                Kind::Topic,
                Fields::new()
                    .word(Key::Change, "add")
                    .and_then(|f| f.id(Key::Fp, "00ff00ff00ff00ff"))
                    .and_then(|f| f.text(Key::TopicName, "QZP6TOPIC2"))
                    .and_then(|f| f.text(Key::Since, "@QZP6SINCE2"))
                    .expect("a topic mark's fields"),
            ),
            &["QZP6TOPIC2", "QZP6SINCE2"][..],
        ),
        (
            mark(
                Kind::Mail,
                Fields::new()
                    .word(Key::Dir, "out")
                    .and_then(|f| f.u(Key::Inbox, 3))
                    .and_then(|f| f.u(Key::Dl, 60_000))
                    .and_then(|f| f.text(Key::Idem, "QZP6IDEM2"))
                    .expect("a post's fields"),
            ),
            &[][..],
        ),
        (
            mark(
                Kind::Hold,
                Fields::new()
                    .word(Key::State, "on")
                    .and_then(|f| f.word(Key::Origin, "local"))
                    .and_then(|f| f.tok(Key::HoldOrigin, "fleet,local"))
                    .and_then(|f| f.text(Key::Reason, "QZP6REASON2"))
                    .expect("a hold's fields"),
            ),
            &["QZP6REASON2"][..],
        ),
        (
            mark(
                Kind::MailSeen,
                Fields::new()
                    .u(Key::Inbox, 2)
                    .and_then(|f| f.u(Key::Off, 7))
                    .and_then(|f| f.u(Key::Floor, 8))
                    .and_then(|f| f.u(Key::Seen, 2))
                    .and_then(|f| f.u(Key::Moved, 0))
                    .expect("a seen's fields"),
            ),
            &[][..],
        ),
    ];
    for (r, planted) in &marks {
        let content = encode(r, Tier::Content).expect("fits");
        assert_eq!(decode(&content), Ok(r.clone()), "{content}");
        let host = encode(r, Tier::Host).expect("fits");
        let wire = encode(r, Tier::Wire).expect("fits");
        for (key, _, tier, _) in rows.iter().filter(|(k, ..)| r.f.get(*k).is_some()) {
            let token = format!(" {}=", key.word());
            assert_eq!(
                host.contains(&token),
                *tier <= Tier::Host,
                "{key:?}: {host}"
            );
            assert_eq!(
                wire.contains(&token),
                *tier == Tier::Wire,
                "{key:?}: {wire}"
            );
        }
        for planted in *planted {
            assert!(content.contains(planted), "{planted} at content: {content}");
            assert!(!host.contains(planted), "{planted} below content: {host}");
        }
        let mut older = content.clone();
        for (key, ..) in &rows {
            older = older.replace(
                &format!(" {}=", key.word()),
                &format!(" zz_{}=", key.word()),
            );
        }
        let read = decode(&older).expect("an older reader still reads it");
        assert!(
            rows.iter().all(|(key, ..)| read.f.get(*key).is_none()),
            "every new key skipped: {older}"
        );
    }
    let topic = encode(&marks[0].0, Tier::Wire).expect("fits");
    assert!(topic.contains(" fp=00ff00ff00ff00ff "), "{topic}");
    let post = encode(&marks[1].0, Tier::Wire).expect("fits");
    assert!(
        !post.contains("QZP6IDEM2"),
        "a post's idem= is host: {post}"
    );
    let hold = encode(&marks[2].0, Tier::Wire).expect("fits");
    assert!(hold.contains(" hold_origin=fleet,local "), "{hold}");
}

#[test]
fn phase_6_session_row_keys_are_appended_at_their_tiers() {
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    let rows: [(Key, &str, Tier, Cap); 14] = [
        (Key::Net, "net", Tier::Wire, Cap::Word(NET_WORDS)),
        (Key::AfterMs, "after_ms", Tier::Wire, Cap::U),
        (Key::ResizeEpoch, "resize_epoch", Tier::Wire, Cap::U),
        (Key::Pushed, "pushed", Tier::Wire, Cap::I),
        (Key::Cut, "cut", Tier::Wire, Cap::U),
        (Key::Drew, "drew", Tier::Wire, Cap::U),
        (Key::Class, "class", Tier::Wire, Cap::Word(CLASS_WORDS)),
        (
            Key::HealedBy,
            "healed_by",
            Tier::Wire,
            Cap::Word(HEALED_BY_WORDS),
        ),
        (Key::Source, "source", Tier::Wire, Cap::Word(SOURCE_WORDS)),
        (Key::FromPg, "from_pg", Tier::Host, Cap::I),
        (Key::ToPg, "to_pg", Tier::Host, Cap::I),
        (Key::KeySeq, "key_seq", Tier::Wire, Cap::U),
        (Key::Reply, "reply", Tier::Content, Cap::Text(64)),
        (Key::Build, "build", Tier::Wire, Cap::U),
    ];
    let first = at(Key::Moved).map(|i| i + 1).expect("6F-6's last key");
    for (i, (key, word, tier, cap)) in rows[..13].iter().enumerate() {
        assert_eq!(at(*key), Some(first + i), "{key:?} in its place");
        assert_eq!(
            (key.word(), key.tier(), key.cap()),
            (*word, *tier, *cap),
            "{key:?}"
        );
    }
    assert_eq!(
        at(Key::HarnessAct),
        Some(first + 13),
        "the review's `harness_act=` follows"
    );
    assert_eq!(NET_WORDS, ["zero", "changed"]);
    assert_eq!(CLASS_WORDS, ["agent", "learned", "unknown"]);
    assert_eq!(HEALED_BY_WORDS, ["app", "undo", "nudge"]);
    assert_eq!(SOURCE_WORDS, ["ctl", "menu"]);
    for (key, word) in [
        (Key::Net, "same"),
        (Key::Class, "shell"),
        (Key::HealedBy, "-"),
        (Key::Source, "palette"),
    ] {
        assert!(Fields::new().word(key, word).is_err(), "{key:?} {word}");
    }
    let mark = |k: Kind, f: Fields| Rec {
        seq: 51,
        t: 9,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k,
        src: Src::Aterm,
        conf: Conf::Exact,
        by: Actor::Nobody,
        f,
    };
    let marks = [
        mark(
            Kind::Render,
            Fields::new()
                .word(Key::State, "desync-risk")
                .and_then(|f| f.u(Key::ResizeEpoch, 4))
                .and_then(|f| f.i(Key::Displaced, -2))
                .and_then(|f| f.i(Key::Pushed, 1))
                .and_then(|f| f.u(Key::Cut, 6))
                .and_then(|f| f.u(Key::Drew, 1))
                .and_then(|f| f.word(Key::Class, "learned"))
                .and_then(|f| f.u(Key::At, 70))
                .expect("an epoch's risk"),
        ),
        mark(
            Kind::Render,
            Fields::new()
                .word(Key::State, "healed")
                .and_then(|f| f.u(Key::Run, 2))
                .and_then(|f| f.word(Key::Net, "changed"))
                .and_then(|f| f.u(Key::AfterMs, 40))
                .and_then(|f| f.word(Key::HealedBy, "undo"))
                .expect("a healed verdict"),
        ),
        mark(
            Kind::Modes,
            Fields::new()
                .u(Key::N, 2)
                .and_then(|f| f.word(Key::Source, "menu"))
                .and_then(|f| f.i(Key::FromPg, 300))
                .and_then(|f| f.i(Key::ToPg, 301))
                .and_then(|f| f.tok(Key::Reverted, "alt,kitty"))
                .and_then(|f| f.u(Key::Bytes, 42))
                .expect("a modes mark"),
        ),
        mark(
            Kind::InDoubt,
            Fields::new()
                .word(Key::Verb, "send")
                .and_then(|f| f.u(Key::Producer, 3))
                .and_then(|f| f.u(Key::KeySeq, 9))
                .and_then(|f| f.text(Key::Reply, "QZP6REPLY1"))
                .expect("an in-doubt mark"),
        ),
        mark(
            Kind::Handoff,
            Fields::new()
                .u(Key::Carried, 4)
                .and_then(|f| f.u(Key::Build, 7))
                .expect("a handoff mark"),
        ),
    ];
    for r in &marks {
        let content = encode(r, Tier::Content).expect("fits");
        assert_eq!(decode(&content), Ok(r.clone()), "{content}");
        let host = encode(r, Tier::Host).expect("fits");
        let wire = encode(r, Tier::Wire).expect("fits");
        for (key, _, tier, _) in rows.iter().filter(|(k, ..)| r.f.get(*k).is_some()) {
            let token = format!(" {}=", key.word());
            assert_eq!(
                host.contains(&token),
                *tier <= Tier::Host,
                "{key:?}: {host}"
            );
            assert_eq!(
                wire.contains(&token),
                *tier == Tier::Wire,
                "{key:?}: {wire}"
            );
        }
        assert!(
            !host.contains("QZP6REPLY1"),
            "a reply below content: {host}"
        );
        let mut older = content.clone();
        for (key, ..) in &rows[..13] {
            older = older.replace(
                &format!(" {}=", key.word()),
                &format!(" zz_{}=", key.word()),
            );
        }
        let read = decode(&older).expect("an older reader still reads it");
        assert!(
            rows[..13]
                .iter()
                .all(|(key, ..)| read.f.get(*key).is_none()),
            "every new key skipped: {older}"
        );
    }
    assert!(
        encode(&marks[3], Tier::Content)
            .expect("fits")
            .split(' ')
            .any(|t| t == "reply=QZP6REPLY1"),
        "the reply at content"
    );
    let handoff = encode_bus(&marks[4], Tier::Wire, 3, Some(1)).expect("fits");
    assert!(
        handoff.split(' ').any(|t| t == "build=7"),
        "a handoff's build is wire: {handoff}"
    );
}

#[test]
fn phase_6_harness_act_is_appended_host_only() {
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    assert_eq!(at(Key::HarnessAct), at(Key::Reply).map(|i| i + 1));
    assert_eq!(
        at(Key::LandAt),
        at(Key::HarnessAct).map(|i| i + 1),
        "6F-8's `land_at=` follows"
    );
    assert_eq!(
        (
            Key::HarnessAct.word(),
            Key::HarnessAct.tier(),
            Key::HarnessAct.cap()
        ),
        ("harness_act", Tier::Host, Cap::Text(64))
    );
    let act = "forever/land@v2";
    let r = Rec {
        seq: 52,
        t: 9,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k: Kind::Act,
        f: Fields::new()
            .word(Key::Word, "unknown")
            .and_then(|f| f.id(Key::Holder, "codex-alab@41"))
            .and_then(|f| f.text(Key::HarnessAct, act))
            .expect("an act mark's fields"),
        src: Src::Mark,
        conf: Conf::Claim,
        by: Actor::Ctl,
    };
    let host = encode(&r, Tier::Host).expect("fits");
    assert_eq!(
        host,
        "52 mark - act word=unknown holder=codex-alab@41 harness_act=forever/land@v2 by=ctl \
         src=mark conf=claim t=9"
    );
    assert_eq!(decode(&host), Ok(r.clone()), "{host}");
    let wire = encode(&r, Tier::Wire).expect("fits");
    assert!(
        !wire.contains("harness_act=") && !wire.contains(act),
        "{wire}"
    );
    let older = host.replace(" harness_act=", " zz_harness_act=");
    let mut without = r.clone();
    without.f.remove(Key::HarnessAct);
    assert_eq!(decode(&older), Ok(without), "{older}");
    assert!(
        Fields::new().id(Key::Rule, act).is_err(),
        "no rule holds it"
    );
}

#[test]
fn phase_6_land_at_is_appended_host_only() {
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    assert_eq!(at(Key::LandAt), at(Key::HarnessAct).map(|i| i + 1));
    assert_eq!(
        at(Key::Sev),
        at(Key::LandAt).map(|i| i + 1),
        "6F-9's `sev=` follows"
    );
    assert_eq!(
        (Key::LandAt.word(), Key::LandAt.tier(), Key::LandAt.cap()),
        ("land_at", Tier::Host, Cap::I)
    );
    assert_eq!(Key::Value.cap(), Cap::Text(256));
    let r = Rec {
        seq: 53,
        t: 9,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k: Kind::Meta,
        f: Fields::new()
            .tok(Key::Field, "autopilot")
            .and_then(|f| f.text(Key::Value, "cruise"))
            .and_then(|f| f.i(Key::LandAt, -1))
            .expect("a meta mark's fields"),
        src: Src::Meta,
        conf: Conf::Exact,
        by: Actor::Ctl,
    };
    let host = encode(&r, Tier::Host).expect("fits");
    assert_eq!(
        host,
        "53 mark - meta field=autopilot value=cruise land_at=-1 by=ctl src=meta conf=exact t=9"
    );
    assert_eq!(decode(&host), Ok(r.clone()), "{host}");
    let wire = encode(&r, Tier::Wire).expect("fits");
    assert!(!wire.contains("land_at="), "{wire}");
    let older = host.replace(" land_at=", " zz_land_at=");
    let mut without = r.clone();
    without.f.remove(Key::LandAt);
    assert_eq!(decode(&older), Ok(without), "{older}");
    assert!(
        Fields::new().u(Key::LandAt, 1).is_err(),
        "NEGATIVE CONTROL: a timer is signed, no U"
    );
}

#[test]
fn phase_6_shown_keys_are_appended_wire_and_the_mark_is_local() {
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    assert_eq!(at(Key::Sev), at(Key::LandAt).map(|i| i + 1));
    assert_eq!(at(Key::Held), at(Key::Sev).map(|i| i + 1));
    assert_eq!(
        at(Key::RowKind),
        at(Key::Held).map(|i| i + 1),
        "`row=` follows `held=`"
    );
    assert_eq!(
        (Key::Sev.word(), Key::Sev.tier(), Key::Sev.cap()),
        ("sev", Tier::Wire, Cap::Word(SEV_WORDS))
    );
    assert_eq!(
        (Key::Held.word(), Key::Held.tier(), Key::Held.cap()),
        ("held", Tier::Wire, Cap::Word(HELD_WORDS))
    );
    assert_eq!(SEV_WORDS, ["stuck", "decide"]);
    assert_eq!(HELD_WORDS, ["person", "remote", "local", "bridge"]);
    for w in [
        "frozen", "stopped", "stalled", "login", "memory", "wall", "ask", "approval", "question",
    ] {
        assert!(NEED_WORDS.contains(&w), "{w}");
    }
    let r = Rec {
        seq: 54,
        t: 9,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k: Kind::Shown,
        f: Fields::new()
            .word(Key::Need, "approval")
            .and_then(|f| f.word(Key::Sev, "decide"))
            .and_then(|f| f.word(Key::Held, "person"))
            .and_then(|f| f.u(Key::Coalesced, 3))
            .expect("a shown mark's fields"),
        src: Src::Aterm,
        conf: Conf::Strong,
        by: Actor::Aterm,
    };
    let line = encode(&r, Tier::Wire).expect("fits");
    assert_eq!(
        line,
        "54 mark - shown need=approval sev=decide held=person coalesced=3 by=aterm src=aterm \
         conf=strong t=9"
    );
    assert_eq!(decode(&line), Ok(r.clone()), "{line}");
    assert!(!r.is_bus_class(), "local: no bus class");
    assert_eq!(
        aterm_slog::encode_mirror(&r, Tier::Wire, 1, None, &|_| None),
        Err(EncodeError::NotBusClass)
    );
    let older = line
        .replace(" shown ", " zz-kind-of-phase-9 ")
        .replace(" sev=", " zz_sev=")
        .replace(" held=", " zz_held=");
    let read = decode(&older).expect("an older reader still reads it");
    assert_eq!(read.k, Kind::Unknown, "{older}");
    assert_eq!(read.f.get(Key::Sev), None, "{older}");
    assert_eq!(read.f.get(Key::Held), None, "{older}");
    assert!(
        Fields::new().word(Key::Need, "-").is_err(),
        "none is no word"
    );
    assert!(
        Fields::new().word(Key::Sev, "-").is_err(),
        "none is no word"
    );
    assert!(
        Fields::new().word(Key::Held, "fleet").is_err(),
        "an origin is no hold word"
    );
}

#[test]
fn phase_6_row_mark_is_appended_local_and_its_text_never_leaves_ram() {
    let kind_at = |k: Kind| Kind::ALL.iter().position(|x| *x == k);
    assert_eq!(kind_at(Kind::Row), kind_at(Kind::Shown).map(|i| i + 1));
    assert_eq!(Kind::Row.word(), "row");
    assert!(Kind::Row.is_mark() && Kind::Row.node_level().is_none());
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    assert_eq!(at(Key::RowText), at(Key::RowKind).map(|i| i + 1));
    assert_eq!(
        at(Key::FactRev),
        at(Key::RowText).map(|i| i + 1),
        "the fact word's keys follow"
    );
    assert_eq!(
        (Key::RowKind.word(), Key::RowKind.tier(), Key::RowKind.cap()),
        ("row", Tier::Wire, Cap::Tok(aterm_slog::Charset::Lower, 32))
    );
    assert_eq!(
        (Key::RowText.word(), Key::RowText.tier(), Key::RowText.cap()),
        ("row_text", Tier::Content, Cap::Text(360))
    );
    let text = format!("QZP6ROW1 {}", " ".repeat(256));
    let r = Rec {
        seq: 4_294_967_296,
        t: 1_760_545_012_345,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k: Kind::Row,
        f: Fields::new()
            .tok(Key::RowKind, "spare-adopted")
            .and_then(|f| f.text(Key::RowText, &text))
            .expect("a row mark's fields"),
        src: Src::Aterm,
        conf: Conf::Exact,
        by: Actor::Aterm,
    };
    let content = encode(&r, Tier::Content).expect("the longest fits");
    assert!(content.len() <= MAX_RECORD_BYTES, "{}", content.len());
    assert_eq!(decode(&content), Ok(r.clone()), "{content}");
    assert!(content.contains(" row=spare-adopted row_text=QZP6ROW1"));
    let host = encode(&r, Tier::Host).expect("fits");
    let wire = encode(&r, Tier::Wire).expect("fits");
    for line in [&host, &wire] {
        assert!(line.contains(" row=spare-adopted "), "{line}");
        assert!(
            !line.contains("row_text=") && !line.contains("QZP6ROW1"),
            "{line}"
        );
    }
    assert!(!r.is_bus_class() && !r.is_turn_class(), "local");
    assert_eq!(
        aterm_slog::encode_mirror(&r, Tier::Wire, 1, None, &|_| None),
        Err(EncodeError::NotBusClass)
    );
    let older = content
        .replace(" row ", " zz-kind-of-phase-9 ")
        .replace(" row=", " zz_row=")
        .replace(" row_text=", " zz_row_text=");
    let read = decode(&older).expect("an older reader still reads it");
    assert_eq!(read.k, Kind::Unknown, "{older}");
    assert_eq!(read.f.iter().count(), 0, "both keys skipped: {older}");
    let mut leaky = r.clone();
    leaky.f = Fields::new()
        .text(Key::Text, "QZP6ROW1")
        .expect("a host text");
    assert!(
        encode(&leaky, Tier::Host)
            .expect("fits")
            .contains("QZP6ROW1"),
        "CONTROL: the host tier holds a host key's text"
    );
}

/// A9 (B22): THE MAIL GAP MARK is appended last, local, and names no key of its own. A
/// broker's gap notice says which offsets of the node's mail lanes retention gave up unread
/// (`off=` the tail's cursor, `floor=` the oldest offset the lane still reaches, both host
/// facts as every mail offset is) and, where the broker counted them, how many (`lost=`, a
/// wire count). The host tier holds the range, the wire tier the count alone; it never crosses
/// the bus (mail crosses only as counts); an older reader reads an unknown kind and its keys.
/// The fold counts no mail arrival for it. CONTROL: the same fields as a `mail dir=in` mark count
/// one, so the count is the kind's.
#[test]
fn a9_mail_gap_is_appended_local_at_its_keys_tiers() {
    let kind_at = |k: Kind| Kind::ALL.iter().position(|x| *x == k);
    assert_eq!(Kind::ALL.last(), Some(&Kind::MailGap), "the last kind");
    assert_eq!(
        kind_at(Kind::MailGap),
        kind_at(Kind::Row).map(|i| i + 1),
        "it follows phase 6's row mark"
    );
    assert_eq!(Kind::MailGap.word(), "mail-gap");
    assert!(Kind::MailGap.is_mark() && Kind::MailGap.node_level().is_none());
    assert_eq!(Kind::from_word("mail-gap"), Kind::MailGap);
    assert_eq!(
        [Key::Off, Key::Floor, Key::Lost].map(|k| (k.word(), k.tier(), k.cap())),
        [
            ("off", Tier::Host, Cap::U),
            ("floor", Tier::Host, Cap::U),
            ("lost", Tier::Wire, Cap::U),
        ],
        "its keys, at the tiers every mail offset and every count has"
    );
    let r = Rec {
        seq: 51,
        t: 1_760_545_012_345,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k: Kind::MailGap,
        f: Fields::new()
            .u(Key::Off, 7)
            .and_then(|f| f.u(Key::Floor, 42))
            .and_then(|f| f.u(Key::Lost, 3))
            .expect("a mail gap's fields"),
        src: Src::Fabric,
        conf: Conf::Exact,
        by: Actor::Bridge,
    };
    let host = encode(&r, Tier::Host).expect("fits");
    assert_eq!(
        host,
        "51 mark - mail-gap off=7 floor=42 lost=3 by=bridge src=fabric conf=exact t=1760545012345"
    );
    assert_eq!(decode(&host), Ok(r.clone()), "{host}");
    let wire = encode(&r, Tier::Wire).expect("fits");
    assert!(
        wire.contains(" lost=3 ") && !wire.contains("off=") && !wire.contains("floor="),
        "{wire}"
    );
    assert!(!r.is_bus_class() && !r.is_turn_class(), "local");
    let mut unknown = r.clone();
    unknown.f.remove(Key::Lost);
    let unknown = encode(&unknown, Tier::Host).expect("fits");
    assert!(
        !unknown.contains("lost="),
        "a count the broker did not give: {unknown}"
    );
    let older = host
        .replace(" mail-gap ", " zz-kind-of-phase-9 ")
        .replace(" lost=", " zz_lost=")
        .replace(" floor=", " zz_floor=")
        .replace(" off=", " zz_off=");
    let read = decode(&older).expect("an older reader still reads it");
    assert_eq!(read.k, Kind::Unknown, "{older}");
    assert_eq!(read.f.iter().count(), 0, "every key skipped: {older}");
    let folded = |k: Kind, f: Fields| {
        let mut log = aterm_slog::Log::new(0);
        let mark = aterm_slog::Mark::new(k, Src::Fabric, Conf::Exact)
            .by(Actor::Bridge)
            .fields(f);
        log.mark(mark, crate::support::at(1)).expect("a mark");
        (log.state().totals.mail_in, log.state().mail_open)
    };
    assert_eq!(
        folded(Kind::MailGap, r.f.clone()),
        (0, 0),
        "a give-up is no mail arrival"
    );
    let arrival = r.f.clone().word(Key::Dir, "in").expect("a direction");
    assert_eq!(
        folded(Kind::Mail, arrival),
        (1, 1),
        "CONTROL: the same fields as a `mail dir=in` mark are one"
    );
}

#[test]
fn occupancy_fact_keys_are_appended_wire_and_an_older_reader_skips_them() {
    let at = |key: Key| FIELDS.iter().position(|(k, _, _)| *k == key);
    let first = at(Key::RowText).map(|i| i + 1).expect("phase 6's last key");
    for (i, (key, word, cap)) in [
        (Key::FactRev, "fact_rev", Cap::U),
        (Key::FactRule, "fact_rule", Cap::Word(FACT_RULE_WORDS)),
        (Key::FactN, "fact_n", Cap::U),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(at(key), Some(first + i), "{key:?} in its place");
        assert_eq!(
            (key.word(), key.tier(), key.cap()),
            (word, Tier::Wire, cap),
            "{key:?}"
        );
    }
    assert_eq!(
        FIELDS.len(),
        at(Key::FactN).map_or(0, |i| i + 1),
        "the last key"
    );
    assert_eq!(
        FACT_RULE_WORDS,
        ["wait-nudge@v1", "hold-room@v1", "wake@v1"]
    );
    assert!(
        Fields::new().word(Key::FactRule, "hold-room").is_err(),
        "a rule is named with its version"
    );
    let r = rec(
        Op::Mark,
        Node::SESSION,
        Kind::Task,
        Fields::new()
            .u(Key::Rev, 3)
            .and_then(|f| f.u(Key::FactRev, 2))
            .and_then(|f| f.word(Key::FactRule, "hold-room@v1"))
            .and_then(|f| f.u(Key::FactN, 1))
            .expect("a task mark's fact fields"),
    );
    let wire = encode(&r, Tier::Wire).expect("fits");
    assert!(
        wire.contains(" fact_rev=2 fact_rule=hold-room@v1 fact_n=1 "),
        "{wire}"
    );
    assert_eq!(decode(&wire), Ok(r.clone()), "{wire}");
    let older = wire
        .replace(" fact_rev=", " zz_fact_rev=")
        .replace(" fact_rule=", " zz_fact_rule=")
        .replace(" fact_n=", " zz_fact_n=");
    let read = decode(&older).expect("an older reader still reads it");
    assert_eq!(read.f.get(Key::Rev), Some(&Val::U(3)), "{older}");
    for key in [Key::FactRev, Key::FactRule, Key::FactN] {
        assert_eq!(read.f.get(key), None, "{key:?} skipped: {older}");
    }
}
