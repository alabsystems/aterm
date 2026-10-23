// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::scrollback::serialize_lines;
use aterm_core::terminal::{ShellIntegrationNonce, Terminal};

fn head_for(t: &Terminal, seq: u64, bridge: u32) -> KeyframeHead {
    KeyframeHead {
        seq,
        tail_from: 96,
        stream_at: 1234,
        resize_lifetime: t.resize_lifetime(),
        resize_ordinal: t.resize_ordinal(),
        fence: t.history_fence(),
        bridge_lines: bridge,
        config_fp: 0xC0FF_EE00,
        integration_on: t.shell_integration_posture()
            == aterm_core::terminal::ShellIntegrationPosture::On,
    }
}

fn shell_with_history(rows: u16, cols: u16, lines: usize) -> Terminal {
    let mut t = Terminal::new(rows, cols);
    for i in 0..lines {
        t.process(format!("\x1b[3{}mline {i}\x1b[0m\r\n", i % 8).as_bytes());
    }
    t
}

/// The decompressed body of a keyframe record, for tests that tamper with it.
fn body_of(record: &[u8], seq: u64) -> Vec<u8> {
    let compressed = one_step(record, seq).expect("one step");
    bounded_lz4(compressed, MAX_KEYFRAME_BODY).expect("decompresses")
}

/// A keyframe record around `body` (already a KFR body), as the writer frames one.
fn record_of(body: &[u8], seq: u64) -> Vec<u8> {
    encode_step(seq, &aterm_lz4::compress_prepend_size(body)).expect("frames")
}

/// Every tail record kind round-trips through its header and payload; the PAD header
/// spans exactly its gap.
#[test]
fn tail_records_round_trip() {
    let bytes = b"\x1b[31mhello\x1b[0m";
    let records = [
        TailRecord::Bytes {
            synthesized: false,
            bytes,
        },
        TailRecord::Bytes {
            synthesized: true,
            bytes: b"\x1bc",
        },
        TailRecord::Resize {
            rows: 63,
            cols: 120,
            policy: ResizePolicy::ConPty,
            lifetime: 9,
            ordinal: 41,
        },
        TailRecord::ResizesLost { n: 3 },
        TailRecord::EngineReplaced,
        TailRecord::Superseded,
    ];
    for record in records {
        let mut out = Vec::new();
        record.encode(777, &mut out);
        assert_eq!(out.len(), record.encoded_len());
        assert_eq!(out.len() % 8, 0, "8-aligned");
        let head: [u8; TAIL_RECORD_HEADER] = out[..TAIL_RECORD_HEADER].try_into().expect("16");
        let header = TailHeader::read(&head);
        assert_eq!(header.stream_end, 777);
        assert_eq!(header.record_len(), out.len());
        let payload = &out[TAIL_RECORD_HEADER..TAIL_RECORD_HEADER + record.payload_len()];
        assert_eq!(decode_tail_payload(&header, payload), Ok(record));
    }
    let pad = TailHeader::read(&pad_header(40, 5));
    assert_eq!(
        (pad.kind, pad.record_len(), pad.stream_end),
        (KIND_PAD, 40, 5)
    );
}

/// A record the reader does not understand stops the replay with the fault named.
#[test]
fn a_bad_tail_record_is_refused_not_guessed() {
    let header = |kind, flags, reserved, payload_len| TailHeader {
        kind,
        flags,
        reserved,
        payload_len,
        stream_end: 0,
    };
    assert_eq!(
        decode_tail_payload(&header(9, 0, 0, 0), &[]),
        Err(TailFault::UnknownKind(9))
    );
    assert_eq!(
        decode_tail_payload(&header(KIND_BYTES, 2, 0, 1), b"x"),
        Err(TailFault::BadFlags)
    );
    assert_eq!(
        decode_tail_payload(&header(KIND_BYTES, 0, 1, 1), b"x"),
        Err(TailFault::BadFlags),
        "reserved bits"
    );
    assert_eq!(
        decode_tail_payload(&header(KIND_BYTES, 0, 0, 2), b"x"),
        Err(TailFault::BadLength)
    );
    assert_eq!(
        decode_tail_payload(&header(KIND_RESIZE, 2, 0, 24), &[0; 24]),
        Err(TailFault::BadFlags),
        "an unknown policy"
    );
    assert_eq!(
        decode_tail_payload(&header(KIND_RESIZE, 0, 0, 23), &[0; 23]),
        Err(TailFault::BadLength)
    );
    assert_eq!(
        decode_tail_payload(&header(KIND_RESIZES_LOST, 0, 0, 5), &[0; 5]),
        Err(TailFault::BadLength)
    );
    assert_eq!(
        decode_tail_payload(&header(KIND_ENGINE_REPLACED, 0, 0, 1), &[0]),
        Err(TailFault::BadLength)
    );
    assert_eq!(
        decode_tail_payload(&header(KIND_SUPERSEDED, 0, 0, 1), &[0]),
        Err(TailFault::BadLength)
    );
    assert_eq!(
        decode_tail_payload(&header(KIND_SUPERSEDED, 1, 0, 0), &[]),
        Err(TailFault::BadFlags)
    );
    assert_eq!(
        decode_tail_payload(&header(KIND_PAD, 0, 0, 0), &[]),
        Err(TailFault::UnknownKind(KIND_PAD)),
        "a PAD carries no content: the reader steps over it"
    );
}

/// A keyframe round-trips across main, mid-sequence, alternate-screen, and shell-integration states.
#[test]
fn a_keyframe_round_trips() {
    let mut cases = Vec::new();
    cases.push(("shell", shell_with_history(6, 30, 40)));
    let mut mid = shell_with_history(6, 30, 10);
    mid.process(b"\x1b]0;a title being wri");
    cases.push(("mid-sequence", mid));
    let mut alt = shell_with_history(6, 30, 40);
    alt.process(b"\x1b[?1049h\x1b[2Jfull screen \xe2\x9c\x93");
    cases.push(("alternate screen", alt));
    let mut keyed = shell_with_history(6, 30, 10);
    keyed.set_require_shell_integration_nonce(true);
    keyed.authorize_shell_integration([0x3C; 32]);
    cases.push(("integration on", keyed));
    for (label, t) in cases {
        let checkpoint = t.checkpoint_journal(16).expect("carried");
        let head = head_for(&t, 7, 16);
        let record = encode_keyframe(&head, &checkpoint).expect("encodes");
        let kf = decode_keyframe(&record, 7).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(kf.head, head, "{label}");
        assert_eq!(kf.head.integration_on, label == "integration on", "{label}");
        assert_eq!(kf.checkpoint, checkpoint, "{label}");
    }
}

/// The alternate screen's saved primary decodes at `rows + bridge` and is refused past it.
#[test]
fn the_alternate_screen_bridge_decodes_at_its_bound_and_not_past_it() {
    let mut t = shell_with_history(5, 20, 60);
    t.process(b"\x1b[?1049happ");
    let checkpoint = t.checkpoint_journal(12).expect("ground");
    let primary = checkpoint.alt_grid.as_deref().expect("the saved primary");
    assert_eq!(
        aterm_core::scrollback::deserialize_lines(primary).len(),
        5 + 12
    );
    let at_bound = encode_keyframe(&head_for(&t, 3, 12), &checkpoint).expect("encodes");
    assert!(
        decode_keyframe(&at_bound, 3).is_ok(),
        "rows + bridge decodes"
    );
    let past = encode_keyframe(&head_for(&t, 3, 11), &checkpoint).expect("encodes");
    assert_eq!(
        decode_keyframe(&past, 3),
        Err(KeyframeFault::Grid),
        "rows + bridge + 1 is refused"
    );
    let shell = shell_with_history(5, 20, 60);
    let main = shell.checkpoint_journal(12).expect("ground");
    assert!(
        decode_keyframe(
            &encode_keyframe(&head_for(&shell, 1, 12), &main).expect("e"),
            1
        )
        .is_ok()
    );
    assert_eq!(
        decode_keyframe(
            &encode_keyframe(&head_for(&shell, 1, 11), &main).expect("e"),
            1
        ),
        Err(KeyframeFault::Grid)
    );
}

/// No keyframe record holds the shell-integration nonce.
#[test]
fn no_keyframe_holds_the_nonce() {
    let key = [0x5Au8; 32];
    let hex = ShellIntegrationNonce(key).to_hex();
    let mut t = Terminal::new(4, 80);
    t.set_require_shell_integration_nonce(true);
    t.authorize_shell_integration(key);
    t.process(format!("\x1b]133;A;id={hex}\x07$ ").as_bytes());
    assert_eq!(t.shell_marks_verified(), 1, "integration is in use");

    let checkpoint = t.checkpoint_journal(64).expect("ground");
    let record = encode_keyframe(&head_for(&t, 1, 64), &checkpoint).expect("encodes");
    let body = body_of(&record, 1);
    let hex_of_hex: String = hex.bytes().map(|b| format!("{b:02x}")).collect();
    let holds_nonce = |haystack: &[u8]| {
        haystack.windows(hex.len()).any(|w| w == hex.as_bytes())
            || haystack.windows(key.len()).any(|w| w == key)
            || haystack
                .windows(hex_of_hex.len())
                .any(|w| w == hex_of_hex.as_bytes())
    };
    assert!(!holds_nonce(&record), "the nonce is nowhere in the record");
    assert!(!holds_nonce(&body), "nor in its body");

    t.process(format!("\r\n\x1b]133;A;id={hex}").as_bytes());
    assert!(
        t.checkpoint_journal(64).is_none(),
        "no keyframe is cut inside a signed mark"
    );
    let carried = t.checkpoint_continuing().expect("an OSC is carried");
    assert!(
        carried.parser_holds_shell_mark(),
        "control: the carry holds the mark"
    );
    assert_eq!(
        encode_keyframe(&head_for(&t, 1, 64), &carried),
        Err(EncodeError::Nonce),
        "the encoder refuses a carry that holds a mark"
    );
    let mut forged = checkpoint.clone();
    forged.parser = carried.parser.clone();
    forged.parser_ground = false;
    let mut f = Fields { bytes: &body };
    let fixed = f.take(103).expect("the fixed fields").to_vec();
    let _meta = f.blob().expect("meta");
    let grid = f.blob().expect("grid").to_vec();
    let alt = f.blob().expect("alt").to_vec();
    let forged_meta =
        aterm_json::to_string(&CheckpointMeta::from_checkpoint(&forged)).expect("meta");
    let mut forged_body = fixed;
    forged_body[1] &= !FLAG_PARSER_GROUND;
    put_blob(&mut forged_body, forged_meta.as_bytes()).expect("blob");
    put_blob(&mut forged_body, &grid).expect("blob");
    put_blob(&mut forged_body, &alt).expect("blob");
    assert!(
        holds_nonce(&forged_body),
        "control: the forged meta holds it"
    );
    assert_eq!(
        decode_keyframe(&record_of(&forged_body, 1), 1),
        Err(KeyframeFault::Nonce),
        "the decoder refuses a keyframe whose carry holds a mark"
    );

    let carry = t.checkpoint_carry(0).expect("ground");
    assert!(carry.shell_integration_nonce.is_some(), "control");
    assert_eq!(
        encode_keyframe(&head_for(&t, 1, 0), &carry),
        Err(EncodeError::Nonce)
    );

    let mut f = Fields { bytes: &body };
    let fixed = f.take(103).expect("the fixed fields").to_vec();
    let meta = std::str::from_utf8(f.blob().expect("meta")).expect("utf-8");
    let grid = f.blob().expect("grid").to_vec();
    let alt = f.blob().expect("alt").to_vec();
    let tampered = meta.replacen('{', &format!("{{\"shell_integration_nonce\":\"{hex}\","), 1);
    let mut body = fixed;
    put_blob(&mut body, tampered.as_bytes()).expect("blob");
    put_blob(&mut body, &grid).expect("blob");
    put_blob(&mut body, &alt).expect("blob");
    assert_eq!(
        decode_keyframe(&record_of(&body, 1), 1),
        Err(KeyframeFault::Nonce)
    );
}

/// A damaged keyframe is refused across corrupted bytes, mismatched steps, truncation, and invalid metadata.
#[test]
fn a_damaged_keyframe_is_refused() {
    let t = shell_with_history(4, 20, 8);
    let checkpoint = t.checkpoint_journal(8).expect("ground");
    let record = encode_keyframe(&head_for(&t, 5, 8), &checkpoint).expect("encodes");
    assert!(decode_keyframe(&record, 5).is_ok());

    for at in [0, 3, 12, record.len() / 2, record.len() - 1] {
        let mut flipped = record.clone();
        flipped[at] ^= 0x40;
        assert!(
            matches!(decode_keyframe(&flipped, 5), Err(KeyframeFault::Step(_))),
            "a flipped byte at {at}"
        );
    }
    assert!(matches!(
        decode_keyframe(&record, 6),
        Err(KeyframeFault::Step(StepFault::Frame(StepEnd::Fault { .. })))
    ));
    assert!(matches!(
        decode_keyframe(&record[..record.len() - 1], 5),
        Err(KeyframeFault::Step(_))
    ));

    let mut bomb = Vec::new();
    bomb.extend_from_slice(&u32::MAX.to_le_bytes());
    bomb.extend_from_slice(&[0u8; 16]);
    assert_eq!(
        decode_keyframe(&encode_step(5, &bomb).expect("frames"), 5),
        Err(KeyframeFault::Compressed)
    );

    let body = body_of(&record, 5);
    let mut f = Fields { bytes: &body };
    let fixed = f.take(103).expect("fixed").to_vec();
    let meta = std::str::from_utf8(f.blob().expect("meta")).expect("utf-8");
    let grid = f.blob().expect("grid").to_vec();
    let alt = f.blob().expect("alt").to_vec();
    let without_charset = meta.replacen("\"charset\"", "\"charzet\"", 1);
    assert_ne!(without_charset, meta);
    let mut missing = fixed.clone();
    put_blob(&mut missing, without_charset.as_bytes()).expect("blob");
    put_blob(&mut missing, &grid).expect("blob");
    put_blob(&mut missing, &alt).expect("blob");
    assert_eq!(
        decode_keyframe(&record_of(&missing, 5), 5),
        Err(KeyframeFault::Meta)
    );
    let mut trailing = body.clone();
    trailing.push(0);
    assert_eq!(
        decode_keyframe(&record_of(&trailing, 5), 5),
        Err(KeyframeFault::Truncated)
    );
    let mut versioned = body.clone();
    versioned[0] = 2;
    assert_eq!(
        decode_keyframe(&record_of(&versioned, 5), 5),
        Err(KeyframeFault::Version(2))
    );
    let mut reseq = body;
    reseq[8] ^= 1;
    assert_eq!(
        decode_keyframe(&record_of(&reseq, 5), 5),
        Err(KeyframeFault::Seq)
    );
}

/// A history chunk round-trips its head and its lines, and a damaged one is refused.
#[test]
fn a_history_chunk_round_trips() {
    let t = shell_with_history(4, 24, 30);
    let fence = t.history_fence();
    let lines = t
        .history_lines_since_fence(&fence, fence.oldest, CHUNK_LINES)
        .expect("the fence holds");
    assert!(!lines.is_empty());
    let identity = HistoryIdentity::of(&fence);
    let record = encode_history_chunk(9, fence.oldest, identity, &lines).expect("encodes");
    let body = one_step(&record, 9).expect("one step");
    let chunk = decode_history_chunk(9, body).expect("a head");
    assert_eq!(
        (chunk.first_row, chunk.n_lines as usize, chunk.identity),
        (fence.oldest, lines.len(), identity)
    );
    assert_eq!(
        serialize_lines(&chunk.lines().expect("decodes")),
        serialize_lines(&lines)
    );

    let mut lying = chunk.clone();
    lying.n_lines += 1;
    assert!(
        matches!(lying.lines(), Err(ChunkFault::Frame)),
        "a count the frame does not hold"
    );
    let mut short = body.to_vec();
    short.truncate(HCR_HEAD_LEN - 1);
    assert_eq!(decode_history_chunk(9, &short), Err(ChunkFault::Head));
    assert_eq!(
        encode_history_chunk(9, 0, identity, &vec![lines[0].clone(); CHUNK_LINES + 1]),
        Err(EncodeError::TooLarge)
    );
}

/// A resize is a fixed-width payload, unlike the variable BYTES record.
#[test]
fn resize_payloads_have_exactly_twenty_four_bytes() {
    let record = TailRecord::Resize {
        rows: 24,
        cols: 80,
        policy: ResizePolicy::Native,
        lifetime: 9,
        ordinal: 11,
    };
    let mut encoded = Vec::new();
    record.encode(0, &mut encoded);
    let head: [u8; TAIL_RECORD_HEADER] = encoded[..TAIL_RECORD_HEADER].try_into().expect("head");
    let header = TailHeader::read(&head);
    let payload = &encoded[TAIL_RECORD_HEADER..TAIL_RECORD_HEADER + RESIZE_PAYLOAD];
    assert_eq!(decode_tail_payload(&header, payload), Ok(record));
    let mut short = header;
    short.payload_len = (RESIZE_PAYLOAD - 1) as u32;
    assert_eq!(
        decode_tail_payload(&short, &payload[..RESIZE_PAYLOAD - 1]),
        Err(TailFault::BadLength)
    );
    let mut long = header;
    long.payload_len = (RESIZE_PAYLOAD + 1) as u32;
    let mut trailing = payload.to_vec();
    trailing.push(0);
    assert_eq!(
        decode_tail_payload(&long, &trailing),
        Err(TailFault::BadLength)
    );
}

/// The same small frame fits at its exact byte budget and is refused one byte below.
#[test]
fn history_frames_share_a_preallocation_output_budget() {
    let lines = vec![Line::from("bounded history")];
    let identity = HistoryIdentity {
        cols: 24,
        ..HistoryIdentity::default()
    };
    let bytes = serialize_lines(&lines);
    let budget = 4 + bytes.len();
    let record =
        encode_history_chunk_with_budget(0, 0, identity, &lines, budget).expect("exact budget");
    let chunk = decode_history_chunk(0, one_step(&record, 0).expect("one step")).expect("head");
    assert_eq!(
        serialize_lines(&chunk.lines_with_budget(budget).expect("exact budget")),
        bytes
    );
    assert_eq!(
        serialize_lines(&chunk.lines().expect("ordinary public decoder")),
        bytes
    );
    assert!(
        encode_history_chunk(0, 0, identity, &lines).is_ok(),
        "ordinary public encoder"
    );
    let encoder_refused = matches!(
        encode_history_chunk_with_budget(0, 0, identity, &lines, budget - 1),
        Err(EncodeError::TooLarge)
    );
    let decoder_refused = matches!(chunk.lines_with_budget(budget - 1), Err(ChunkFault::Frame));
    assert_eq!((encoder_refused, decoder_refused), (true, true));
}

/// The frame handed to a history importer has the same line count as its CRC-valid chunk head.
#[test]
fn history_frames_keep_the_chunk_s_declared_line_count() {
    let source = [Line::from("first"), Line::from("second")];
    let identity = HistoryIdentity {
        cols: 24,
        ..HistoryIdentity::default()
    };
    for count in 0..=source.len() {
        let record = encode_history_chunk(7, 20, identity, &source[..count])
            .expect("ordinary history chunk");
        let body = one_step(&record, 7).expect("the public encoder's CRC is valid");
        let chunk = decode_history_chunk(7, body).expect("ordinary head");
        let mut expected = Vec::new();
        put_blob(&mut expected, &serialize_lines(&source[..count])).expect("frame");
        assert_eq!(chunk.frame(), Ok(expected));
        assert_eq!(chunk.lines().expect("ordinary strict decoder").len(), count);
    }
    eprintln!("HISTORY_FRAME_DECLARED_COUNT_ORDINARY_CONTROLS_REACHED");

    let record = encode_history_chunk(7, 20, identity, &source).expect("two lines");
    let body = one_step(&record, 7).expect("ordinary CRC");
    let mut refused = Vec::new();
    for declared in [1u32, 3] {
        let mut lying_body = body.to_vec();
        lying_body[4..8].copy_from_slice(&declared.to_le_bytes());
        let lying_record = encode_step(7, &lying_body).expect("fresh CRC");
        let checked_body = one_step(&lying_record, 7).expect("the changed CRC is valid");
        let lying = decode_history_chunk(7, checked_body).expect("a valid chunk head");
        assert_eq!(lying.n_lines, declared);
        assert!(
            lying.frame_size().is_ok(),
            "the small frame fits either byte bound"
        );
        assert!(
            matches!(lying.lines(), Err(ChunkFault::Frame)),
            "strict count control"
        );
        refused.push(matches!(lying.frame(), Err(ChunkFault::Frame)));
    }
    eprintln!("HISTORY_FRAME_DECLARED_COUNT_MISMATCH_CASES_REACHED");
    assert!(
        refused.iter().all(|&refused| refused),
        "HISTORY_FRAME_DECLARED_COUNT_MISMATCH: {refused:?}"
    );
}

/// Computes CRC-32/IEEE (reflected, polynomial `0xEDB8_8320`) without astream code.
fn crc32_by_hand(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Where a v1 step record's body starts: the 12-byte frame header, then the 13-byte step
/// header.
const V1_STEP_BODY_AT: usize = 25;

/// Frames a layout v1 step record around `body` at `step` by hand.
fn v1_step_record(step: u64, body: &[u8]) -> Vec<u8> {
    let body_len = u32::try_from(body.len()).expect("a test body");
    let mut payload = vec![1];
    payload.extend_from_slice(&step.to_le_bytes());
    payload.extend_from_slice(&body_len.to_le_bytes());
    payload.extend_from_slice(body);
    let payload_len = u32::try_from(payload.len()).expect("a test payload");
    let mut record = vec![0xA5, 0x71, 1, 0];
    record.extend_from_slice(&payload_len.to_le_bytes());
    record.extend_from_slice(&crc32_by_hand(&payload).to_le_bytes());
    record.extend_from_slice(&payload);
    record
}

/// Mismatches found by [`layout_v1_holds_astream_step_records_byte_for_byte`] in each direction.
#[derive(Debug, Default, PartialEq, Eq)]
struct V1Directions {
    /// This build's encoders against layout v1's bytes.
    write: Vec<String>,
    /// This build's readers against hand-framed layout v1 records.
    read: Vec<String>,
}

impl V1Directions {
    /// A row when `encoded`'s frame and step headers differ from those of `hand`, the
    /// same body framed by hand.
    fn write_row(&mut self, what: &str, encoded: &[u8], hand: &[u8]) {
        let (encoded, hand) = (encoded.get(..V1_STEP_BODY_AT), hand.get(..V1_STEP_BODY_AT));
        if encoded != hand {
            self.write.push(format!(
                "{what}: framed {encoded:02X?}, layout v1 is {hand:02X?}"
            ));
        }
    }
}

/// Verifies that layout v1 step records match the vendored astream format byte for byte.
#[test]
fn layout_v1_holds_astream_step_records_byte_for_byte() {
    assert_eq!(crc32_by_hand(b"123456789"), 0xCBF4_3926);
    let literal: [u8; 27] = [
        0xA5, 0x71, 0x01, 0x00, 0x0F, 0x00, 0x00, 0x00, 0x69, 0x23, 0xB7, 0x6A, 0x01, 0x01, 0x02,
        0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x02, 0x00, 0x00, 0x00, b'a', b'b',
    ];
    assert_eq!(v1_step_record(0x0807_0605_0403_0201, b"ab"), literal);

    let mut found = V1Directions::default();

    let t = shell_with_history(6, 32, 40);
    let seq = 0x0102_0304_0506_0708;
    let head = head_for(&t, seq, 8);
    let checkpoint = t.checkpoint_journal(8).expect("ground");
    let keyframe = encode_keyframe(&head, &checkpoint).expect("encodes");
    let compressed = keyframe.get(V1_STEP_BODY_AT..).expect("a framed keyframe");
    let hand_keyframe = v1_step_record(seq, compressed);
    found.write_row("keyframe", &keyframe, &hand_keyframe);

    let fence = t.history_fence();
    let lines = t
        .history_lines_since_fence(&fence, fence.oldest, CHUNK_LINES)
        .expect("the fence holds");
    let (older, newer) = lines.split_at(lines.len() / 2);
    assert!(
        !older.is_empty() && !newer.is_empty(),
        "two chunks of history"
    );
    let identity = HistoryIdentity::of(&fence);
    let first_seq = 0x0A0B_0C0D_0E0F_1011;
    let mut hand_run = Vec::new();
    for (step, first_row, part) in [
        (first_seq, fence.oldest, older),
        (first_seq + 1, fence.oldest + older.len() as u64, newer),
    ] {
        let chunk = encode_history_chunk(step, first_row, identity, part).expect("encodes");
        let body = chunk.get(V1_STEP_BODY_AT..).expect("a framed chunk");
        let hand = v1_step_record(step, body);
        found.write_row(&format!("history chunk {step:#x}"), &chunk, &hand);
        hand_run.extend_from_slice(&hand);
    }

    match decode_keyframe(&hand_keyframe, seq) {
        Ok(kf) if kf.head == head && kf.checkpoint == checkpoint => {}
        Ok(_) => found
            .read
            .push("keyframe: decode_keyframe restored another screen".to_owned()),
        Err(fault) => found
            .read
            .push(format!("keyframe: decode_keyframe refused it: {fault:?}")),
    }

    let scan = scan_steps_from(&hand_run, first_seq);
    if scan.end != StepEnd::Clean || scan.valid_len != hand_run.len() || scan.steps.len() != 2 {
        found.read.push(format!(
            "history: the ring's step scan ended {:?} after {} of {} bytes, {} of 2 chunks accepted",
            scan.end,
            scan.valid_len,
            hand_run.len(),
            scan.steps.len()
        ));
    }
    let mut read = Vec::new();
    for (step, range) in &scan.steps {
        let body = hand_run.get(range.clone()).expect("the scan's range");
        match decode_history_chunk(*step, body) {
            Ok(chunk) if chunk.identity != identity => found.read.push(format!(
                "history chunk {step:#x}: another history's identity"
            )),
            Ok(chunk) => match chunk.lines() {
                Ok(part) => read.extend(part),
                Err(fault) => found
                    .read
                    .push(format!("history chunk {step:#x}: its lines: {fault:?}")),
            },
            Err(fault) => found
                .read
                .push(format!("history chunk {step:#x}: refused: {fault:?}")),
        }
    }
    if found.read.is_empty() && serialize_lines(&read) != serialize_lines(&lines) {
        found
            .read
            .push("history: the chunks read back other lines".to_owned());
    }

    assert_eq!(
        found,
        V1Directions::default(),
        "the vendored astream moved layout v1's step records: each row is a record that \
         broke that direction, and vendor/README-astream.md says what the bump owes each"
    );
}

/// A control record (CTR v1) round-trips under its own step and enforces [`MAX_CONTROL_CARRY`].
#[test]
fn a_control_record_round_trips_and_its_bounds_are_held() {
    let record = encode_control(3, b"the ledger").expect("encodes");
    assert_eq!(decode_control(&record, 3), Ok(b"the ledger".to_vec()));
    assert!(matches!(
        decode_control(&record, 4),
        Err(ControlFault::Step(_))
    ));
    let mut two = record.clone();
    two.extend_from_slice(&encode_control(4, b"more").expect("encodes"));
    assert!(matches!(
        decode_control(&two, 3),
        Err(ControlFault::Step(_))
    ));

    let mut past = vec![CTR_VERSION];
    past.extend_from_slice(
        &u32::try_from(MAX_CONTROL_CARRY + 1)
            .expect("u32")
            .to_le_bytes(),
    );
    past.extend_from_slice(&[0u8; 16]);
    let past = encode_step(5, &past).expect("a frame");
    assert_eq!(decode_control(&past, 5), Err(ControlFault::Compressed));

    let mut newer = vec![CTR_VERSION + 1];
    newer.extend_from_slice(&aterm_lz4::compress_prepend_size(b"x"));
    let newer = encode_step(6, &newer).expect("a frame");
    assert_eq!(
        decode_control(&newer, 6),
        Err(ControlFault::Version(CTR_VERSION + 1))
    );
    let empty = encode_step(7, &[]).expect("a frame");
    assert!(matches!(
        decode_control(&empty, 7),
        Err(ControlFault::Step(_))
    ));

    assert_eq!(
        encode_control(1, &vec![0u8; MAX_CONTROL_CARRY + 1]),
        Err(EncodeError::TooLarge)
    );
    let at_bound = encode_control(1, &vec![b'{'; MAX_CONTROL_CARRY]).expect("at the bound");
    assert_eq!(
        decode_control(&at_bound, 1).map(|carry| carry.len()),
        Ok(MAX_CONTROL_CARRY)
    );
}
